#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `#[derive(MappedNewtype)]`: mapping a single-field tuple
//! struct onto whatever `Value` its own field already converts to/from.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, MappedNewtype)]
struct Email(String);

#[derive(Debug, Clone, Copy, PartialEq, MappedNewtype)]
struct Age(i64);

// A newtype wrapping a `MappedEnum` composes with it directly.
#[derive(Debug, Clone, Copy, PartialEq, MappedEnum)]
enum Tier {
    Free,
    Paid,
}

#[derive(Debug, Clone, Copy, PartialEq, MappedNewtype)]
struct AccountTier(Tier);

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    email: Email,
    age: Option<Age>,
    tier: AccountTier,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL, age INTEGER, \
             tier TEXT NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn newtype_fields_round_trip_through_their_inner_values_own_conversion(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let user = User {
        id: 1,
        email: Email("alice@example.com".to_string()),
        age: Some(Age(30)),
        tier: AccountTier(Tier::Paid),
    };
    engine.execute(&user.insert()).await?;

    let fetched: User = engine
        .fetch_one_as(&Select::from(&User::table()).filter(User::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, user);

    // Storage form matches the wrapped type's own: TEXT for String, plain
    // integer for i64, and (via the composed MappedEnum) TEXT for Tier.
    let row = engine
        .fetch_one(&Select::from(&User::table()).filter(User::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(
        row.get_by_name::<String>("email")?,
        "alice@example.com".to_string()
    );
    assert_eq!(row.get_by_name::<i64>("age")?, 30);
    assert_eq!(row.get_by_name::<String>("tier")?, "paid".to_string());

    Ok(())
}

#[tokio::test]
async fn none_newtype_field_stores_and_reads_back_as_null() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let user = User {
        id: 1,
        email: Email("bob@example.com".to_string()),
        age: None,
        tier: AccountTier(Tier::Free),
    };
    engine.execute(&user.insert()).await?;

    let fetched: User = engine
        .fetch_one_as(&Select::from(&User::table()).filter(User::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched.age, None);

    Ok(())
}
