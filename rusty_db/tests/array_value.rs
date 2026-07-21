#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Value::Array`/`Vec<T>` against SQLite, which has no native
//! array column type of its own — every array flattens to a JSON array
//! (stored as `Value::Text`) there, the same treatment `Uuid`/
//! `BigDecimal`/`Json` already get on their own non-native backends.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "playlists")]
struct Playlist {
    #[table(primary_key)]
    id: i64,
    track_ids: Vec<i64>,
    tags: Vec<String>,
    featured_ids: Option<Vec<i64>>,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE playlists (id INTEGER PRIMARY KEY, track_ids TEXT NOT NULL, tags TEXT \
             NOT NULL, featured_ids TEXT)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn array_fields_round_trip_through_a_mapped_struct() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let playlist = Playlist {
        id: 1,
        track_ids: vec![10, 20, 30],
        tags: vec!["rock".to_string(), "live".to_string()],
        featured_ids: Some(vec![10, 30]),
    };
    engine.execute(&playlist.insert()).await?;

    let fetched: Playlist = engine
        .fetch_one_as(
            &Select::from(&Playlist::table()).filter(Playlist::table().col("id").eq(1_i64)),
        )
        .await?;
    assert_eq!(fetched, playlist);

    // SQLite has no native array type: this flattens to Value::Text
    // underneath, as a JSON array.
    let row = engine
        .fetch_one(&Select::from(&Playlist::table()).filter(Playlist::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(row.get_by_name::<String>("track_ids")?, "[10,20,30]");

    Ok(())
}

#[tokio::test]
async fn empty_array_round_trips() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let playlist = Playlist {
        id: 1,
        track_ids: vec![],
        tags: vec![],
        featured_ids: None,
    };
    engine.execute(&playlist.insert()).await?;

    let fetched: Playlist = engine
        .fetch_one_as(
            &Select::from(&Playlist::table()).filter(Playlist::table().col("id").eq(1_i64)),
        )
        .await?;
    assert_eq!(fetched, playlist);

    Ok(())
}

#[tokio::test]
async fn null_array_field_round_trips_as_none() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let playlist = Playlist {
        id: 1,
        track_ids: vec![1],
        tags: vec![],
        featured_ids: None,
    };
    engine.execute(&playlist.insert()).await?;

    let fetched: Playlist = engine
        .fetch_one_as(
            &Select::from(&Playlist::table()).filter(Playlist::table().col("id").eq(1_i64)),
        )
        .await?;
    assert_eq!(fetched.featured_ids, None);

    Ok(())
}

#[tokio::test]
async fn array_field_accepts_raw_json_array_text() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    // Insert via raw text (bypassing this crate's own binding), confirming
    // FromValue parses plain JSON array text directly.
    engine
        .connect()
        .await?
        .execute(
            "INSERT INTO playlists (id, track_ids, tags) VALUES (1, '[1,2,3]', '[\"a\",\"b\"]')",
            &[],
        )
        .await?;

    let rows = engine
        .fetch_all(&Select::from(&Table::new("playlists")))
        .await?;
    assert_eq!(rows[0].get_by_name::<Vec<i64>>("track_ids")?, vec![1, 2, 3]);
    assert_eq!(
        rows[0].get_by_name::<Vec<String>>("tags")?,
        vec!["a".to_string(), "b".to_string()]
    );

    Ok(())
}
