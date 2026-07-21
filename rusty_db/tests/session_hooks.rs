#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Session::on_before_flush`/`on_after_flush`/
//! `on_before_commit`/`on_after_commit`/`on_after_rollback`: registering
//! plain callbacks that fire at specific points in a session's
//! flush/commit/rollback lifecycle.

use std::cell::RefCell;
use std::rc::Rc;

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_session_hooks_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn before_flush_fires_only_when_something_is_pending() -> rusty_db::Result<()> {
    let engine = file_engine("before_flush_only_when_pending").await?;
    let mut session = engine.session();

    let count = Rc::new(RefCell::new(0));
    let counted = count.clone();
    session.on_before_flush(move || *counted.borrow_mut() += 1);

    session.flush().await?; // nothing pending: shouldn't fire
    assert_eq!(*count.borrow(), 0);

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.flush().await?; // something pending: should fire once
    assert_eq!(*count.borrow(), 1);

    Ok(())
}

#[tokio::test]
async fn after_flush_fires_after_a_successful_flush_not_before() -> rusty_db::Result<()> {
    let engine = file_engine("after_flush_fires_after").await?;
    let mut session = engine.session();

    let log = Rc::new(RefCell::new(Vec::<&'static str>::new()));
    let before_log = log.clone();
    session.on_before_flush(move || before_log.borrow_mut().push("before"));
    let after_log = log.clone();
    session.on_after_flush(move || after_log.borrow_mut().push("after"));

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.flush().await?;

    assert_eq!(*log.borrow(), vec!["before", "after"]);

    Ok(())
}

#[tokio::test]
async fn after_flush_does_not_fire_on_a_failing_flush() -> rusty_db::Result<()> {
    let engine = file_engine("after_flush_skips_on_failure").await?;
    let mut session = engine.session();
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    let mut new_session = engine.session();
    let after_flush_count = Rc::new(RefCell::new(0));
    let counted = after_flush_count.clone();
    new_session.on_after_flush(move || *counted.borrow_mut() += 1);

    // Duplicate primary key: this flush will fail.
    new_session.add(&User {
        id: 1,
        name: "duplicate".to_string(),
    });
    let outcome = new_session.flush().await;
    assert!(outcome.is_err());
    assert_eq!(
        *after_flush_count.borrow(),
        0,
        "on_after_flush should not fire when the flush itself failed"
    );

    Ok(())
}

#[tokio::test]
async fn before_commit_fires_every_time_even_with_nothing_pending() -> rusty_db::Result<()> {
    let engine = file_engine("before_commit_always_fires").await?;
    let mut session = engine.session();

    let count = Rc::new(RefCell::new(0));
    let counted = count.clone();
    session.on_before_commit(move || *counted.borrow_mut() += 1);

    session.commit().await?;
    session.commit().await?;
    assert_eq!(
        *count.borrow(),
        2,
        "on_before_commit fires on every commit() call, pending or not"
    );

    Ok(())
}

#[tokio::test]
async fn after_commit_fires_only_when_a_transaction_was_actually_committed() -> rusty_db::Result<()>
{
    let engine = file_engine("after_commit_only_with_real_txn").await?;
    let mut session = engine.session();

    let count = Rc::new(RefCell::new(0));
    let counted = count.clone();
    session.on_after_commit(move || *counted.borrow_mut() += 1);

    // Nothing was ever flushed or read, so no transaction was ever opened.
    session.commit().await?;
    assert_eq!(
        *count.borrow(),
        0,
        "on_after_commit should not fire when commit() had no transaction to commit"
    );

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;
    assert_eq!(*count.borrow(), 1);

    Ok(())
}

#[tokio::test]
async fn after_rollback_fires_only_when_a_transaction_was_actually_rolled_back(
) -> rusty_db::Result<()> {
    let engine = file_engine("after_rollback_only_with_real_txn").await?;
    let mut session = engine.session();

    let count = Rc::new(RefCell::new(0));
    let counted = count.clone();
    session.on_after_rollback(move || *counted.borrow_mut() += 1);

    // No transaction ever opened: rollback() is a no-op.
    session.rollback().await?;
    assert_eq!(*count.borrow(), 0);

    // A transaction only actually begins once something is flushed —
    // queuing a write with `add` alone doesn't open one yet.
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.flush().await?;
    session.rollback().await?;
    assert_eq!(*count.borrow(), 1);

    Ok(())
}

#[tokio::test]
async fn hooks_run_in_registration_order() -> rusty_db::Result<()> {
    let engine = file_engine("hooks_run_in_order").await?;
    let mut session = engine.session();

    let log = Rc::new(RefCell::new(Vec::<&'static str>::new()));
    let first = log.clone();
    session.on_before_flush(move || first.borrow_mut().push("first"));
    let second = log.clone();
    session.on_before_flush(move || second.borrow_mut().push("second"));

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.flush().await?;

    assert_eq!(*log.borrow(), vec!["first", "second"]);

    Ok(())
}
