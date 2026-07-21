#![cfg(feature = "mysql")]

//! A reduced version of `pool_stats.rs` (SQLite) against a real
//! MySQL/MariaDB server — just the two tests that most directly prove
//! `pool_stats()` against a real network-backed pool (checkout/release and
//! blocked-acquire waiters); the pure counting behavior (`total_acquires`
//! accumulating across several checkouts, a fresh pool's zeroed counters)
//! is already covered there and doesn't need re-proving per driver.

use std::time::Duration;

use rusty_db::prelude::*;

/// Connects to a real MySQL/MariaDB server for this test. There's no way
/// to spin one up portably in every environment this test suite runs in,
/// so this is opt-in: point `MYSQL_TEST_URL` at a scratch database, or the
/// test skips itself instead of failing when no server is reachable. No
/// table is ever created — these tests only care about connection
/// checkout/release, not query results.
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
async fn checking_out_and_releasing_a_connection_is_reflected_immediately() -> rusty_db::Result<()>
{
    let Some(engine) = test_engine(PoolConfig::new(2)).await else {
        return Ok(());
    };

    let held = engine.connect().await?;
    let while_held = engine.pool_stats();
    assert_eq!(while_held.idle, 0);
    assert_eq!(while_held.in_use, 1);
    assert_eq!(while_held.total_acquires, 1);

    drop(held);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let after_release = engine.pool_stats();
    assert_eq!(
        after_release.active, 1,
        "the connection stays open, just idle"
    );
    assert_eq!(after_release.idle, 1);
    assert_eq!(after_release.in_use, 0);
    assert_eq!(
        after_release.total_acquires, 1,
        "releasing a connection isn't itself a new acquire"
    );

    Ok(())
}

#[tokio::test]
async fn waiters_reflects_a_blocked_acquire_and_clears_once_unblocked() -> rusty_db::Result<()> {
    let Some(engine) = test_engine(PoolConfig::new(1)).await else {
        return Ok(());
    };

    let held = engine.connect().await?;
    assert_eq!(engine.pool_stats().waiters, 0);

    let mut blocked = Box::pin(engine.connect());
    tokio::select! {
        _ = &mut blocked => panic!("connect() should still be blocked on the held connection"),
        _ = tokio::time::sleep(Duration::from_millis(150)) => {}
    }

    let stats_while_blocked = engine.pool_stats();
    assert_eq!(stats_while_blocked.waiters, 1);
    assert_eq!(
        stats_while_blocked.total_acquires, 1,
        "a still-blocked acquire hasn't succeeded yet"
    );

    drop(held);
    let unblocked = blocked.await?;

    let stats_after = engine.pool_stats();
    assert_eq!(stats_after.waiters, 0);
    assert_eq!(stats_after.total_acquires, 2);

    drop(unblocked);
    Ok(())
}
