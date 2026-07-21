#![cfg(feature = "sqlite")]

//! Exercises `Engine::pool_stats()`: a live snapshot of the underlying
//! connection pool (open/idle/in-use counts, waiters, and total
//! successful acquires), for monitoring saturation directly instead of
//! inferring it from `acquire_timeout` errors after the fact (see
//! `pool_exhaustion.rs` for that side of things).

use std::time::Duration;

use rusty_db::prelude::*;

/// A file-backed database (not `:memory:`, whose pool is forced to a
/// single connection regardless of `max_connections`) with an explicit
/// `PoolConfig`. No table is ever created — these tests only care about
/// connection checkout/release, not query results.
async fn file_engine(name: &str, config: PoolConfig) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_pool_stats_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    SqliteDriver::engine_with(&url, config).await
}

/// sqlx's pool opens (and validates) one connection up front when it's
/// first constructed, so a "fresh" pool already reports that connection
/// as active/idle — `total_acquires` stays 0 either way, since that
/// startup connection didn't go through `Driver::connect`/`Engine::connect`.
#[tokio::test]
async fn a_fresh_pool_reports_no_checkouts_yet() -> rusty_db::Result<()> {
    let engine = file_engine("fresh", PoolConfig::new(3)).await?;

    let stats = engine.pool_stats();
    assert_eq!(stats.max_connections, 3);
    assert_eq!(stats.in_use, 0);
    assert_eq!(stats.waiters, 0);
    assert_eq!(stats.total_acquires, 0);

    Ok(())
}

#[tokio::test]
async fn checking_out_and_releasing_a_connection_is_reflected_immediately() -> rusty_db::Result<()>
{
    let engine = file_engine("checkout_release", PoolConfig::new(2)).await?;

    let held = engine.connect().await?;
    let while_held = engine.pool_stats();
    assert_eq!(while_held.idle, 0);
    assert_eq!(while_held.in_use, 1);
    assert_eq!(while_held.total_acquires, 1);

    drop(held);
    // Returning a connection to the pool happens on the pool's own
    // background bookkeeping, not synchronously in `drop`, so give it a
    // moment before reading the snapshot back.
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
async fn total_acquires_counts_every_checkout_not_just_the_current_one() -> rusty_db::Result<()> {
    let engine = file_engine("total_acquires", PoolConfig::new(1)).await?;

    for _ in 0..3 {
        drop(engine.connect().await?);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let stats = engine.pool_stats();
    assert_eq!(stats.total_acquires, 3);
    assert_eq!(
        stats.in_use, 0,
        "every connection was released before the next checkout"
    );

    Ok(())
}

#[tokio::test]
async fn waiters_reflects_a_blocked_acquire_and_clears_once_unblocked() -> rusty_db::Result<()> {
    let engine = file_engine("waiters", PoolConfig::new(1)).await?;

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
