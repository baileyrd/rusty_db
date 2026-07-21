#![cfg(feature = "postgres")]

//! Same coverage as `schema_introspection.rs` (SQLite), against a real
//! Postgres server.

use rusty_db::{ColumnInfo, Engine};

/// Connects to a real PostgreSQL server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `POSTGRES_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| "postgres://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match rusty_db::postgres::PostgresDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping Postgres test: could not connect to {url}: {err}");
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
                name: "id".to_string(),
                type_name: "bigint".to_string(),
                nullable: false,
                primary_key: true,
            },
            ColumnInfo {
                name: "name".to_string(),
                type_name: "text".to_string(),
                nullable: false,
                primary_key: false,
            },
            ColumnInfo {
                name: "nickname".to_string(),
                type_name: "text".to_string(),
                nullable: true,
                primary_key: false,
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
