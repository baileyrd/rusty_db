#![cfg(feature = "postgres")]

//! `PoolConfig::with_statement_cache_capacity` against a real Postgres
//! server — the backend where a prepared statement genuinely costs a
//! server-side parse/plan round trip, making this the one place the
//! feature's actual point (`baked query`-equivalent reuse) has real
//! per-dialect weight, not just a portable LRU-counting exercise.

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

const DISTINCT_QUERIES: [&str; 5] = [
    "SELECT 1",
    "SELECT 2, 3",
    "SELECT 'a'",
    "SELECT 'b', 'c'",
    "SELECT 4, 5, 6",
];

#[tokio::test]
async fn with_statement_cache_capacity_caps_the_cache_via_lru_eviction() -> rusty_db::Result<()> {
    let Some(engine) = test_engine(PoolConfig::new(1).with_statement_cache_capacity(2)).await
    else {
        return Ok(());
    };

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
