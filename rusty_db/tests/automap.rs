#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Engine::automap_table`/`automap_all`: generating
//! `#[derive(Mapped)]` struct source from live schema reflection.

use rusty_db::prelude::*;

#[tokio::test]
async fn automap_table_generates_source_matching_the_reflected_schema() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, nickname TEXT, \
             active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;

    let source = engine.automap_table("users").await?;
    assert!(source.contains("#[derive(Mapped, Debug, Clone)]"));
    assert!(source.contains("#[table(name = \"users\")]"));
    assert!(source.contains("struct Users {"));
    assert!(source.contains("#[table(primary_key)]"));
    assert!(source.contains("#[table(column = \"id\")]\n    id: i64,"));
    assert!(source.contains("#[table(column = \"name\")]\n    name: String,"));
    assert!(source.contains("#[table(column = \"nickname\")]\n    nickname: Option<String>,"));
    assert!(source.contains("#[table(column = \"active\")]\n    active: bool,"));

    Ok(())
}

#[tokio::test]
async fn automap_all_concatenates_every_table_and_lists_foreign_keys() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER NOT NULL \
             REFERENCES users(id), amount INTEGER NOT NULL)",
            &[],
        )
        .await?;

    let source = engine.automap_all().await?;
    assert!(source.contains("struct Users {"));
    assert!(source.contains("struct Orders {"));
    assert!(source.contains("orders(user_id) -> users(id)"));

    Ok(())
}

#[tokio::test]
async fn automap_table_errors_for_a_table_that_does_not_exist() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let result = engine.automap_table("nonexistent").await;
    assert!(result.is_err());

    Ok(())
}

// A hand-written struct following exactly the shape
// `automap_table_generates_source_matching_the_reflected_schema` above
// asserts `Engine::automap_table` would generate for that same `users`
// table — this is what actually proves the generated pattern is valid,
// working `#[derive(Mapped)]` syntax that round-trips through the real
// derive macro, since compiling the generated *string* itself isn't
// practical inside a test.
#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct Users {
    #[table(primary_key)]
    #[table(column = "id")]
    id: i64,
    #[table(column = "name")]
    name: String,
    #[table(column = "nickname")]
    nickname: Option<String>,
    #[table(column = "active")]
    active: bool,
}

#[tokio::test]
async fn the_generated_shape_actually_round_trips_through_the_real_derive_macro(
) -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, nickname TEXT, \
             active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;

    let ada = Users {
        id: 1,
        name: "ada".to_string(),
        nickname: None,
        active: true,
    };
    engine.execute(&ada.insert()).await?;

    let fetched: Users = engine.fetch_one_as(&Select::from(&Users::table())).await?;
    assert_eq!(fetched, ada);

    Ok(())
}
