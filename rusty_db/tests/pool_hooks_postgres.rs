#![cfg(feature = "postgres")]

//! `PoolConfig::with_on_connect`/`.with_before_acquire`/`.with_after_release`
//! against a real Postgres server. Postgres's arbitrary custom GUC
//! variables (`SET myapp.whatever = ...` / `current_setting('myapp.whatever',
//! true)`, no predeclaration needed) make it easy to prove each hook
//! actually ran on the exact physical connection handed back, not just that
//! the SQL string was accepted somewhere.

use rusty_db::prelude::*;

/// Connects to a real PostgreSQL server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `POSTGRES_TEST_URL` at a scratch database, or the
/// test skips itself instead of failing when no server is reachable.
async fn test_engine(config: PoolConfig) -> Option<Engine> {
    let url = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| "postgres://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match PostgresDriver::engine_with(&url, config).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping Postgres test: could not connect to {url}: {err}");
            None
        }
    }
}

#[tokio::test]
async fn on_connect_runs_once_per_new_physical_connection() -> rusty_db::Result<()> {
    let config = PoolConfig::new(1).with_on_connect("SET application_name = 'rusty_db_hook_test'");
    let Some(engine) = test_engine(config).await else {
        return Ok(());
    };

    let row = engine
        .connect()
        .await?
        .fetch_all("SHOW application_name", &[])
        .await?
        .remove(0);
    assert_eq!(
        row.get_by_name::<String>("application_name")?,
        "rusty_db_hook_test"
    );

    Ok(())
}

#[tokio::test]
async fn before_acquire_and_after_release_both_run_on_the_same_physical_connection(
) -> rusty_db::Result<()> {
    // A pool of size 1 guarantees the same physical connection is reused
    // across both acquires below, so anything `.with_after_release` sets
    // during the first release must still be visible on the second acquire.
    let config = PoolConfig::new(1)
        .with_before_acquire("SET myapp.on_acquire = 'yes'")
        .with_after_release("SET myapp.on_release = 'yes'");
    let Some(engine) = test_engine(config).await else {
        return Ok(());
    };

    let mut first = engine.connect().await?;
    let row = first
        .fetch_all(
            "SELECT current_setting('myapp.on_acquire', true) AS value",
            &[],
        )
        .await?
        .remove(0);
    assert_eq!(
        row.get_by_name::<String>("value")?,
        "yes",
        "before_acquire should have run before this connection was handed out"
    );
    drop(first);

    // Give the pool a brief moment to actually run `after_release` and put
    // the connection back to idle before the next acquire (releasing back
    // to the pool isn't synchronous inside `drop`).
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let mut second = engine.connect().await?;
    let row = second
        .fetch_all(
            "SELECT current_setting('myapp.on_release', true) AS value",
            &[],
        )
        .await?
        .remove(0);
    assert_eq!(
        row.get_by_name::<String>("value")?,
        "yes",
        "after_release should have run on this same physical connection before it was reacquired"
    );

    Ok(())
}
