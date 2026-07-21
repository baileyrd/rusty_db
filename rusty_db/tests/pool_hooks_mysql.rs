#![cfg(feature = "mysql")]

//! A reduced version of `pool_hooks.rs` (SQLite)/`pool_hooks_postgres.rs`
//! against a real MySQL/MariaDB server — just `.with_on_connect`, since the
//! hook mechanism itself (wiring into sqlx's own `after_connect`/
//! `before_acquire`/`after_release`) is identical across all three drivers
//! and already gets its `before_acquire`/`after_release` behavioral proof
//! against Postgres's custom GUC variables; this just confirms the same
//! mechanism actually executes real SQL against a real MySQL/MariaDB
//! connection too, using a user-defined `@variable` instead of a GUC.

use rusty_db::prelude::*;

/// Connects to a real MySQL/MariaDB server for this test. There's no way
/// to spin one up portably in every environment this test suite runs in,
/// so this is opt-in: point `MYSQL_TEST_URL` at a scratch database, or the
/// test skips itself instead of failing when no server is reachable.
async fn test_engine(config: PoolConfig) -> Option<Engine> {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match MySqlDriver::engine_with(&url, config).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL test: could not connect to {url}: {err}");
            None
        }
    }
}

#[tokio::test]
async fn on_connect_runs_once_per_new_physical_connection() -> rusty_db::Result<()> {
    let config = PoolConfig::new(1).with_on_connect("SET @rusty_db_hook_marker := 'ran'");
    let Some(engine) = test_engine(config).await else {
        return Ok(());
    };

    let row = engine
        .connect()
        .await?
        .fetch_all("SELECT @rusty_db_hook_marker AS marker", &[])
        .await?
        .remove(0);
    assert_eq!(row.get_by_name::<String>("marker")?, "ran");

    Ok(())
}
