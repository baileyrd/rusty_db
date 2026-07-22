#![cfg(feature = "postgres")]

//! Exercises the portable DDL builder against a real PostgreSQL server —
//! a reduced version of `ddl.rs`: just the cases that differ meaningfully
//! from SQLite (native-type column mapping, and foreign keys actually
//! being enforced, unlike SQLite's off-by-default behavior).

use rusty_db::prelude::*;

/// Connects to a real PostgreSQL server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `POSTGRES_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| "postgres://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match PostgresDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping Postgres test: could not connect to {url}: {err}");
            None
        }
    }
}

#[tokio::test]
async fn create_table_maps_native_types_and_supports_ordinary_use() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .execute(&DropTable::new("ddl_pg_widgets").if_exists())
        .await?;
    let create = CreateTable::new("ddl_pg_widgets")
        .column("id", ColumnType::I64)
        .primary_key()
        .autoincrement()
        .column("label", ColumnType::VarChar(50))
        .not_null()
        .column("id_str", ColumnType::Uuid)
        .not_null();
    engine.execute(&create).await?;

    let table = Table::new("ddl_pg_widgets");
    let id_str = Uuid::new_v4();
    engine
        .execute(
            &Insert::into_table(&table)
                .value("label", "widget")
                .value("id_str", id_str),
        )
        .await?;

    let row = engine
        .fetch_optional(&Select::from(&table))
        .await?
        .expect("the row was inserted");
    assert_eq!(row.get_by_name::<String>("label")?, "widget");
    assert_eq!(row.get_by_name::<Uuid>("id_str")?, id_str);
    assert_eq!(
        row.get_by_name::<i64>("id")?,
        1,
        "autoincrement assigned the first id"
    );

    engine.execute(&DropTable::new("ddl_pg_widgets")).await?;
    Ok(())
}

#[tokio::test]
async fn foreign_key_is_actually_enforced() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .execute(&DropTable::new("ddl_pg_orders").if_exists())
        .await?;
    engine
        .execute(&DropTable::new("ddl_pg_tenants").if_exists())
        .await?;
    engine
        .execute(
            &CreateTable::new("ddl_pg_tenants")
                .column("id", ColumnType::I64)
                .primary_key(),
        )
        .await?;
    engine
        .execute(
            &CreateTable::new("ddl_pg_orders")
                .column("id", ColumnType::I64)
                .primary_key()
                .column("tenant_id", ColumnType::I64)
                .not_null()
                .foreign_key(["tenant_id"], "ddl_pg_tenants", ["id"]),
        )
        .await?;

    let orders = Table::new("ddl_pg_orders");
    let outcome = engine
        .execute(
            &Insert::into_table(&orders)
                .value("id", 1_i64)
                .value("tenant_id", 999_i64), // no such tenant
        )
        .await;
    assert!(
        outcome.is_err(),
        "Postgres should reject an insert violating the foreign key"
    );

    engine
        .execute(&Insert::into_table(&Table::new("ddl_pg_tenants")).value("id", 999_i64))
        .await?;
    // Now that the referenced row exists, the same insert succeeds.
    engine
        .execute(
            &Insert::into_table(&orders)
                .value("id", 1_i64)
                .value("tenant_id", 999_i64),
        )
        .await?;

    engine.execute(&DropTable::new("ddl_pg_orders")).await?;
    engine.execute(&DropTable::new("ddl_pg_tenants")).await?;
    Ok(())
}

#[tokio::test]
async fn alter_table_add_and_drop_column() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .execute(&DropTable::new("ddl_pg_alter").if_exists())
        .await?;
    engine
        .execute(
            &CreateTable::new("ddl_pg_alter")
                .column("id", ColumnType::I64)
                .primary_key(),
        )
        .await?;
    let table = Table::new("ddl_pg_alter");
    engine
        .execute(&Insert::into_table(&table).value("id", 1_i64))
        .await?;

    engine
        .execute(
            &AlterTable::add_column("ddl_pg_alter", "credits", ColumnType::I64)
                .not_null()
                .default_raw("0"),
        )
        .await?;
    let row = engine
        .fetch_optional(&Select::from(&table))
        .await?
        .expect("the row still exists");
    assert_eq!(
        row.get_by_name::<i64>("credits")?,
        0,
        "the new column's default backfilled the existing row"
    );

    engine
        .execute(&AlterTable::drop_column("ddl_pg_alter", "credits"))
        .await?;
    // See `AlterTable`'s own docs: a "SELECT *"-shaped statement already
    // prepared against this table's pre-drop shape can hit Postgres's
    // "cached plan must not change result type" error if reused on the
    // same connection right after — a fresh `Engine` (like `test_engine()`
    // gives us here, a separate call from the one above) avoids it.
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    let schema = engine
        .table_schema("ddl_pg_alter")
        .await?
        .expect("the table still exists");
    assert!(
        !schema.columns.iter().any(|c| c.name == "credits"),
        "the dropped column should no longer be part of the schema"
    );

    engine.execute(&DropTable::new("ddl_pg_alter")).await?;
    Ok(())
}
