#![cfg(feature = "sqlite")]

//! Exercises `PoolConfig::with_on_connect` (connection-level event hooks):
//! raw SQL run once on every newly-opened physical connection, before it's
//! ever handed to a caller. SQLite's `PRAGMA case_sensitive_like` is
//! per-connection (not persisted in the database file, and off by
//! default — `LIKE` is case-insensitive for ASCII unless a connection
//! turns this on), which makes it a genuinely behavioral way to prove the
//! hook actually ran — not just that some SQL string got recorded
//! somewhere. See `pool_hooks_postgres.rs` for `.with_before_acquire`/
//! `.with_after_release` (SQLite has no session-local GUC-style variable
//! to observe those two against the way Postgres does).

use rusty_db::prelude::*;

/// A file-backed database (not `:memory:`, whose pool is forced to a single
/// connection regardless of `max_connections`) with an explicit `PoolConfig`.
async fn file_engine(name: &str, config: PoolConfig) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_pool_hooks_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    SqliteDriver::engine_with(&url, config).await
}

async fn like_is_case_sensitive(engine: &Engine) -> rusty_db::Result<bool> {
    let row = engine
        .connect()
        .await?
        .fetch_all("SELECT 'ABC' LIKE 'abc' AS matched", &[])
        .await?
        .remove(0);
    Ok(row.get_by_name::<i64>("matched")? == 0)
}

#[tokio::test]
async fn without_on_connect_like_stays_case_insensitive() -> rusty_db::Result<()> {
    let engine = file_engine("baseline", PoolConfig::new(1)).await?;
    assert!(
        !like_is_case_sensitive(&engine).await?,
        "SQLite's LIKE is case-insensitive by default"
    );

    Ok(())
}

#[tokio::test]
async fn on_connect_runs_once_per_new_connection_and_actually_takes_effect() -> rusty_db::Result<()>
{
    let engine = file_engine(
        "on_connect",
        PoolConfig::new(1).with_on_connect("PRAGMA case_sensitive_like = ON"),
    )
    .await?;
    assert!(
        like_is_case_sensitive(&engine).await?,
        "on_connect's PRAGMA case_sensitive_like = ON should have taken effect"
    );

    Ok(())
}
