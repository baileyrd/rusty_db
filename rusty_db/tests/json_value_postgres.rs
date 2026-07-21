#![cfg(all(feature = "postgres", feature = "derive"))]

//! Exercises `Value::Json`/`Json` against a real Postgres server, which has
//! native `JSON`/`JSONB` column types — unlike SQLite/MySQL (see
//! `json_value.rs`), a column reflected/decoded here should come back as
//! `Value::Json` directly, not `Value::Text`.

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

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "json_value_pg_events")]
struct Event {
    #[table(primary_key)]
    id: i64,
    name: String,
    payload: Json,
    metadata: Option<Json>,
}

#[tokio::test]
async fn json_field_round_trips_through_a_native_jsonb_column() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS json_value_pg_events", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE json_value_pg_events (\
                 id BIGINT PRIMARY KEY, name TEXT NOT NULL, \
                 payload JSONB NOT NULL, metadata JSONB\
             )",
            &[],
        )
        .await?;

    let event = Event {
        id: 1,
        name: "signup".to_string(),
        payload: serde_json::json!({"user_id": 42, "plan": "pro"}),
        metadata: Some(serde_json::json!(["a", "b", "c"])),
    };
    engine.execute(&event.insert()).await?;

    let table = Event::table();
    let fetched: Event = engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, event);

    // Confirm the native path is actually taken, not text-flattened.
    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert!(
        matches!(row.value(2), Some(Value::Json(_))),
        "a native JSONB column should decode as Value::Json, not Value::Text: {:?}",
        row.value(2)
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE json_value_pg_events", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn null_json_field_round_trips_as_none() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS json_value_pg_null_events", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE json_value_pg_null_events (\
                 id BIGINT PRIMARY KEY, name TEXT NOT NULL, \
                 payload JSONB NOT NULL, metadata JSONB\
             )",
            &[],
        )
        .await?;

    let table = Table::new("json_value_pg_null_events");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("name", "ping")
                .value("payload", serde_json::json!({}))
                .value("metadata", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    let metadata: Option<Json> = row.get_by_name("metadata")?;
    assert_eq!(metadata, None);

    engine
        .connect()
        .await?
        .execute("DROP TABLE json_value_pg_null_events", &[])
        .await?;
    Ok(())
}
