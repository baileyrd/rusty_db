#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Engine::fetch_stream`/`fetch_stream_as`: a cursor/`Stream`-
//! based fetch instead of always collecting a full `Vec<Row>` first — the
//! escape hatch for large exports/reports where materializing everything
//! up front would be a real memory ceiling.

use std::time::Duration;

use rusty_db::prelude::*;

/// A file-backed database (not `:memory:`, whose pool is forced to a
/// single connection regardless of `max_connections`) with an explicit
/// `PoolConfig`.
async fn file_engine(name: &str, config: PoolConfig) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_streaming_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine_with(&url, config).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER NOT NULL)",
            &[],
        )
        .await?;
    let orders = Table::new("orders");
    for (id, amount) in [(1_i64, 10_i64), (2, 50), (3, 200)] {
        engine
            .execute(
                &Insert::into_table(&orders)
                    .value("id", id)
                    .value("amount", amount),
            )
            .await?;
    }
    Ok(engine)
}

#[derive(Mapped, Debug, PartialEq)]
#[table(name = "orders")]
struct Order {
    #[table(primary_key)]
    id: i64,
    amount: i64,
}

#[tokio::test]
async fn fetch_stream_yields_the_same_rows_fetch_all_would_in_the_same_order(
) -> rusty_db::Result<()> {
    let engine = file_engine("order", PoolConfig::new(2)).await?;
    let orders = Table::new("orders");
    let query = Select::from(&orders).order_by(orders.col("id").asc());

    let expected = engine.fetch_all(&query).await?;

    let mut stream = engine.fetch_stream(&query).await?;
    let mut streamed = Vec::new();
    while let Some(row) = stream.next().await {
        streamed.push(row?);
    }

    assert_eq!(streamed.len(), expected.len());
    for (streamed_row, expected_row) in streamed.iter().zip(expected.iter()) {
        assert_eq!(
            streamed_row.get_by_name::<i64>("id")?,
            expected_row.get_by_name::<i64>("id")?
        );
        assert_eq!(
            streamed_row.get_by_name::<i64>("amount")?,
            expected_row.get_by_name::<i64>("amount")?
        );
    }

    Ok(())
}

#[tokio::test]
async fn fetch_stream_as_decodes_into_a_mapped_type() -> rusty_db::Result<()> {
    let engine = file_engine("decode", PoolConfig::new(2)).await?;
    let orders = Table::new("orders");
    let query = Select::from(&orders).order_by(orders.col("id").asc());

    let mut stream = engine.fetch_stream_as::<Order>(&query).await?;
    let mut streamed = Vec::new();
    while let Some(order) = stream.next().await {
        streamed.push(order?);
    }

    assert_eq!(
        streamed,
        vec![
            Order { id: 1, amount: 10 },
            Order { id: 2, amount: 50 },
            Order { id: 3, amount: 200 },
        ]
    );

    Ok(())
}

#[tokio::test]
async fn fetch_stream_holds_the_connection_checked_out_across_partial_consumption(
) -> rusty_db::Result<()> {
    // A pool of size 1: if `fetch_stream` genuinely streams row-by-row
    // (rather than materializing everything up front and only then
    // handing back a stream, which would release the connection before
    // this function even got the first item), the connection it checked
    // out must still show as `in_use` while only partway through
    // consuming the stream — and drop back to idle once the stream itself
    // is dropped, not before.
    let engine = file_engine("holds_connection", PoolConfig::new(1)).await?;
    let orders = Table::new("orders");
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
        engine.pool_stats().in_use,
        1,
        "still only partway through the stream, so the connection should still be held"
    );

    drop(stream);
    // Releasing a connection back to the pool isn't synchronous inside
    // `drop`, the same not-quite-synchronous cleanup this suite works
    // around elsewhere (e.g. `pool_stats.rs`).
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        engine.pool_stats().in_use,
        0,
        "dropping the stream (even mid-iteration) should release the connection back"
    );

    Ok(())
}
