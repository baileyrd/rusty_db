#![cfg(all(feature = "sqlite", feature = "derive"))]

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "create_users",
    up: &["CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"],
    down: &["DROP TABLE users"],
}];

const BROKEN_MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "broken",
    up: &["THIS IS NOT VALID SQL"],
    down: &[],
}];

/// Two independent connections to the same file-backed database — an
/// in-memory SQLite database's single pooled connection can't distinguish
/// "flushed into an open transaction" from "committed", since there's only
/// one physical connection to begin with.
async fn two_engines_on_the_same_file(name: &str) -> rusty_db::Result<(Engine, Engine)> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_session_migrations_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    Ok((
        SqliteDriver::engine(&url).await?,
        SqliteDriver::engine(&url).await?,
    ))
}

async fn table_exists(engine: &Engine, table: &str) -> rusty_db::Result<bool> {
    let rows = engine
        .connect()
        .await?
        .fetch_all(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
            &[Value::Text(table.to_string())],
        )
        .await?;
    Ok(!rows.is_empty())
}

#[tokio::test]
async fn migrate_is_invisible_to_other_connections_until_commit() -> rusty_db::Result<()> {
    let (owner, observer) = two_engines_on_the_same_file("invisible_until_commit").await?;
    let mut session = owner.session();

    let applied = session.migrate(MIGRATIONS).await?;
    assert_eq!(applied, vec![1]);

    // Flushed into the session's own transaction, but not committed: a
    // separate connection to the same database sees neither the new table
    // nor the bookkeeping row.
    assert!(!table_exists(&observer, "users").await?);
    assert!(!table_exists(&observer, "_rusty_db_migrations").await?);

    // Calling migrate() again within the same still-open transaction sees
    // its own prior write and correctly reports nothing new to apply.
    assert_eq!(session.migrate(MIGRATIONS).await?, Vec::<i64>::new());

    session.commit().await?;

    assert!(table_exists(&observer, "users").await?);
    assert!(table_exists(&observer, "_rusty_db_migrations").await?);

    Ok(())
}

#[tokio::test]
async fn migrate_and_a_regular_write_commit_together() -> rusty_db::Result<()> {
    let (owner, observer) = two_engines_on_the_same_file("commit_together").await?;
    let mut session = owner.session();

    session.migrate(MIGRATIONS).await?;
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });

    // Neither the schema change nor the data write is visible yet.
    assert!(!table_exists(&observer, "users").await?);

    session.commit().await?;

    assert!(table_exists(&observer, "users").await?);
    let rows: Vec<User> = observer.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(
        rows,
        vec![User {
            id: 1,
            name: "ada".to_string()
        }]
    );

    Ok(())
}

#[tokio::test]
async fn rollback_undoes_a_migration_and_a_write_together() -> rusty_db::Result<()> {
    let (owner, observer) = two_engines_on_the_same_file("rollback_together").await?;
    let mut session = owner.session();

    session.migrate(MIGRATIONS).await?;
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.rollback().await?;

    assert!(!table_exists(&observer, "users").await?);
    assert!(!table_exists(&observer, "_rusty_db_migrations").await?);

    Ok(())
}

#[tokio::test]
async fn migrate_failure_rolls_back_the_whole_session_transaction() -> rusty_db::Result<()> {
    let (owner, observer) = two_engines_on_the_same_file("migrate_failure").await?;
    let mut session = owner.session();

    let result = session.migrate(BROKEN_MIGRATIONS).await;
    assert!(result.is_err());

    // Even the bookkeeping table's own creation (which succeeded before the
    // broken migration's statement failed) was rolled back.
    assert!(!table_exists(&observer, "_rusty_db_migrations").await?);

    // The session is left usable: a later, valid migrate() starts a fresh
    // transaction rather than reusing whatever was left of the broken one.
    session.migrate(MIGRATIONS).await?;
    session.commit().await?;
    assert!(table_exists(&observer, "users").await?);

    Ok(())
}
