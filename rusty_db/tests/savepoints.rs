#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Session::savepoint`/`rollback_to_savepoint`/
//! `release_savepoint`: marking a point inside a session's ongoing
//! transaction that a sub-unit of work can be undone back to, without
//! aborting the whole transaction the way a full `Session::rollback`
//! would.

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
        "rusty_db_savepoints_{name}_{}.sqlite3",
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

async fn all_names(engine: &Engine) -> rusty_db::Result<Vec<String>> {
    let mut rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    rows.sort_by_key(|u| u.id);
    Ok(rows.into_iter().map(|u| u.name).collect())
}

#[tokio::test]
async fn rollback_to_savepoint_undoes_a_sub_unit_of_work_without_aborting_the_transaction(
) -> rusty_db::Result<()> {
    let engine = file_engine("rollback_undoes_sub_unit").await?;
    let mut session = engine.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.flush().await?;

    let sp = session.savepoint().await?;
    session.add(&User {
        id: 2,
        name: "bad-write".to_string(),
    });
    session.flush().await?;
    session.rollback_to_savepoint(&sp).await?;

    session.add(&User {
        id: 3,
        name: "grace".to_string(),
    });
    session.commit().await?;

    assert_eq!(
        all_names(&engine).await?,
        vec!["ada".to_string(), "grace".to_string()],
        "the row added between the savepoint and the rollback should be gone, \
         but everything before and after should have committed normally"
    );

    Ok(())
}

#[tokio::test]
async fn rollback_to_savepoint_discards_still_queued_writes_too() -> rusty_db::Result<()> {
    let engine = file_engine("rollback_discards_queued").await?;
    let mut session = engine.session();
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    let mut session = engine.session();
    let sp = session.savepoint().await?;
    // Queued but never flushed before the rollback.
    session.add(&User {
        id: 2,
        name: "never-lands".to_string(),
    });
    assert_eq!(session.pending_len(), 1);
    session.rollback_to_savepoint(&sp).await?;
    assert_eq!(
        session.pending_len(),
        0,
        "a queued-but-unflushed write since the savepoint should be discarded, not flushed"
    );
    session.commit().await?;

    assert_eq!(all_names(&engine).await?, vec!["ada".to_string()]);

    Ok(())
}

#[tokio::test]
async fn release_savepoint_keeps_its_effects_and_the_transaction_continues() -> rusty_db::Result<()>
{
    let engine = file_engine("release_keeps_effects").await?;
    let mut session = engine.session();

    let sp = session.savepoint().await?;
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.release_savepoint(sp).await?;

    session.add(&User {
        id: 2,
        name: "grace".to_string(),
    });
    session.commit().await?;

    assert_eq!(
        all_names(&engine).await?,
        vec!["ada".to_string(), "grace".to_string()]
    );

    Ok(())
}

#[tokio::test]
async fn nested_savepoints_roll_back_independently() -> rusty_db::Result<()> {
    let engine = file_engine("nested_savepoints").await?;
    let mut session = engine.session();

    let outer = session.savepoint().await?;
    session.add(&User {
        id: 1,
        name: "outer-write".to_string(),
    });
    session.flush().await?;

    let inner = session.savepoint().await?;
    session.add(&User {
        id: 2,
        name: "inner-write".to_string(),
    });
    session.flush().await?;
    session.rollback_to_savepoint(&inner).await?;

    // The outer savepoint's write is still there — only the inner one
    // was undone.
    session.add(&User {
        id: 3,
        name: "after-inner-rollback".to_string(),
    });
    session.release_savepoint(outer).await?;
    session.commit().await?;

    assert_eq!(
        all_names(&engine).await?,
        vec![
            "outer-write".to_string(),
            "after-inner-rollback".to_string()
        ]
    );

    Ok(())
}

#[tokio::test]
async fn an_unreleased_savepoint_is_committed_along_with_the_whole_transaction(
) -> rusty_db::Result<()> {
    let engine = file_engine("unreleased_savepoint_commit").await?;
    let mut session = engine.session();

    let _sp = session.savepoint().await?; // deliberately never released or rolled back to
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    assert_eq!(all_names(&engine).await?, vec!["ada".to_string()]);

    Ok(())
}

#[tokio::test]
async fn a_full_session_rollback_undoes_everything_even_with_an_open_savepoint(
) -> rusty_db::Result<()> {
    let engine = file_engine("full_rollback_with_savepoint").await?;
    let mut session = engine.session();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    let _sp = session.savepoint().await?;
    session.add(&User {
        id: 2,
        name: "grace".to_string(),
    });
    session.rollback().await?;

    assert_eq!(
        all_names(&engine).await?,
        Vec::<String>::new(),
        "a full session rollback undoes everything, savepoint or not"
    );

    Ok(())
}
