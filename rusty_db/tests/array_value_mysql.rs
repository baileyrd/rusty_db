#![cfg(all(feature = "mysql", feature = "derive"))]

//! Exercises `Value::Array`/`Vec<T>` against a real MySQL/MariaDB server,
//! which has no array column type at all — like SQLite (see
//! `array_value.rs`), every array flattens to a JSON array. Storing it in
//! a native MySQL `JSON` column specifically means it decodes back as
//! `Value::Bytes` rather than `Value::Text` (see `Value::Json`'s own doc
//! for why), which this covers too.

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
#[table(name = "array_value_mysql_playlists")]
struct Playlist {
    #[table(primary_key)]
    id: i64,
    track_ids: Vec<i64>,
    tags: Vec<String>,
    featured_ids: Option<Vec<i64>>,
}

#[tokio::test]
async fn array_fields_round_trip_through_mysql_json_storage() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS array_value_mysql_playlists", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE array_value_mysql_playlists (\
                 id BIGINT PRIMARY KEY, track_ids JSON NOT NULL, tags JSON NOT NULL, \
                 featured_ids JSON\
             )",
            &[],
        )
        .await?;

    let playlist = Playlist {
        id: 1,
        track_ids: vec![10, 20, 30],
        tags: vec!["rock".to_string(), "live".to_string()],
        featured_ids: Some(vec![10, 30]),
    };
    engine.execute(&playlist.insert()).await?;

    let table = Playlist::table();
    let fetched: Playlist = engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, playlist);

    engine
        .connect()
        .await?
        .execute("DROP TABLE array_value_mysql_playlists", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn empty_and_null_array_fields_round_trip() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute(
            "DROP TABLE IF EXISTS array_value_mysql_empty_playlists",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE array_value_mysql_empty_playlists (\
                 id BIGINT PRIMARY KEY, track_ids JSON NOT NULL, tags JSON NOT NULL, \
                 featured_ids JSON\
             )",
            &[],
        )
        .await?;

    let table = Table::new("array_value_mysql_empty_playlists");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("track_ids", Vec::<i64>::new())
                .value("tags", Vec::<String>::new())
                .value("featured_ids", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(row.get_by_name::<Vec<i64>>("track_ids")?, Vec::<i64>::new());
    assert_eq!(
        row.get_by_name::<Vec<String>>("tags")?,
        Vec::<String>::new()
    );
    assert_eq!(row.get_by_name::<Option<Vec<i64>>>("featured_ids")?, None);

    engine
        .connect()
        .await?
        .execute("DROP TABLE array_value_mysql_empty_playlists", &[])
        .await?;
    Ok(())
}
