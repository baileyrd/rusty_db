#![cfg(all(feature = "postgres", feature = "derive"))]

//! Same coverage as `pool_exhaustion.rs` (SQLite), against a real Postgres
//! server: `PoolConfig` lets these constrain `max_connections` (and
//! `acquire_timeout`) so exhaustion is triggered deterministically rather
//! than by guessing how much concurrent load a default-sized pool can
//! absorb.

use std::time::Duration;

use rusty_db::prelude::*;
use tokio::task::{spawn_local, LocalSet};
use tokio::time::timeout;

/// Connects to a real PostgreSQL server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `POSTGRES_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
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

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "pool_exhaustion_users_serialize")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn acquiring_beyond_max_connections_blocks_then_succeeds_after_release(
) -> rusty_db::Result<()> {
    let Some(engine) = test_engine(PoolConfig::new(1)).await else {
        return Ok(());
    };
    recreate_table(&engine, "pool_exhaustion_users").await?;

    let held = engine.connect().await?;

    let quick_attempt = timeout(Duration::from_millis(200), engine.connect()).await;
    assert!(
        quick_attempt.is_err(),
        "expected connect() to still be waiting for the held connection"
    );

    drop(held);

    let after_release = timeout(Duration::from_secs(5), engine.connect()).await;
    assert!(
        after_release.is_ok(),
        "connect() should have succeeded once the held connection was released"
    );
    after_release.unwrap()?;

    engine
        .connect()
        .await?
        .execute("DROP TABLE pool_exhaustion_users", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn acquire_timeout_errors_instead_of_hanging_forever() -> rusty_db::Result<()> {
    let config = PoolConfig::new(1).with_acquire_timeout(Duration::from_millis(200));
    let Some(engine) = test_engine(config).await else {
        return Ok(());
    };
    recreate_table(&engine, "pool_exhaustion_users_timeout").await?;

    let held = engine.connect().await?;

    let start = std::time::Instant::now();
    let outcome = timeout(Duration::from_secs(5), engine.connect()).await;
    let elapsed = start.elapsed();

    let result = outcome.expect("engine.connect() hung well past its configured acquire_timeout");
    assert!(
        result.is_err(),
        "expected the pool's own acquire_timeout to produce an error"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "acquire_timeout should have fired quickly, took {elapsed:?}"
    );

    drop(held);

    engine
        .connect()
        .await?
        .execute("DROP TABLE pool_exhaustion_users_timeout", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn sessions_serialize_correctly_when_the_pool_is_exhausted() -> rusty_db::Result<()> {
    const COUNT: i64 = 5;
    let Some(engine) = test_engine(PoolConfig::new(1)).await else {
        return Ok(());
    };
    recreate_table(&engine, "pool_exhaustion_users_serialize").await?;
    let local = LocalSet::new();

    local
        .run_until(async {
            let mut handles = Vec::with_capacity(COUNT as usize);
            for id in 0..COUNT {
                let engine = engine.clone();
                handles.push(spawn_local(async move {
                    let mut session = engine.session();
                    session.add(&User {
                        id,
                        name: format!("user-{id}"),
                    });
                    session.commit().await
                }));
            }
            for handle in handles {
                handle.await.unwrap()?;
            }
            rusty_db::Result::Ok(())
        })
        .await?;

    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(rows.len(), COUNT as usize);

    engine
        .connect()
        .await?
        .execute("DROP TABLE pool_exhaustion_users_serialize", &[])
        .await?;

    Ok(())
}
