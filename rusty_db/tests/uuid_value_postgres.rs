#![cfg(all(feature = "postgres", feature = "derive"))]

//! Exercises `Value::Uuid`/`Uuid` against a real Postgres server, which has
//! a native `UUID` column type — unlike SQLite/MySQL (see `uuid_value.rs`),
//! a column reflected/decoded here should come back as `Value::Uuid`
//! directly, not `Value::Text`.

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
#[table(name = "uuid_value_pg_widgets")]
struct Widget {
    #[table(primary_key)]
    id: Uuid,
    name: String,
    owner: Option<Uuid>,
}

#[tokio::test]
async fn uuid_field_round_trips_through_a_native_uuid_column() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS uuid_value_pg_widgets", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE uuid_value_pg_widgets (id UUID PRIMARY KEY, name TEXT NOT NULL, owner UUID)",
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

    // Confirm the native path is actually taken, not text-flattened.
    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(id)))
        .await?;
    assert!(
        matches!(row.value(0), Some(Value::Uuid(_))),
        "a native UUID column should decode as Value::Uuid, not Value::Text: {:?}",
        row.value(0)
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE uuid_value_pg_widgets", &[])
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
        .execute("DROP TABLE IF EXISTS uuid_value_pg_null_widgets", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE uuid_value_pg_null_widgets (id UUID PRIMARY KEY, name TEXT NOT NULL, owner UUID)",
            &[],
        )
        .await?;

    let table = Table::new("uuid_value_pg_null_widgets");
    let id = Uuid::new_v4();
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", id)
                .value("name", "ownerless")
                .value("owner", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(id)))
        .await?;
    let owner: Option<Uuid> = row.get_by_name("owner")?;
    assert_eq!(owner, None);

    engine
        .connect()
        .await?
        .execute("DROP TABLE uuid_value_pg_null_widgets", &[])
        .await?;
    Ok(())
}
