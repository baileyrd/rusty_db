#![cfg(all(feature = "mysql", feature = "derive"))]

//! Exercises `Value::Uuid`/`Uuid` against a real MySQL/MariaDB server,
//! which has no native UUID column type (unlike Postgres — see
//! `uuid_value_postgres.rs`) — a UUID column here is really just
//! `CHAR(36)`, so this covers `Uuid` round-tripping through a mapped
//! struct via `FromValue`'s text-parsing fallback.

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

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "uuid_value_mysql_widgets")]
struct Widget {
    #[table(primary_key)]
    id: Uuid,
    name: String,
    owner: Option<Uuid>,
}

#[tokio::test]
async fn uuid_field_round_trips_through_char36_storage() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS uuid_value_mysql_widgets", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE uuid_value_mysql_widgets (\
                 id CHAR(36) PRIMARY KEY, name TEXT NOT NULL, owner CHAR(36)\
             )",
            &[],
        )
        .await?;

    let id = Uuid::new_v4();
    let owner = Uuid::new_v4();
    let widget = Widget {
        id,
        name: "gizmo".to_string(),
        owner: Some(owner),
    };
    engine.execute(&widget.insert()).await?;

    let table = Widget::table();
    let fetched: Widget = engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(id)))
        .await?;
    assert_eq!(fetched, widget);

    engine
        .connect()
        .await?
        .execute("DROP TABLE uuid_value_mysql_widgets", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn null_uuid_field_round_trips_as_none() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS uuid_value_mysql_null_widgets", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE uuid_value_mysql_null_widgets (\
                 id CHAR(36) PRIMARY KEY, name TEXT NOT NULL, owner CHAR(36)\
             )",
            &[],
        )
        .await?;

    let widget = Widget {
        id: Uuid::new_v4(),
        name: "ownerless".to_string(),
        owner: None,
    };
    let table = Table::new("uuid_value_mysql_null_widgets");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", widget.id)
                .value("name", widget.name.clone())
                .value("owner", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(widget.id)))
        .await?;
    let owner: Option<Uuid> = row.get_by_name("owner")?;
    assert_eq!(owner, None);

    engine
        .connect()
        .await?
        .execute("DROP TABLE uuid_value_mysql_null_widgets", &[])
        .await?;
    Ok(())
}
