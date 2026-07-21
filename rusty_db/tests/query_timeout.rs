#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `with_timeout`: cancelling a database operation that's
//! taking too long, rather than waiting on it indefinitely.
//!
//! To get a genuinely *blocked* (not just slow) query deterministically,
//! these hold SQLite's write lock open on one connection (`BEGIN
//! IMMEDIATE`) while a second connection attempts a conflicting write.
//! sqlx's SQLite driver retries internally against its own 5-second
//! `busy_timeout` rather than erroring immediately, so a client-side
//! timeout shorter than that genuinely has something in-flight to cancel
//! — this is real lock contention, not a simulated delay.

use std::time::Duration;

use rusty_db::prelude::*;
use rusty_db::{with_timeout, Error};
use tokio::task::{spawn_local, LocalSet};

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "counters")]
struct Counter {
    #[table(primary_key)]
    id: i64,
    value: i64,
}

/// A file-backed database (not `:memory:`) with room for two connections:
/// one to hold the write lock, one to attempt (and get blocked on) a
/// competing write.
async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_query_timeout_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine_with(&url, PoolConfig::new(2)).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE counters (id INTEGER PRIMARY KEY, value INTEGER NOT NULL)",
            &[],
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&Counter::table())
                .value("id", 1_i64)
                .value("value", 0_i64),
        )
        .await?;
    Ok(engine)
}

fn bump(value: i64) -> Update {
    let counter = Counter::table();
    Update::table(&counter)
        .set("value", value)
        .filter(counter.col("id").eq(1_i64))
}

async fn value_of(engine: &Engine) -> rusty_db::Result<i64> {
    let rows: Vec<Counter> = engine
        .fetch_all_as(&Select::from(&Counter::table()))
        .await?;
    Ok(rows.into_iter().next().unwrap().value)
}

#[tokio::test]
async fn an_operation_finishing_within_the_timeout_succeeds_normally() -> rusty_db::Result<()> {
    let engine = file_engine("fast").await?;

    let rows = with_timeout(
        Duration::from_secs(5),
        engine.fetch_all(&Select::from(&Counter::table())),
    )
    .await?;
    assert_eq!(rows.len(), 1);

    Ok(())
}

#[tokio::test]
async fn a_write_blocked_on_a_lock_is_cancelled_by_its_timeout() -> rusty_db::Result<()> {
    let engine = file_engine("blocked_write").await?;

    // Hold the write lock on one connection...
    let mut lock_holder = engine.connect().await?;
    lock_holder.execute("BEGIN IMMEDIATE", &[]).await?;

    // ...so a competing write on another connection blocks rather than
    // erroring immediately.
    let outcome = with_timeout(Duration::from_millis(300), engine.execute(&bump(1))).await;
    assert!(
        matches!(outcome, Err(Error::Timeout(_))),
        "expected the blocked write to time out, got {outcome:?}"
    );

    lock_holder.execute("ROLLBACK", &[]).await?;
    drop(lock_holder);

    // Cancelling the stuck operation didn't leave the engine unusable —
    // the pool recovers.
    assert_eq!(value_of(&engine).await?, 0);

    Ok(())
}

#[tokio::test]
async fn aborting_the_task_running_a_blocked_write_also_frees_the_pool() -> rusty_db::Result<()> {
    let engine = file_engine("aborted_task").await?;

    let mut lock_holder = engine.connect().await?;
    lock_holder.execute("BEGIN IMMEDIATE", &[]).await?;

    let local = LocalSet::new();
    local
        .run_until(async {
            let blocked_engine = engine.clone();
            let handle = spawn_local(async move { blocked_engine.execute(&bump(2)).await });

            // Give it a moment to actually start waiting on the lock, then
            // cancel it directly — `JoinHandle::abort` is the other
            // standard way (besides a timeout) to cancel a running async
            // task in Rust.
            tokio::time::sleep(Duration::from_millis(100)).await;
            handle.abort();
            let result = handle.await;
            assert!(result.unwrap_err().is_cancelled());
        })
        .await;

    lock_holder.execute("ROLLBACK", &[]).await?;
    drop(lock_holder);

    assert_eq!(value_of(&engine).await?, 0);

    Ok(())
}

#[tokio::test]
async fn a_timeout_on_one_call_does_not_affect_later_calls() -> rusty_db::Result<()> {
    let engine = file_engine("not_sticky").await?;

    let mut lock_holder = engine.connect().await?;
    lock_holder.execute("BEGIN IMMEDIATE", &[]).await?;
    let timed_out = with_timeout(Duration::from_millis(200), engine.execute(&bump(9))).await;
    assert!(matches!(timed_out, Err(Error::Timeout(_))));
    lock_holder.execute("ROLLBACK", &[]).await?;
    drop(lock_holder);

    // No lingering effect from the earlier timeout — a later call (with a
    // generous timeout of its own) just works.
    with_timeout(Duration::from_secs(5), engine.execute(&bump(42))).await?;
    assert_eq!(value_of(&engine).await?, 42);

    Ok(())
}
