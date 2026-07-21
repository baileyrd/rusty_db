#![cfg(feature = "sqlite")]

//! Exercises `PoolConfig::with_statement_cache_capacity`: each connection
//! caches up to that many distinct prepared statements (LRU-evicted past
//! it) instead of the underlying driver's default of 100. `Connection::
//! cached_statement_count()` (a thin pass-through to sqlx's own per-
//! connection `cached_statements_size()`) makes this directly observable,
//! rather than only inferable from timing.

use rusty_db::prelude::*;

/// A file-backed database (not `:memory:`, whose pool is forced to a
/// single connection regardless of `max_connections`) with an explicit
/// `PoolConfig`, constrained to one connection so every query in a test
/// definitely lands on the same physical connection.
async fn file_engine(name: &str, config: PoolConfig) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_pool_statement_cache_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    SqliteDriver::engine_with(&url, config).await
}

/// Five genuinely distinct query shapes (differing in structure, not just
/// bound parameter values), so each needs its own cache entry.
const DISTINCT_QUERIES: [&str; 5] = [
    "SELECT 1",
    "SELECT 2, 3",
    "SELECT 'a'",
    "SELECT 'b', 'c'",
    "SELECT 4, 5, 6",
];

#[tokio::test]
async fn without_a_capacity_every_distinct_shape_stays_cached() -> rusty_db::Result<()> {
    let engine = file_engine("default_capacity", PoolConfig::new(1)).await?;
    let mut conn = engine.connect().await?;
    for sql in DISTINCT_QUERIES {
        conn.fetch_all(sql, &[]).await?;
    }
    assert_eq!(
        conn.cached_statement_count(),
        DISTINCT_QUERIES.len(),
        "well under the default capacity of 100, so nothing should be evicted"
    );

    Ok(())
}

#[tokio::test]
async fn with_statement_cache_capacity_caps_the_cache_via_lru_eviction() -> rusty_db::Result<()> {
    let engine = file_engine(
        "small_capacity",
        PoolConfig::new(1).with_statement_cache_capacity(2),
    )
    .await?;
    let mut conn = engine.connect().await?;
    for sql in DISTINCT_QUERIES {
        conn.fetch_all(sql, &[]).await?;
    }
    assert_eq!(
        conn.cached_statement_count(),
        2,
        "capacity 2 should LRU-evict everything but the 2 most recently used shapes"
    );

    Ok(())
}
