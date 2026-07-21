#![cfg(feature = "mysql")]

//! Exercises `Engine::begin_two_phase`/`Transaction::prepare`/
//! `commit_prepared`/`rollback_prepared` against a real MySQL/MariaDB
//! server, using `XA START`/`XA END`/`XA PREPARE`/`XA COMMIT`/
//! `XA ROLLBACK` under the hood. Unlike Postgres, MySQL's `XA` support
//! doesn't need any special server configuration, so there's no
//! skip-if-disabled path here — just skip-if-unreachable, like every
//! other live-server test in this suite.
//!
//! DDL isn't allowed inside a MySQL `XA` transaction (it causes an
//! implicit commit, which conflicts with the explicit two-phase protocol),
//! so the tables below are created before `begin_two_phase`, not inside
//! the transaction the way an ordinary write might be.

use rusty_db::prelude::*;

/// Connects to a real MySQL/MariaDB server for this test. There's no
/// portable way to spin one up for every environment this suite runs in,
/// so this is opt-in: point `MYSQL_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match MySqlDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL test: could not connect to {url}: {err}");
            None
        }
    }
}

#[tokio::test]
async fn commit_prepared_makes_a_two_phase_committed_write_visible() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS two_phase_commit_mysql_commit", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE two_phase_commit_mysql_commit (id BIGINT PRIMARY KEY)",
            &[],
        )
        .await?;

    let mut txn = engine
        .begin_two_phase("rusty_db_test_mysql_commit_gid")
        .await?;
    txn.execute("INSERT INTO two_phase_commit_mysql_commit VALUES (1)", &[])
        .await?;
    txn.prepare(engine.dialect()).await?;

    // Not yet visible: prepared, but not yet committed.
    let table = Table::new("two_phase_commit_mysql_commit");
    let before = engine.fetch_all(&Select::from(&table)).await?;
    assert_eq!(
        before.len(),
        0,
        "a prepared-but-uncommitted write shouldn't be visible yet"
    );

    // Simulates a coordinator finalizing on a separate connection: this
    // call has no `Transaction` handle at all, just the gid.
    engine
        .commit_prepared("rusty_db_test_mysql_commit_gid")
        .await?;

    let after = engine.fetch_all(&Select::from(&table)).await?;
    assert_eq!(after.len(), 1, "commit_prepared should finalize the write");

    engine
        .connect()
        .await?
        .execute("DROP TABLE two_phase_commit_mysql_commit", &[])
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
        .execute("DROP TABLE IF EXISTS two_phase_commit_mysql_rollback", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE two_phase_commit_mysql_rollback (id BIGINT PRIMARY KEY)",
            &[],
        )
        .await?;

    let mut txn = engine
        .begin_two_phase("rusty_db_test_mysql_rollback_gid")
        .await?;
    txn.execute(
        "INSERT INTO two_phase_commit_mysql_rollback VALUES (1)",
        &[],
    )
    .await?;
    txn.prepare(engine.dialect()).await?;

    engine
        .rollback_prepared("rusty_db_test_mysql_rollback_gid")
        .await?;

    let table = Table::new("two_phase_commit_mysql_rollback");
    let rows = engine.fetch_all(&Select::from(&table)).await?;
    assert_eq!(rows.len(), 0, "rollback_prepared should discard the write");

    engine
        .connect()
        .await?
        .execute("DROP TABLE two_phase_commit_mysql_rollback", &[])
        .await?;
    Ok(())
}
