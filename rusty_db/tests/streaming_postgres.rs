#![cfg(feature = "postgres")]

//! `Engine::fetch_stream` against a real Postgres server — confirming
//! genuine row-at-a-time streaming (not materializing the whole result set
//! before returning the stream) actually holds up over the network, not
//! just against SQLite's in-process driver (see `streaming.rs`).

use std::time::Duration;

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
async fn fetch_stream_holds_the_connection_checked_out_across_partial_consumption(
) -> rusty_db::Result<()> {
    // A pool of size 1: a genuinely row-at-a-time stream must still show
    // the connection as `in_use` partway through iteration, only dropping
    // back to idle once the stream itself is dropped -- a
    // materialize-then-stream fallback would have already released the
    // connection before the first item was even available.
    let Some(engine) = test_engine(PoolConfig::new(1)).await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS streaming_pg_orders", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE streaming_pg_orders (id BIGINT PRIMARY KEY, amount BIGINT NOT NULL)",
            &[],
        )
        .await?;

    let orders = Table::new("streaming_pg_orders");
    for (id, amount) in [(1_i64, 10_i64), (2, 50), (3, 200)] {
        engine
            .execute(
                &Insert::into_table(&orders)
                    .value("id", id)
                    .value("amount", amount),
            )
            .await?;
    }

    let query = Select::from(&orders).order_by(orders.col("id").asc());
    let mut stream = engine.fetch_stream(&query).await?;
    assert_eq!(
        engine.pool_stats().in_use,
        1,
        "starting the stream should already have checked out the connection"
    );

    let first = stream.next().await;
    assert!(first.is_some());
    assert_eq!(
        first.unwrap()?.get_by_name::<i64>("id")?,
        1,
        "rows should still arrive in the requested order"
    );
    assert_eq!(
        engine.pool_stats().in_use,
        1,
        "still only partway through the stream, so the connection should still be held"
    );

    drop(stream);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        engine.pool_stats().in_use,
        0,
        "dropping the stream (even mid-iteration) should release the connection back"
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE streaming_pg_orders", &[])
        .await?;
    Ok(())
}
