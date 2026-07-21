#![cfg(all(feature = "mysql", feature = "derive"))]

//! Exercises `Value::Decimal`/`BigDecimal` against a real MySQL/MariaDB
//! server. MySQL/MariaDB sends `DECIMAL` as text on its own wire protocol
//! (unlike Postgres — see `decimal_value_postgres.rs`), so this covers
//! `BigDecimal` round-tripping through a mapped struct via `FromValue`'s
//! text-parsing fallback.

use std::str::FromStr;

use rusty_db::prelude::*;

/// Connects to a real MySQL/MariaDB server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `MYSQL_TEST_URL` at a scratch database (its schema
/// is created and dropped by this test) or the test skips itself instead of
/// failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match MySqlDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL test: could not connect to {url}: {err}");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "decimal_value_mysql_invoices")]
struct Invoice {
    #[table(primary_key)]
    id: i64,
    total: BigDecimal,
    discount: Option<BigDecimal>,
}

#[tokio::test]
async fn decimal_field_round_trips_through_mysql_decimal_storage() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS decimal_value_mysql_invoices", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE decimal_value_mysql_invoices (\
                 id BIGINT PRIMARY KEY, total DECIMAL(12,2) NOT NULL, discount DECIMAL(12,2)\
             )",
            &[],
        )
        .await?;

    let invoice = Invoice {
        id: 1,
        total: BigDecimal::from_str("129.99").unwrap(),
        discount: Some(BigDecimal::from_str("10.00").unwrap()),
    };
    engine.execute(&invoice.insert()).await?;

    let table = Invoice::table();
    let fetched: Invoice = engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, invoice);

    engine
        .connect()
        .await?
        .execute("DROP TABLE decimal_value_mysql_invoices", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn null_decimal_field_round_trips_as_none() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute(
            "DROP TABLE IF EXISTS decimal_value_mysql_null_invoices",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE decimal_value_mysql_null_invoices (\
                 id BIGINT PRIMARY KEY, total DECIMAL(12,2) NOT NULL, discount DECIMAL(12,2)\
             )",
            &[],
        )
        .await?;

    let table = Table::new("decimal_value_mysql_null_invoices");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("total", BigDecimal::from_str("50.00").unwrap())
                .value("discount", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    let discount: Option<BigDecimal> = row.get_by_name("discount")?;
    assert_eq!(discount, None);

    engine
        .connect()
        .await?
        .execute("DROP TABLE decimal_value_mysql_null_invoices", &[])
        .await?;
    Ok(())
}
