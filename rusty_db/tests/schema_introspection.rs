#![cfg(feature = "sqlite")]

//! Exercises `Engine::list_tables`/`table_schema`: reflecting a live
//! database's actual catalog rather than relying on what the
//! application's own `#[derive(Mapped)]` structs declare.

use rusty_db::{ColumnInfo, Engine};

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_schema_introspection_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    rusty_db::sqlite::SqliteDriver::engine(&url).await
}

#[tokio::test]
async fn list_tables_reports_created_tables_and_ignores_sqlite_internals() -> rusty_db::Result<()> {
    let engine = file_engine("list_tables").await?;
    engine
        .connect()
        .await?
        .execute("CREATE TABLE widgets (id INTEGER PRIMARY KEY)", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute("CREATE TABLE gadgets (id INTEGER PRIMARY KEY)", &[])
        .await?;

    let tables = engine.list_tables().await?;
    assert_eq!(tables, vec!["gadgets".to_string(), "widgets".to_string()]);
    assert!(
        !tables.iter().any(|t| t.starts_with("sqlite_")),
        "internal sqlite_* tables should never be reported: {tables:?}"
    );

    Ok(())
}

#[tokio::test]
async fn table_schema_reports_columns_nullability_and_primary_key() -> rusty_db::Result<()> {
    let engine = file_engine("table_schema").await?;
    engine
        .connect()
        .await?
        .execute(
            // `NOT NULL` on the primary key is redundant in SQLite (an
            // `INTEGER PRIMARY KEY` column can never actually hold NULL
            // regardless), but spelled out explicitly anyway so its
            // reflected `nullable` flag — which echoes the DDL text, not
            // SQLite's deeper rowid-alias semantics — comes back
            // deterministically `false` rather than depending on that
            // quirk.
            "CREATE TABLE people (\
                 id INTEGER PRIMARY KEY NOT NULL, \
                 name TEXT NOT NULL, \
                 nickname TEXT\
             )",
            &[],
        )
        .await?;

    let schema = engine.table_schema("people").await?.expect("table exists");
    assert_eq!(schema.name, "people");
    assert_eq!(
        schema.columns,
        vec![
            ColumnInfo {
                name: "id".to_string(),
                type_name: "INTEGER".to_string(),
                nullable: false,
                primary_key: true,
                default: None,
            },
            ColumnInfo {
                name: "name".to_string(),
                type_name: "TEXT".to_string(),
                nullable: false,
                primary_key: false,
                default: None,
            },
            ColumnInfo {
                name: "nickname".to_string(),
                type_name: "TEXT".to_string(),
                nullable: true,
                primary_key: false,
                default: None,
            },
        ]
    );

    Ok(())
}

#[tokio::test]
async fn table_schema_reports_defaults_unique_and_check_constraints() -> rusty_db::Result<()> {
    let engine = file_engine("constraints").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE accounts (\
                 id INTEGER PRIMARY KEY, \
                 email TEXT NOT NULL, \
                 balance INTEGER NOT NULL DEFAULT 0, \
                 CONSTRAINT email_unique UNIQUE (email), \
                 CONSTRAINT balance_check CHECK (balance >= 0)\
             )",
            &[],
        )
        .await?;

    let schema = engine
        .table_schema("accounts")
        .await?
        .expect("table exists");

    let balance = schema.columns.iter().find(|c| c.name == "balance").unwrap();
    assert_eq!(balance.default.as_deref(), Some("0"));
    let email = schema.columns.iter().find(|c| c.name == "email").unwrap();
    assert_eq!(
        email.default, None,
        "a column with no DEFAULT reflects None"
    );

    assert_eq!(schema.unique_constraints.len(), 1);
    // SQLite implements UNIQUE as an index and doesn't actually keep the
    // `CONSTRAINT email_unique` name from the DDL — it names the backing
    // index itself instead (`sqlite_autoindex_<table>_<n>`).
    assert!(
        schema.unique_constraints[0]
            .name
            .starts_with("sqlite_autoindex_accounts_"),
        "got {:?}",
        schema.unique_constraints[0].name
    );
    assert_eq!(
        schema.unique_constraints[0].columns,
        vec!["email".to_string()]
    );

    assert_eq!(schema.check_constraints.len(), 1);
    assert_eq!(schema.check_constraints[0].name, "balance_check");
    assert_eq!(schema.check_constraints[0].expression, "balance >= 0");

    Ok(())
}

#[tokio::test]
async fn table_schema_returns_none_for_a_table_that_does_not_exist() -> rusty_db::Result<()> {
    let engine = file_engine("missing_table").await?;
    assert_eq!(engine.table_schema("does_not_exist").await?, None);
    Ok(())
}

#[tokio::test]
async fn list_tables_is_empty_for_a_fresh_database() -> rusty_db::Result<()> {
    let engine = file_engine("empty").await?;
    assert_eq!(engine.list_tables().await?, Vec::<String>::new());
    Ok(())
}
