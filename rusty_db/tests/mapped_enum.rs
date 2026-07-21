#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `#[derive(MappedEnum)]`: mapping a fieldless enum onto a
//! single column, in both its default text mode and its `as_int` mode.

use rusty_db::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, MappedEnum)]
enum Status {
    Active,
    Inactive,
    #[mapped_enum(rename = "banned_user")]
    Banned,
}

#[derive(Debug, Clone, Copy, PartialEq, MappedEnum)]
#[mapped_enum(as_int)]
enum Priority {
    Low,
    Medium,
    High = 10,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "accounts")]
struct Account {
    #[table(primary_key)]
    id: i64,
    status: Status,
    priority: Priority,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE accounts (id INTEGER PRIMARY KEY, status TEXT NOT NULL, \
             priority INTEGER NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn text_mode_enum_field_round_trips() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let account = Account {
        id: 1,
        status: Status::Banned,
        priority: Priority::Low,
    };
    engine.execute(&account.insert()).await?;

    let fetched: Account = engine
        .fetch_one_as(&Select::from(&Account::table()).filter(Account::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, account);

    // The renamed variant's actual stored text.
    let row = engine
        .fetch_one(&Select::from(&Account::table()).filter(Account::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(
        row.get_by_name::<String>("status")?,
        "banned_user".to_string()
    );

    Ok(())
}

#[tokio::test]
async fn default_snake_case_text_for_unrenamed_variants() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let account = Account {
        id: 1,
        status: Status::Active,
        priority: Priority::Low,
    };
    engine.execute(&account.insert()).await?;

    let row = engine
        .fetch_one(&Select::from(&Account::table()).filter(Account::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(row.get_by_name::<String>("status")?, "active".to_string());

    Ok(())
}

#[tokio::test]
async fn as_int_mode_enum_field_round_trips_using_the_variants_own_discriminant(
) -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let account = Account {
        id: 1,
        status: Status::Active,
        priority: Priority::High,
    };
    engine.execute(&account.insert()).await?;

    let fetched: Account = engine
        .fetch_one_as(&Select::from(&Account::table()).filter(Account::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched.priority, Priority::High);

    let row = engine
        .fetch_one(&Select::from(&Account::table()).filter(Account::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(row.get_by_name::<i64>("priority")?, 10);

    Ok(())
}

#[tokio::test]
async fn unknown_stored_text_reports_a_conversion_error() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    engine
        .execute(
            &Insert::into_table(&Table::new("accounts"))
                .value("id", 1_i64)
                .value("status", "not_a_real_status")
                .value("priority", 0_i64),
        )
        .await?;

    let result: rusty_db::Result<Account> = engine
        .fetch_one_as(&Select::from(&Account::table()).filter(Account::table().col("id").eq(1_i64)))
        .await;
    assert!(result.is_err());

    Ok(())
}
