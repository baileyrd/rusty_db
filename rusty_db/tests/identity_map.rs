#![cfg(all(feature = "sqlite", feature = "derive"))]

use std::rc::Rc;

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
    active: bool,
}

async fn engine_with_users() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;
    engine
        .execute(
            &User {
                id: 1,
                name: "ada".to_string(),
                active: true,
            }
            .insert(),
        )
        .await?;
    engine
        .execute(
            &User {
                id: 2,
                name: "grace".to_string(),
                active: true,
            }
            .insert(),
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn get_returns_the_same_instance_on_repeat_lookups() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    let first = session.get::<User>(1_i64).await?.unwrap();
    let second = session.get::<User>(1_i64).await?.unwrap();

    // Same underlying object, not two separately-decoded copies.
    assert!(Rc::ptr_eq(&first, &second));
    assert_eq!(session.identity_map_len(), 1);

    // A different primary key is a distinct cache entry.
    let other = session.get::<User>(2_i64).await?.unwrap();
    assert!(!Rc::ptr_eq(&first, &other));
    assert_eq!(session.identity_map_len(), 2);

    Ok(())
}

#[tokio::test]
async fn get_of_a_missing_row_returns_none_and_caches_nothing() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    assert_eq!(session.get::<User>(999_i64).await?, None);
    assert_eq!(session.identity_map_len(), 0);

    Ok(())
}

#[tokio::test]
async fn mutations_through_one_handle_are_visible_through_another() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    let handle = session.get::<User>(1_i64).await?.unwrap();
    handle.borrow_mut().name = "ada lovelace".to_string();

    // A fresh `get()` call returns the SAME cached (mutated) instance, not
    // a re-decoded row from the database — that's the identity map: this
    // in-memory change was never written back, but the cache still wins.
    let same_handle_again = session.get::<User>(1_i64).await?.unwrap();
    assert_eq!(same_handle_again.borrow().name, "ada lovelace");

    // The database itself is untouched.
    let raw: User = engine
        .fetch_one_as(&Select::from(&User::table()).filter(User::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(raw.name, "ada");

    Ok(())
}

#[tokio::test]
async fn load_all_reuses_cached_handles_for_already_loaded_rows() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    let ada = session.get::<User>(1_i64).await?.unwrap();
    ada.borrow_mut().name = "changed in memory".to_string();

    let all: Vec<Rc<std::cell::RefCell<User>>> = session
        .load_all(&Select::from(&User::table()).order_by(User::table().col("id").asc()))
        .await?;
    assert_eq!(all.len(), 2);

    // Row 1 comes back as the same handle already in the identity map...
    assert!(Rc::ptr_eq(&all[0], &ada));
    assert_eq!(all[0].borrow().name, "changed in memory");
    // ...row 2 is freshly decoded and cached for the first time.
    assert_eq!(all[1].borrow().name, "grace");
    assert_eq!(session.identity_map_len(), 2);

    Ok(())
}

#[tokio::test]
async fn clear_identity_map_forces_a_fresh_decode_next_time() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    let handle = session.get::<User>(1_i64).await?.unwrap();
    handle.borrow_mut().name = "changed in memory".to_string();

    session.clear_identity_map();
    assert_eq!(session.identity_map_len(), 0);

    let refetched = session.get::<User>(1_i64).await?.unwrap();
    assert!(!Rc::ptr_eq(&handle, &refetched));
    assert_eq!(refetched.borrow().name, "ada"); // back to what's actually in the database

    Ok(())
}

#[tokio::test]
async fn delete_evicts_from_the_identity_map_immediately() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    let ada = session.get::<User>(1_i64).await?.unwrap();
    assert_eq!(session.identity_map_len(), 1);

    // Eviction happens right away, before the delete is even flushed.
    session.delete(&*ada.borrow());
    assert_eq!(session.identity_map_len(), 0);
    assert_eq!(session.pending_len(), 1);

    // get() autoflushes the queued delete, then queries fresh -> gone.
    assert_eq!(session.get::<User>(1_i64).await?, None);

    Ok(())
}

#[tokio::test]
async fn delete_of_an_uncached_entity_is_a_harmless_no_op_eviction() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    // Never loaded through this session, so there's nothing to evict —
    // deleting it should not panic or otherwise misbehave.
    let grace = User {
        id: 2,
        name: "grace".to_string(),
        active: true,
    };
    assert_eq!(session.identity_map_len(), 0);
    session.delete(&grace);
    assert_eq!(session.identity_map_len(), 0);

    session.commit().await?;
    assert_eq!(session.get::<User>(2_i64).await?, None);

    Ok(())
}

#[tokio::test]
async fn load_all_reflects_a_deletion_and_does_not_recache_the_stale_row() -> rusty_db::Result<()> {
    let engine = engine_with_users().await?;
    let mut session = engine.session();

    let all = session
        .load_all::<User>(&Select::from(&User::table()).order_by(User::table().col("id").asc()))
        .await?;
    assert_eq!(all.len(), 2);
    assert_eq!(session.identity_map_len(), 2);

    session.delete(&*all[0].borrow()); // ada
    assert_eq!(session.identity_map_len(), 1);

    let remaining = session
        .load_all::<User>(&Select::from(&User::table()))
        .await?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].borrow().name, "grace");
    assert_eq!(session.identity_map_len(), 1);

    Ok(())
}
