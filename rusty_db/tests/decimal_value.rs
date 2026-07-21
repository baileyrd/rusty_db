#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Value::Decimal`/`BigDecimal` on SQLite, which has no native
//! NUMERIC/DECIMAL column type at all — a decimal column there decodes as
//! whatever runtime type the stored value actually has, so this covers
//! `BigDecimal` round-tripping through a mapped struct via `FromValue`'s
//! text-parsing fallback rather than the native `Value::Decimal` form (see
//! `decimal_value_postgres.rs` for that).

use std::str::FromStr;

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "invoices")]
struct Invoice {
    #[table(primary_key)]
    id: i64,
    total: BigDecimal,
    discount: Option<BigDecimal>,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE invoices (id INTEGER PRIMARY KEY, total TEXT NOT NULL, discount TEXT)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn decimal_field_round_trips_through_text_storage() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let invoice = Invoice {
        id: 1,
        total: BigDecimal::from_str("129.99").unwrap(),
        discount: Some(BigDecimal::from_str("10.005").unwrap()),
    };
    engine.execute(&invoice.insert()).await?;

    let fetched: Invoice = engine
        .fetch_one_as(&Select::from(&Invoice::table()).filter(Invoice::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, invoice);

    Ok(())
}

#[tokio::test]
async fn null_decimal_field_round_trips_as_none() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let invoice = Invoice {
        id: 1,
        total: BigDecimal::from_str("50").unwrap(),
        discount: None,
    };
    engine.execute(&invoice.insert()).await?;

    let fetched: Invoice = engine
        .fetch_one_as(&Select::from(&Invoice::table()).filter(Invoice::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched.discount, None);

    Ok(())
}

#[tokio::test]
async fn raw_value_round_trips_decimal_through_text() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let total = BigDecimal::from_str("42.5").unwrap();
    let table = Table::new("invoices");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("total", total.clone())
                .value("discount", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    let decoded: BigDecimal = row.get_by_name("total")?;
    assert_eq!(decoded, total);

    Ok(())
}
