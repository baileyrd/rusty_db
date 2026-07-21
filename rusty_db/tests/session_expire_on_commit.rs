#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Session::with_expire_on_commit`: clearing the identity map
//! after every successful commit, so the next `get`/`load_all`/`query`
//! for a row re-fetches it fresh instead of returning a cached, possibly
//! now-stale in-memory handle.

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
        "rusty_db_session_expire_on_commit_{name}_{}.sqlite3",
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
async fn with_expire_on_commit_clears_the_identity_map_after_a_successful_commit(
) -> rusty_db::Result<()> {
    let engine = file_engine("clears_on_commit").await?;
    let mut session = engine.session().with_expire_on_commit();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    // `get` alone doesn't open a transaction, so this by itself has
    // nothing for the *next* commit to expire yet.
    session.get::<User>(1_i64).await?.expect("ada exists");
    assert_eq!(session.identity_map_len(), 1);

    // A real write, so the next commit() actually has a transaction to
    // commit (and therefore something to expire).
    session.add(&User {
        id: 2,
        name: "grace".to_string(),
    });
    session.commit().await?;

    assert_eq!(
        session.identity_map_len(),
        0,
        "with_expire_on_commit should clear the whole identity map after a real commit"
    );

    Ok(())
}

#[tokio::test]
async fn without_expire_on_commit_the_identity_map_is_unaffected_by_commit() -> rusty_db::Result<()>
{
    let engine = file_engine("unaffected_by_default").await?;
    let mut session = engine.session(); // no with_expire_on_commit

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    session.get::<User>(1_i64).await?.expect("ada exists");
    assert_eq!(session.identity_map_len(), 1);

    session.add(&User {
        id: 2,
        name: "grace".to_string(),
    });
    session.commit().await?;

    assert_eq!(
        session.identity_map_len(),
        1,
        "without with_expire_on_commit, the identity map is untouched by commit()"
    );

    Ok(())
}

#[tokio::test]
async fn expire_on_commit_does_not_fire_when_commit_has_nothing_to_commit() -> rusty_db::Result<()>
{
    let engine = file_engine("no_op_commit_does_not_expire").await?;
    let mut session = engine.session().with_expire_on_commit();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    session.get::<User>(1_i64).await?.expect("ada exists");
    assert_eq!(session.identity_map_len(), 1);

    // Nothing queued, and `get` alone never opened a transaction, so this
    // commit() is a genuine no-op — nothing to expire.
    session.commit().await?;
    assert_eq!(
        session.identity_map_len(),
        1,
        "a no-op commit (nothing was ever flushed or read into a transaction) \
         shouldn't expire the identity map"
    );

    Ok(())
}

#[tokio::test]
async fn after_expire_on_commit_a_fresh_get_reflects_the_actual_committed_state(
) -> rusty_db::Result<()> {
    let engine = file_engine("fresh_get_reflects_reality").await?;
    let mut session = engine.session().with_expire_on_commit();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    let ada = session.get::<User>(1_i64).await?.expect("ada exists");
    ada.borrow_mut().name = "not-yet-saved".to_string(); // in-memory only

    // Someone else changes the row directly, bypassing this session
    // entirely (a different connection, in spirit — a plain `Engine`
    // write, not through `session`).
    let table = User::table();
    engine
        .execute(
            &Update::table(&table)
                .set("name", "changed-externally")
                .filter(table.col("id").eq(1_i64)),
        )
        .await?;

    // A real write, so the commit actually has a transaction (and
    // therefore expiration) to go with it.
    session.add(&User {
        id: 2,
        name: "grace".to_string(),
    });
    session.commit().await?;

    let refetched = session.get::<User>(1_i64).await?.expect("still exists");
    assert_eq!(
        refetched.borrow().name,
        "changed-externally",
        "a fresh fetch after expiration should reflect the database's actual state, \
         not the stale in-memory edit"
    );
    assert!(
        !Rc::ptr_eq(&ada, &refetched),
        "expiration should hand back a genuinely fresh handle, not the old cached one"
    );

    Ok(())
}
