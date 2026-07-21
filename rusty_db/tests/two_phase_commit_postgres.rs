#![cfg(feature = "postgres")]

//! Exercises `Engine::begin_two_phase`/`Transaction::prepare`/
//! `commit_prepared`/`rollback_prepared` against a real Postgres server:
//! `PREPARE TRANSACTION`/`COMMIT PREPARED`/`ROLLBACK PREPARED`. Requires
//! `max_prepared_transactions > 0` on the server (it's `0`, i.e. disabled,
//! by default) — if it's still `0`, `PREPARE TRANSACTION` itself fails,
//! which these tests treat as "can't verify this here" and skip, the same
//! way an unreachable server is skipped rather than failed.

use rusty_db::prelude::*;

/// Connects to a real PostgreSQL server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `POSTGRES_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| "postgres://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match PostgresDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping Postgres test: could not connect to {url}: {err}");
            None
        }
    }
}

/// `PREPARE TRANSACTION` fails outright if the server has
/// `max_prepared_transactions = 0` (the shipped default) — there's no
/// portable way to change that from a test, so treat it like an
/// unreachable server: skip rather than fail.
fn is_prepared_transactions_disabled(err: &rusty_db::Error) -> bool {
    err.to_string()
        .to_lowercase()
        .contains("prepared transaction")
}

#[tokio::test]
async fn commit_prepared_makes_a_two_phase_committed_write_visible() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS two_phase_commit_pg_commit", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE two_phase_commit_pg_commit (id BIGINT PRIMARY KEY)",
            &[],
        )
        .await?;

    let mut txn = match engine.begin_two_phase("rusty_db_test_pg_commit_gid").await {
        Ok(txn) => txn,
        Err(err) if is_prepared_transactions_disabled(&err) => return Ok(()),
        Err(err) => return Err(err),
    };
    txn.execute("INSERT INTO two_phase_commit_pg_commit VALUES (1)", &[])
        .await?;
    match txn.prepare(engine.dialect()).await {
        Ok(()) => {}
        Err(err) if is_prepared_transactions_disabled(&err) => return Ok(()),
        Err(err) => return Err(err),
    }

    // Not yet visible: prepared, but not yet committed.
    let table = Table::new("two_phase_commit_pg_commit");
    let before = engine.fetch_all(&Select::from(&table)).await?;
    assert_eq!(
        before.len(),
        0,
        "a prepared-but-uncommitted write shouldn't be visible yet"
    );

    // Simulates a coordinator finalizing on a separate connection: this
    // call has no `Transaction` handle at all, just the gid.
    engine
        .commit_prepared("rusty_db_test_pg_commit_gid")
        .await?;

    let after = engine.fetch_all(&Select::from(&table)).await?;
    assert_eq!(after.len(), 1, "commit_prepared should finalize the write");

    engine
        .connect()
        .await?
        .execute("DROP TABLE two_phase_commit_pg_commit", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn rollback_prepared_discards_a_two_phase_prepared_write() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS two_phase_commit_pg_rollback", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE two_phase_commit_pg_rollback (id BIGINT PRIMARY KEY)",
            &[],
        )
        .await?;

    let mut txn = match engine
        .begin_two_phase("rusty_db_test_pg_rollback_gid")
        .await
    {
        Ok(txn) => txn,
        Err(err) if is_prepared_transactions_disabled(&err) => return Ok(()),
        Err(err) => return Err(err),
    };
    txn.execute("INSERT INTO two_phase_commit_pg_rollback VALUES (1)", &[])
        .await?;
    match txn.prepare(engine.dialect()).await {
        Ok(()) => {}
        Err(err) if is_prepared_transactions_disabled(&err) => return Ok(()),
        Err(err) => return Err(err),
    }

    engine
        .rollback_prepared("rusty_db_test_pg_rollback_gid")
        .await?;

    let table = Table::new("two_phase_commit_pg_rollback");
    let rows = engine.fetch_all(&Select::from(&table)).await?;
    assert_eq!(rows.len(), 0, "rollback_prepared should discard the write");

    engine
        .connect()
        .await?
        .execute("DROP TABLE two_phase_commit_pg_rollback", &[])
        .await?;
    Ok(())
}
