#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Value::Json`/`Json` on SQLite, which has no native JSON
//! column type at all — a JSON column there is really just `TEXT`, so this
//! covers `Json` round-tripping through a mapped struct via `FromValue`'s
//! text-parsing fallback rather than the native `Value::Json` form (see
//! `json_value_postgres.rs` for that).

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "events")]
struct Event {
    #[table(primary_key)]
    id: i64,
    name: String,
    payload: Json,
    metadata: Option<Json>,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             payload TEXT NOT NULL, metadata TEXT)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn json_field_round_trips_through_text_storage() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let event = Event {
        id: 1,
        name: "signup".to_string(),
        payload: serde_json::json!({"user_id": 42, "plan": "pro"}),
        metadata: Some(serde_json::json!(["a", "b", "c"])),
    };
    engine.execute(&event.insert()).await?;

    let fetched: Event = engine
        .fetch_one_as(&Select::from(&Event::table()).filter(Event::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, event);

    Ok(())
}

#[tokio::test]
async fn null_json_field_round_trips_as_none() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let event = Event {
        id: 1,
        name: "ping".to_string(),
        payload: serde_json::json!({}),
        metadata: None,
    };
    engine.execute(&event.insert()).await?;

    let fetched: Event = engine
        .fetch_one_as(&Select::from(&Event::table()).filter(Event::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched.metadata, None);

    Ok(())
}

#[tokio::test]
async fn raw_value_round_trips_json_through_text() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let payload = serde_json::json!({"nested": {"a": [1, 2, 3]}});
    let table = Table::new("events");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("name", "raw")
                .value("payload", payload.clone())
                .value("metadata", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    let decoded: Json = row.get_by_name("payload")?;
    assert_eq!(decoded, payload);

    Ok(())
}
