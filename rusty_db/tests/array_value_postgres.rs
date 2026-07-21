#![cfg(all(feature = "postgres", feature = "derive"))]

//! Exercises `Value::Array`/`Vec<T>` against a real Postgres server, which
//! has a native array column type for virtually every scalar type
//! (`INTEGER[]`/`TEXT[]`/`UUID[]`/... ) — unlike SQLite (see
//! `array_value.rs`), a column reflected/decoded here should come back as
//! `Value::Array` directly, not `Value::Text`.

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
#[table(name = "array_value_pg_playlists")]
struct Playlist {
    #[table(primary_key)]
    id: i64,
    track_ids: Vec<i64>,
    tags: Vec<String>,
    is_public: Vec<bool>,
    owner_ids: Vec<Uuid>,
    featured_ids: Option<Vec<i64>>,
}

#[tokio::test]
async fn array_fields_round_trip_through_native_array_columns() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS array_value_pg_playlists", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE array_value_pg_playlists (\
                 id BIGINT PRIMARY KEY, track_ids BIGINT[] NOT NULL, tags TEXT[] NOT NULL, \
                 is_public BOOLEAN[] NOT NULL, owner_ids UUID[] NOT NULL, featured_ids BIGINT[]\
             )",
            &[],
        )
        .await?;

    let playlist = Playlist {
        id: 1,
        track_ids: vec![10, 20, 30],
        tags: vec!["rock".to_string(), "live".to_string()],
        is_public: vec![true, false],
        owner_ids: vec![Uuid::nil(), Uuid::nil()],
        featured_ids: Some(vec![10, 30]),
    };
    engine.execute(&playlist.insert()).await?;

    let table = Playlist::table();
    let fetched: Playlist = engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, playlist);

    // Confirm the native path is actually taken, not text-flattened.
    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert!(matches!(row.value(1), Some(Value::Array(_))));
    assert!(matches!(row.value(2), Some(Value::Array(_))));
    assert!(matches!(row.value(3), Some(Value::Array(_))));
    assert!(matches!(row.value(4), Some(Value::Array(_))));

    engine
        .connect()
        .await?
        .execute("DROP TABLE array_value_pg_playlists", &[])
        .await?;
    Ok(())
}

// An empty (or all-Value::Null) Value::Array has no element to inspect,
// so binding it picks TEXT[] by default (see Value::Array's own doc) —
// this round-trips correctly when the target column really is TEXT[]...
#[tokio::test]
async fn empty_array_round_trips_into_a_text_column() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS array_value_pg_empty_tags", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE array_value_pg_empty_tags (id BIGINT PRIMARY KEY, tags TEXT[] NOT \
             NULL)",
            &[],
        )
        .await?;

    let table = Table::new("array_value_pg_empty_tags");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("tags", Vec::<String>::new()),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(
        row.get_by_name::<Vec<String>>("tags")?,
        Vec::<String>::new()
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE array_value_pg_empty_tags", &[])
        .await?;
    Ok(())
}

// ...but errors (rather than silently storing wrong data) against a
// differently-typed column, since the default TEXT[] doesn't match —
// pinning down the documented limitation as an explicit, expected error.
#[tokio::test]
async fn empty_array_into_a_non_text_column_is_a_clear_database_error() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS array_value_pg_empty_scores", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE array_value_pg_empty_scores (id BIGINT PRIMARY KEY, scores BIGINT[] \
             NOT NULL)",
            &[],
        )
        .await?;

    let table = Table::new("array_value_pg_empty_scores");
    let result = engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("scores", Vec::<i64>::new()),
        )
        .await;
    assert!(result.is_err());

    engine
        .connect()
        .await?
        .execute("DROP TABLE array_value_pg_empty_scores", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn array_with_a_null_element_decodes_via_vec_value_but_not_vec_i64() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS array_value_pg_nullable_elements", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE array_value_pg_nullable_elements (id BIGINT PRIMARY KEY, scores \
             BIGINT[] NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "INSERT INTO array_value_pg_nullable_elements (id, scores) VALUES (1, \
             ARRAY[1, NULL, 3]::BIGINT[])",
            &[],
        )
        .await?;

    let table = Table::new("array_value_pg_nullable_elements");
    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;

    // The escape-hatch Vec<Value> handles a NULL element directly.
    let raw: Vec<Value> = row.get_by_name("scores")?;
    assert_eq!(raw, vec![Value::I64(1), Value::Null, Value::I64(3)]);

    // A concrete Vec<i64> field has nowhere to put a NULL element, so it's
    // a decode error rather than silently dropping/defaulting it.
    let concrete: rusty_db::Result<Vec<i64>> = row.get_by_name("scores");
    assert!(concrete.is_err());

    engine
        .connect()
        .await?
        .execute("DROP TABLE array_value_pg_nullable_elements", &[])
        .await?;
    Ok(())
}
