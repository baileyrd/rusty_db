#![cfg(all(feature = "postgres", feature = "derive"))]

//! Same coverage as `backup_restore.rs` (SQLite), against a real Postgres
//! server.
//!
//! Unlike the SQLite version (a fresh, isolated file-backed database per
//! test), this shares one live server with every other Postgres test in
//! the suite. `Engine::backup`/`restore` operating on *every* table would
//! risk a `restore` wiping data a concurrently-running test still needs,
//! so these use `backup_tables` to scope everything to just this test's
//! own table instead of the whole-database `backup()`. Each test also
//! uses its own table (`#[derive(Mapped)]`'s table name is a compile-time
//! constant, so distinct tests need distinct structs to avoid colliding —
//! including racing on the same `CREATE TABLE`/`DROP TABLE` — when cargo
//! runs them concurrently against the same server).

use rusty_db::prelude::*;

async fn recreate_table(engine: &Engine, table: &str) -> rusty_db::Result<()> {
    engine
        .connect()
        .await?
        .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            &format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, name TEXT NOT NULL)"),
            &[],
        )
        .await?;
    Ok(())
}

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

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "backup_restore_round_trip_users")]
struct RoundTripUser {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn restore_returns_the_table_to_its_backed_up_state() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "backup_restore_round_trip_users").await?;

    engine
        .execute(
            &Insert::into_table(&RoundTripUser::table())
                .value("id", 1_i64)
                .value("name", "ada"),
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&RoundTripUser::table())
                .value("id", 2_i64)
                .value("name", "grace"),
        )
        .await?;

    let dump = engine
        .backup_tables(&["backup_restore_round_trip_users"])
        .await?;

    engine
        .execute(
            &Delete::from(&RoundTripUser::table())
                .filter(RoundTripUser::table().col("id").eq(1_i64)),
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&RoundTripUser::table())
                .value("id", 3_i64)
                .value("name", "linus"),
        )
        .await?;

    engine.restore(&dump).await?;

    let mut rows: Vec<RoundTripUser> = engine
        .fetch_all_as(&Select::from(&RoundTripUser::table()))
        .await?;
    rows.sort_by_key(|u| u.id);
    assert_eq!(
        rows,
        vec![
            RoundTripUser {
                id: 1,
                name: "ada".to_string()
            },
            RoundTripUser {
                id: 2,
                name: "grace".to_string()
            },
        ]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE backup_restore_round_trip_users", &[])
        .await?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "backup_restore_failed_restore_users")]
struct FailedRestoreUser {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn a_failing_restore_rolls_back_completely() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "backup_restore_failed_restore_users").await?;

    engine
        .execute(
            &Insert::into_table(&FailedRestoreUser::table())
                .value("id", 1_i64)
                .value("name", "ada"),
        )
        .await?;
    let dump = engine
        .backup_tables(&["backup_restore_failed_restore_users"])
        .await?;

    let mut broken_dump = dump.clone();
    broken_dump.tables[0]
        .rows
        .push(vec![1_i64.into(), "duplicate".into()]);

    let outcome = engine.restore(&broken_dump).await;
    assert!(outcome.is_err(), "expected the restore to fail");

    let rows: Vec<FailedRestoreUser> = engine
        .fetch_all_as(&Select::from(&FailedRestoreUser::table()))
        .await?;
    assert_eq!(
        rows,
        vec![FailedRestoreUser {
            id: 1,
            name: "ada".to_string()
        }]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE backup_restore_failed_restore_users", &[])
        .await?;

    Ok(())
}
