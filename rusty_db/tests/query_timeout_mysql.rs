#![cfg(feature = "mysql")]

//! Same coverage as `query_timeout.rs` (SQLite), against a real
//! MySQL/MariaDB server. MySQL/MariaDB has a built-in `SLEEP()`, so these
//! use that directly for a genuinely slow (not simulated) query to
//! cancel, rather than the lock-contention trick the SQLite version needs
//! (SQLite has no server-side sleep function).

use std::time::Duration;

use rusty_db::{with_timeout, Engine, Error, Result};

/// Connects to a real MySQL/MariaDB server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `MYSQL_TEST_URL` at a scratch database or the
/// test skips itself instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match rusty_db::mysql::MySqlDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL test: could not connect to {url}: {err}");
            None
        }
    }
}

#[tokio::test]
async fn an_operation_finishing_within_the_timeout_succeeds_normally() -> Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    let rows = with_timeout(
        Duration::from_secs(5),
        engine.connect().await?.fetch_all("SELECT SLEEP(0.1)", &[]),
    )
    .await?;
    assert_eq!(rows.len(), 1);

    Ok(())
}

#[tokio::test]
async fn a_slow_query_is_cancelled_by_its_timeout() -> Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    let outcome = with_timeout(
        Duration::from_millis(300),
        engine.connect().await?.fetch_all("SELECT SLEEP(3)", &[]),
    )
    .await;
    assert!(
        matches!(outcome, Err(Error::Timeout(_))),
        "expected the slow query to time out, got {outcome:?}"
    );

    Ok(())
}

#[tokio::test]
async fn cancelling_a_slow_query_leaves_the_pool_usable() -> Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    let outcome = with_timeout(
        Duration::from_millis(300),
        engine.connect().await?.fetch_all("SELECT SLEEP(3)", &[]),
    )
    .await;
    assert!(matches!(outcome, Err(Error::Timeout(_))));

    // The pool recovers: a fresh query right after still succeeds quickly.
    let rows = with_timeout(
        Duration::from_secs(5),
        engine.connect().await?.fetch_all("SELECT 1", &[]),
    )
    .await?;
    assert_eq!(rows.len(), 1);

    Ok(())
}

#[tokio::test]
async fn aborting_the_task_running_a_slow_query_also_cancels_it() -> Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    let handle = tokio::spawn(async move {
        engine
            .connect()
            .await?
            .fetch_all("SELECT SLEEP(5)", &[])
            .await
    });

    tokio::time::sleep(Duration::from_millis(200)).await;
    handle.abort();
    let result = handle.await;
    assert!(result.unwrap_err().is_cancelled());

    Ok(())
}
