#![cfg(feature = "sqlite")]

//! Exercises the portable DDL builder (`CreateTable`/`DropTable`/
//! `CreateIndex`/`DropIndex`) against a real SQLite database: every
//! statement it renders is actually executed, not just checked for SQL
//! text, and the resulting table/index is then used through ordinary
//! inserts/selects to confirm it behaves as declared.

use rusty_db::prelude::*;

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_ddl_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    SqliteDriver::engine(&url).await
}

#[tokio::test]
async fn create_table_builds_a_table_usable_through_ordinary_inserts_and_selects(
) -> rusty_db::Result<()> {
    let engine = file_engine("basic_usage").await?;

    let create = CreateTable::new("accounts")
        .column("id", ColumnType::I64)
        .primary_key()
        .column("name", ColumnType::Text)
        .not_null()
        .column("balance", ColumnType::F64)
        .not_null()
        .column("active", ColumnType::Bool)
        .not_null();
    engine.execute(&create).await?;

    let table = Table::new("accounts");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("name", "ada")
                .value("balance", 42.5_f64)
                .value("active", true),
        )
        .await?;

    let rows = engine.fetch_all(&Select::from(&table)).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_name::<String>("name")?, "ada");
    assert_eq!(rows[0].get_by_name::<f64>("balance")?, 42.5);
    assert!(rows[0].get_by_name::<bool>("active")?);

    Ok(())
}

#[tokio::test]
async fn if_not_exists_is_idempotent() -> rusty_db::Result<()> {
    let engine = file_engine("if_not_exists").await?;

    let create = || {
        CreateTable::new("widgets")
            .if_not_exists()
            .column("id", ColumnType::I64)
            .primary_key()
    };
    engine.execute(&create()).await?;
    // A plain (non-"if not exists") second attempt would fail with "table
    // already exists" — if_not_exists is what makes this a no-op instead.
    engine.execute(&create()).await?;

    Ok(())
}

#[tokio::test]
async fn unique_column_constraint_rejects_a_duplicate_value() -> rusty_db::Result<()> {
    let engine = file_engine("unique_column").await?;

    let create = CreateTable::new("users")
        .column("id", ColumnType::I64)
        .primary_key()
        .column("email", ColumnType::Text)
        .not_null()
        .unique();
    engine.execute(&create).await?;

    let table = Table::new("users");
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
        "a duplicate unique value should be rejected"
    );

    Ok(())
}

#[tokio::test]
async fn check_constraint_is_enforced() -> rusty_db::Result<()> {
    let engine = file_engine("check_constraint").await?;

    let create = CreateTable::new("products")
        .column("id", ColumnType::I64)
        .primary_key()
        .column("price", ColumnType::F64)
        .not_null()
        .check("price >= 0");
    engine.execute(&create).await?;

    let table = Table::new("products");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("price", 9.99_f64),
        )
        .await?;

    let outcome = engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 2_i64)
                .value("price", -1.0_f64),
        )
        .await;
    assert!(
        outcome.is_err(),
        "a negative price should violate the CHECK constraint"
    );

    Ok(())
}

#[tokio::test]
async fn foreign_key_clause_is_accepted() -> rusty_db::Result<()> {
    let engine = file_engine("foreign_key_clause").await?;

    engine
        .execute(
            &CreateTable::new("tenants")
                .column("id", ColumnType::I64)
                .primary_key(),
        )
        .await?;
    // SQLite doesn't enforce foreign keys unless PRAGMA foreign_keys=ON is
    // set per-connection (this crate doesn't turn it on), so this only
    // proves the FOREIGN KEY clause itself is valid SQL SQLite accepts —
    // not that a violation would be rejected (see ddl_postgres.rs/
    // ddl_mysql.rs, where foreign keys are enforced natively).
    engine
        .execute(
            &CreateTable::new("orders")
                .column("id", ColumnType::I64)
                .primary_key()
                .column("tenant_id", ColumnType::I64)
                .not_null()
                .foreign_key(["tenant_id"], "tenants", ["id"]),
        )
        .await?;

    Ok(())
}

#[tokio::test]
async fn autoincrement_primary_key_assigns_increasing_ids() -> rusty_db::Result<()> {
    let engine = file_engine("autoincrement").await?;

    let create = CreateTable::new("events")
        .column("id", ColumnType::I64)
        .primary_key()
        .autoincrement()
        .column("label", ColumnType::Text)
        .not_null();
    engine.execute(&create).await?;

    let table = Table::new("events");
    for label in ["first", "second", "third"] {
        engine
            .execute(&Insert::into_table(&table).value("label", label))
            .await?;
    }

    let rows = engine
        .fetch_all(&Select::from(&table).order_by(table.col("id").asc()))
        .await?;
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| r.get_by_name::<i64>("id"))
        .collect::<rusty_db::Result<_>>()?;
    assert_eq!(ids, vec![1, 2, 3]);

    Ok(())
}

#[tokio::test]
async fn default_raw_fills_in_when_a_column_is_omitted_from_the_insert() -> rusty_db::Result<()> {
    let engine = file_engine("default_raw").await?;

    let create = CreateTable::new("tasks")
        .column("id", ColumnType::I64)
        .primary_key()
        .column("status", ColumnType::Text)
        .not_null()
        .default_raw("'pending'");
    engine.execute(&create).await?;

    let table = Table::new("tasks");
    // "status" is never mentioned — the column's own DEFAULT fills it in,
    // not the query builder (contrast `#[table(default = "...")]`, which
    // substitutes at the Rust-struct/INSERT-value level instead).
    engine
        .execute(&Insert::into_table(&table).value("id", 1_i64))
        .await?;

    let row = engine
        .fetch_optional(&Select::from(&table))
        .await?
        .expect("the row was inserted");
    assert_eq!(row.get_by_name::<String>("status")?, "pending");

    Ok(())
}

#[tokio::test]
async fn drop_table_removes_it() -> rusty_db::Result<()> {
    let engine = file_engine("drop_table").await?;

    engine
        .execute(
            &CreateTable::new("scratch")
                .column("id", ColumnType::I64)
                .primary_key(),
        )
        .await?;
    engine.execute(&DropTable::new("scratch")).await?;

    let outcome = engine
        .fetch_all(&Select::from(&Table::new("scratch")))
        .await;
    assert!(outcome.is_err(), "the table should no longer exist");

    // DropTable::if_exists() makes a second drop a no-op instead of an error.
    engine
        .execute(&DropTable::new("scratch").if_exists())
        .await?;

    Ok(())
}

#[tokio::test]
async fn create_index_enforces_uniqueness_and_drop_index_lifts_it() -> rusty_db::Result<()> {
    let engine = file_engine("create_drop_index").await?;

    engine
        .execute(
            &CreateTable::new("people")
                .column("id", ColumnType::I64)
                .primary_key()
                .column("email", ColumnType::Text)
                .not_null(),
        )
        .await?;
    engine
        .execute(&CreateIndex::new("idx_people_email", "people", ["email"]).unique())
        .await?;

    let table = Table::new("people");
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

    engine
        .execute(&DropIndex::new("idx_people_email", "people"))
        .await?;
    // Now that the index is gone, the same duplicate insert succeeds.
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 2_i64)
                .value("email", "ada@example.com"),
        )
        .await?;

    Ok(())
}
