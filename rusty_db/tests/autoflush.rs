#![cfg(all(feature = "sqlite", feature = "derive"))]

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

/// Two independent connections to the same file-backed database, so we can
/// tell "visible within this session's own transaction" (autoflush) apart
/// from "visible to anyone else" (commit) — an in-memory SQLite database
/// with a single pooled connection can't distinguish those, since there's
/// only one physical connection to begin with.
async fn two_engines_on_the_same_file(name: &str) -> rusty_db::Result<(Engine, Engine)> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_autoflush_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());

    let owner = SqliteDriver::engine(&url).await?;
    owner
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;

    let observer = SqliteDriver::engine(&url).await?;
    Ok((owner, observer))
}

#[tokio::test]
async fn get_autoflushes_pending_writes_from_the_same_session() -> rusty_db::Result<()> {
    let (owner, _observer) = two_engines_on_the_same_file("get_sees_own_writes").await?;
    let mut session = owner.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    assert_eq!(session.pending_len(), 1);

    // No explicit flush()/commit() call — get() autoflushes first.
    let found = session.get::<User>(1_i64).await?;
    assert_eq!(found.unwrap().borrow().name, "ada");
    assert_eq!(session.pending_len(), 0);

    Ok(())
}

#[tokio::test]
async fn load_all_autoflushes_pending_writes_from_the_same_session() -> rusty_db::Result<()> {
    let (owner, _observer) = two_engines_on_the_same_file("load_all_sees_own_writes").await?;
    let mut session = owner.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.add(&User {
        id: 2,
        name: "grace".to_string(),
    });

    let all = session
        .load_all::<User>(&Select::from(&User::table()).order_by(User::table().col("id").asc()))
        .await?;
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].borrow().name, "ada");
    assert_eq!(all[1].borrow().name, "grace");
    assert_eq!(session.pending_len(), 0);

    Ok(())
}

#[tokio::test]
async fn flushed_writes_are_invisible_to_other_connections_until_commit() -> rusty_db::Result<()> {
    let (owner, observer) = two_engines_on_the_same_file("flush_not_commit").await?;
    let mut session = owner.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });

    // Autoflush: the owning session sees it immediately...
    assert!(session.get::<User>(1_i64).await?.is_some());

    // ...but it's only flushed into an open, uncommitted transaction, so a
    // completely separate connection to the same database sees nothing yet.
    let seen_before_commit: Vec<User> =
        observer.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(seen_before_commit.is_empty());

    session.commit().await?;

    // Now that it's committed, the other connection sees it too.
    let seen_after_commit: Vec<User> = observer.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(
        seen_after_commit,
        vec![User {
            id: 1,
            name: "ada".to_string()
        }]
    );

    Ok(())
}

#[tokio::test]
async fn rollback_undoes_flushed_but_uncommitted_writes() -> rusty_db::Result<()> {
    let (owner, observer) = two_engines_on_the_same_file("rollback_undoes_flush").await?;
    let mut session = owner.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    // Autoflush via get() — the write is sent to the database, just not committed.
    assert!(session.get::<User>(1_i64).await?.is_some());

    session.rollback().await?;

    // Nothing was ever committed, so no connection — including a fresh
    // query through the session's own engine — sees it.
    let seen: Vec<User> = observer.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(seen.is_empty());
    let seen_via_owner: Vec<User> = owner.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(seen_via_owner.is_empty());

    Ok(())
}

#[tokio::test]
async fn commit_with_nothing_flushed_is_a_no_op() -> rusty_db::Result<()> {
    let (owner, _observer) = two_engines_on_the_same_file("commit_noop").await?;
    let mut session = owner.session();

    // Never wrote or read anything -> no transaction was ever opened.
    session.commit().await?;
    session.rollback().await?;

    Ok(())
}
