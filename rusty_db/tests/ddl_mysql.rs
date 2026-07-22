#![cfg(feature = "mysql")]

//! Exercises the portable DDL builder against a real MySQL/MariaDB server —
//! a reduced version of `ddl.rs`: just the cases that differ meaningfully
//! from SQLite (native-type column mapping, foreign keys actually being
//! enforced, and `DROP INDEX` needing `ON <table>`, which only MySQL/
//! MariaDB requires).

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

#[tokio::test]
async fn create_table_maps_native_types_and_supports_ordinary_use() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .execute(&DropTable::new("ddl_mysql_widgets").if_exists())
        .await?;
    let create = CreateTable::new("ddl_mysql_widgets")
        .column("id", ColumnType::I64)
        .primary_key()
        .autoincrement()
        .column("label", ColumnType::VarChar(50))
        .not_null()
        .column("payload", ColumnType::Json)
        .not_null();
    engine.execute(&create).await?;

    let table = Table::new("ddl_mysql_widgets");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("label", "widget")
                .value("payload", serde_json::json!({"k": "v"})),
        )
        .await?;

    let row = engine
        .fetch_optional(&Select::from(&table))
        .await?
        .expect("the row was inserted");
    assert_eq!(row.get_by_name::<String>("label")?, "widget");
    assert_eq!(
        row.get_by_name::<i64>("id")?,
        1,
        "autoincrement assigned the first id"
    );

    engine.execute(&DropTable::new("ddl_mysql_widgets")).await?;
    Ok(())
}

#[tokio::test]
async fn foreign_key_is_actually_enforced() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .execute(&DropTable::new("ddl_mysql_orders").if_exists())
        .await?;
    engine
        .execute(&DropTable::new("ddl_mysql_tenants").if_exists())
        .await?;
    engine
        .execute(
            &CreateTable::new("ddl_mysql_tenants")
                .column("id", ColumnType::I64)
                .primary_key(),
        )
        .await?;
    engine
        .execute(
            &CreateTable::new("ddl_mysql_orders")
                .column("id", ColumnType::I64)
                .primary_key()
                .column("tenant_id", ColumnType::I64)
                .not_null()
                .foreign_key(["tenant_id"], "ddl_mysql_tenants", ["id"]),
        )
        .await?;

    let orders = Table::new("ddl_mysql_orders");
    let outcome = engine
        .execute(
            &Insert::into_table(&orders)
                .value("id", 1_i64)
                .value("tenant_id", 999_i64), // no such tenant
        )
        .await;
    assert!(
        outcome.is_err(),
        "MySQL/MariaDB (InnoDB) should reject an insert violating the foreign key"
    );

    engine
        .execute(&Insert::into_table(&Table::new("ddl_mysql_tenants")).value("id", 999_i64))
        .await?;
    // Now that the referenced row exists, the same insert succeeds.
    engine
        .execute(
            &Insert::into_table(&orders)
                .value("id", 1_i64)
                .value("tenant_id", 999_i64),
        )
        .await?;

    engine.execute(&DropTable::new("ddl_mysql_orders")).await?;
    engine.execute(&DropTable::new("ddl_mysql_tenants")).await?;
    Ok(())
}

#[tokio::test]
async fn drop_index_uses_the_table_name_mysql_requires() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .execute(&DropTable::new("ddl_mysql_people").if_exists())
        .await?;
    engine
        .execute(
            &CreateTable::new("ddl_mysql_people")
                .column("id", ColumnType::I64)
                .primary_key()
                .column("email", ColumnType::VarChar(255))
                .not_null(),
        )
        .await?;
    engine
        .execute(
            &CreateIndex::new("idx_mysql_people_email", "ddl_mysql_people", ["email"]).unique(),
        )
        .await?;

    let table = Table::new("ddl_mysql_people");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("email", "ada@example.com"),
        )
        .await?;
    let outcome = engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 2_i64)
                .value("email", "ada@example.com"),
        )
        .await;
    assert!(
        outcome.is_err(),
        "the unique index should reject a duplicate email"
    );

    // MySQL/MariaDB's DROP INDEX needs `ON <table>` — DropIndex::new
    // requires the table name up front for exactly this reason.
    engine
        .execute(&DropIndex::new(
            "idx_mysql_people_email",
            "ddl_mysql_people",
        ))
        .await?;
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 2_i64)
                .value("email", "ada@example.com"),
        )
        .await?;

    engine.execute(&DropTable::new("ddl_mysql_people")).await?;
    Ok(())
}

#[tokio::test]
async fn alter_table_add_and_drop_column() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .execute(&DropTable::new("ddl_mysql_alter").if_exists())
        .await?;
    engine
        .execute(
            &CreateTable::new("ddl_mysql_alter")
                .column("id", ColumnType::I64)
                .primary_key(),
        )
        .await?;
    let table = Table::new("ddl_mysql_alter");
    engine
        .execute(&Insert::into_table(&table).value("id", 1_i64))
        .await?;

    engine
        .execute(
            &AlterTable::add_column("ddl_mysql_alter", "credits", ColumnType::I64)
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
        .execute(&AlterTable::drop_column("ddl_mysql_alter", "credits"))
        .await?;
    let schema = engine
        .table_schema("ddl_mysql_alter")
        .await?
        .expect("the table still exists");
    assert!(
        !schema.columns.iter().any(|c| c.name == "credits"),
        "the dropped column should no longer be part of the schema"
    );

    engine.execute(&DropTable::new("ddl_mysql_alter")).await?;
    Ok(())
}
