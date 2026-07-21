#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Value::Uuid`/`Uuid` on SQLite, which has no native UUID
//! column type — a UUID column there is really just `TEXT`, so this
//! covers `Uuid` round-tripping through a mapped struct via `FromValue`'s
//! text-parsing fallback rather than the native `Value::Uuid` form (see
//! `uuid_value_postgres.rs` for that).

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "widgets")]
struct Widget {
    #[table(primary_key)]
    id: Uuid,
    name: String,
    owner: Option<Uuid>,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE widgets (id TEXT PRIMARY KEY, name TEXT NOT NULL, owner TEXT)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn uuid_field_round_trips_through_text_storage() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let id = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let widget = Widget {
        id,
        name: "gizmo".to_string(),
        owner: Some(owner),
    };
    engine.execute(&widget.insert()).await?;

    let fetched: Widget = engine
        .fetch_one_as(&Select::from(&Widget::table()).filter(Widget::table().col("id").eq(id)))
        .await?;
    assert_eq!(fetched, widget);
    assert_eq!(fetched.owner, Some(owner));

    Ok(())
}

#[tokio::test]
async fn null_uuid_field_round_trips_as_none() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let widget = Widget {
        id: Uuid::new_v4(),
        name: "ownerless".to_string(),
        owner: None,
    };
    engine.execute(&widget.insert()).await?;

    let fetched: Widget = engine
        .fetch_one_as(
            &Select::from(&Widget::table()).filter(Widget::table().col("id").eq(widget.id)),
        )
        .await?;
    assert_eq!(fetched.owner, None);

    Ok(())
}

#[tokio::test]
async fn raw_value_round_trips_uuid_through_text() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let id = Uuid::new_v4();
    let table = Table::new("widgets");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", id)
                .value("name", "raw")
                .value("owner", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(id)))
        .await?;
    let decoded: Uuid = row.get_by_name("id")?;
    assert_eq!(decoded, id);

    Ok(())
}
