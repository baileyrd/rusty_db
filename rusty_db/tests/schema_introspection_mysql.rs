#![cfg(feature = "mysql")]

//! Same coverage as `schema_introspection.rs` (SQLite), against a real
//! MySQL/MariaDB server.

use rusty_db::{ColumnInfo, Engine};

/// Connects to a real MySQL/MariaDB server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `MYSQL_TEST_URL` at a scratch database (its schema
/// is created and dropped by this test) or the test skips itself instead of
/// failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match rusty_db::mysql::MySqlDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL test: could not connect to {url}: {err}");
            None
        }
    }
}

#[tokio::test]
async fn list_tables_reports_created_tables() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS schema_introspection_widgets", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE schema_introspection_widgets (id BIGINT PRIMARY KEY)",
            &[],
        )
        .await?;

    let tables = engine.list_tables().await?;
    assert!(
        tables.iter().any(|t| t == "schema_introspection_widgets"),
        "expected schema_introspection_widgets in {tables:?}"
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE schema_introspection_widgets", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn table_schema_reports_columns_nullability_and_primary_key() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS schema_introspection_people", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE schema_introspection_people (\
                 id BIGINT PRIMARY KEY, \
                 name TEXT NOT NULL, \
                 nickname TEXT\
             )",
            &[],
        )
        .await?;

    let schema = engine
        .table_schema("schema_introspection_people")
        .await?
        .expect("table exists");
    assert_eq!(schema.name, "schema_introspection_people");
    assert_eq!(
        schema.columns,
        vec![
            ColumnInfo {
                // MySQL's `column_type` includes the display width.
                name: "id".to_string(),
                type_name: "bigint(20)".to_string(),
                nullable: false,
                primary_key: true,
                default: None,
            },
            ColumnInfo {
                name: "name".to_string(),
                type_name: "text".to_string(),
                nullable: false,
                primary_key: false,
                default: None,
            },
            ColumnInfo {
                name: "nickname".to_string(),
                type_name: "text".to_string(),
                nullable: true,
                primary_key: false,
                default: None,
            },
        ]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE schema_introspection_people", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn table_schema_returns_none_for_a_table_that_does_not_exist() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    assert_eq!(
        engine
            .table_schema("schema_introspection_does_not_exist")
            .await?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn table_schema_reports_defaults_unique_and_check_constraints() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS schema_introspection_accounts", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE schema_introspection_accounts (\
                 id BIGINT PRIMARY KEY, \
                 email VARCHAR(255) NOT NULL, \
                 balance BIGINT NOT NULL DEFAULT 0, \
                 CONSTRAINT email_unique UNIQUE (email), \
                 CONSTRAINT balance_check CHECK (balance >= 0)\
             )",
            &[],
        )
        .await?;

    let schema = engine
        .table_schema("schema_introspection_accounts")
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
    assert_eq!(schema.unique_constraints[0].name, "email_unique");
    assert_eq!(
        schema.unique_constraints[0].columns,
        vec!["email".to_string()]
    );

    assert_eq!(schema.check_constraints.len(), 1);
    assert_eq!(schema.check_constraints[0].name, "balance_check");
    assert_eq!(schema.check_constraints[0].expression, "`balance` >= 0");

    engine
        .connect()
        .await?
        .execute("DROP TABLE schema_introspection_accounts", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn table_schema_reports_single_column_and_composite_foreign_keys() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS schema_introspection_children", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS schema_introspection_parents", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE schema_introspection_parents (\
                 id BIGINT PRIMARY KEY, \
                 code VARCHAR(50), \
                 UNIQUE KEY uq_id_code (id, code)\
             )",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE schema_introspection_children (\
                 id BIGINT PRIMARY KEY, \
                 parent_id BIGINT, \
                 parent_code VARCHAR(50), \
                 CONSTRAINT fk_single FOREIGN KEY (parent_id) \
                     REFERENCES schema_introspection_parents (id), \
                 CONSTRAINT fk_composite FOREIGN KEY (parent_id, parent_code) \
                     REFERENCES schema_introspection_parents (id, code)\
             )",
            &[],
        )
        .await?;

    let schema = engine
        .table_schema("schema_introspection_children")
        .await?
        .expect("table exists");
    assert_eq!(schema.foreign_keys.len(), 2);

    let single = schema
        .foreign_keys
        .iter()
        .find(|fk| fk.columns == vec!["parent_id".to_string()])
        .expect("the single-column foreign key");
    assert_eq!(single.name, "fk_single");
    assert_eq!(single.referenced_table, "schema_introspection_parents");
    assert_eq!(single.referenced_columns, vec!["id".to_string()]);

    let composite = schema
        .foreign_keys
        .iter()
        .find(|fk| fk.columns.len() == 2)
        .expect("the composite foreign key");
    assert_eq!(composite.name, "fk_composite");
    assert_eq!(
        composite.columns,
        vec!["parent_id".to_string(), "parent_code".to_string()]
    );
    assert_eq!(composite.referenced_table, "schema_introspection_parents");
    assert_eq!(
        composite.referenced_columns,
        vec!["id".to_string(), "code".to_string()]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE schema_introspection_children", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute("DROP TABLE schema_introspection_parents", &[])
        .await?;

    Ok(())
}
