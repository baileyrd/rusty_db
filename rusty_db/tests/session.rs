#![cfg(all(feature = "sqlite", feature = "derive"))]

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
    active: bool,
}

async fn engine_with_users_table() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn session_batches_writes_into_one_transaction() -> rusty_db::Result<()> {
    let engine = engine_with_users_table().await?;
    let mut session = engine.session();

    let ada = User {
        id: 1,
        name: "ada".to_string(),
        active: true,
    };
    let grace = User {
        id: 2,
        name: "grace".to_string(),
        active: false,
    };
    session.add(&ada);
    session.add(&grace);
    assert_eq!(session.pending_len(), 2);

    // Nothing hits the database until commit().
    let before: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(before.is_empty());

    session.commit().await?;
    assert_eq!(session.pending_len(), 0);

    let after: Vec<User> = engine
        .fetch_all_as(&Select::from(&User::table()).order_by(User::table().col("id").asc()))
        .await?;
    assert_eq!(after, vec![ada.clone(), grace.clone()]);

    // Queue an update and a delete together; both commit atomically.
    let mut promoted = grace.clone();
    promoted.active = true;
    session.update(&promoted);
    session.delete(&ada);
    session.commit().await?;

    let remaining: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(remaining, vec![promoted]);

    Ok(())
}

#[tokio::test]
async fn session_rollback_discards_pending_writes() -> rusty_db::Result<()> {
    let engine = engine_with_users_table().await?;
    let mut session = engine.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
        active: true,
    });
    session.rollback().await?;
    assert_eq!(session.pending_len(), 0);

    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(rows.is_empty());

    Ok(())
}

#[tokio::test]
async fn session_commit_failure_rolls_back_and_keeps_the_queue() -> rusty_db::Result<()> {
    let engine = engine_with_users_table().await?;
    let mut session = engine.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
        active: true,
    });
    // Same primary key as above -> violates the PRIMARY KEY constraint.
    session.add(&User {
        id: 1,
        name: "duplicate".to_string(),
        active: true,
    });
    assert_eq!(session.pending_len(), 2);

    let result = session.commit().await;
    assert!(result.is_err());

    // The whole transaction rolled back, including the first (valid) insert.
    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(rows.is_empty());

    // The queue is untouched, so the caller can fix the issue and retry.
    assert_eq!(session.pending_len(), 2);

    Ok(())
}
