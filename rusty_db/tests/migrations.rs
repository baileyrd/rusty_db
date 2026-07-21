#![cfg(feature = "sqlite")]

use rusty_db::prelude::*;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "create_users",
        up: &["CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"],
        down: &["DROP TABLE users"],
    },
    Migration {
        version: 2,
        name: "add_users_email",
        up: &["ALTER TABLE users ADD COLUMN email TEXT"],
        down: &[], // irreversible, for this test's purposes
    },
];

#[tokio::test]
async fn up_applies_pending_migrations_in_order_and_is_idempotent() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let migrator = engine.migrator();

    let applied = migrator.up(MIGRATIONS).await?;
    assert_eq!(applied, vec![1, 2]);

    // Both migrations actually ran: the table exists with both columns.
    let users = Table::new("users");
    engine
        .execute(
            &Insert::into_table(&users)
                .value("id", 1_i64)
                .value("name", "ada")
                .value("email", "ada@example.com"),
        )
        .await?;

    let status = migrator.status(MIGRATIONS).await?;
    assert!(status.iter().all(|(_, is_applied)| *is_applied));

    // Nothing pending on a second call.
    let applied_again = migrator.up(MIGRATIONS).await?;
    assert!(applied_again.is_empty());

    Ok(())
}

#[tokio::test]
async fn up_resumes_from_where_a_previous_call_left_off() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let migrator = engine.migrator();

    // Apply just the first migration...
    let first_batch = migrator.up(&MIGRATIONS[..1]).await?;
    assert_eq!(first_batch, vec![1]);

    // ...then a later call with the full list only applies what's new.
    let second_batch = migrator.up(MIGRATIONS).await?;
    assert_eq!(second_batch, vec![2]);

    Ok(())
}

#[tokio::test]
async fn down_reverts_the_last_applied_migration() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let migrator = engine.migrator();
    migrator.up(&MIGRATIONS[..1]).await?;

    let reverted = migrator.down(&MIGRATIONS[..1]).await?;
    assert_eq!(reverted, Some(1));

    // The table is gone, so re-applying migration 1 succeeds again.
    let applied = migrator.up(&MIGRATIONS[..1]).await?;
    assert_eq!(applied, vec![1]);

    // Nothing left applied that isn't in an empty list -> None.
    let engine2 = SqliteDriver::engine("sqlite::memory:").await?;
    assert_eq!(engine2.migrator().down(MIGRATIONS).await?, None);

    Ok(())
}

#[tokio::test]
async fn down_errors_when_the_migration_has_no_down_statements() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let migrator = engine.migrator();
    migrator.up(MIGRATIONS).await?; // migration 2 has an empty `down`

    let result = migrator.down(MIGRATIONS).await;
    assert!(result.is_err());

    Ok(())
}

#[tokio::test]
async fn up_rejects_duplicate_versions() -> rusty_db::Result<()> {
    const DUPLICATES: &[Migration] = &[
        Migration {
            version: 1,
            name: "a",
            up: &["CREATE TABLE a (id INTEGER PRIMARY KEY)"],
            down: &["DROP TABLE a"],
        },
        Migration {
            version: 1,
            name: "b",
            up: &["CREATE TABLE b (id INTEGER PRIMARY KEY)"],
            down: &["DROP TABLE b"],
        },
    ];

    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let result = engine.migrator().up(DUPLICATES).await;
    assert!(result.is_err());

    Ok(())
}
