#![cfg(all(feature = "sqlite", feature = "derive"))]

//! `Session` is intentionally `!Send` (see its identity map), so these
//! don't run on `tokio::spawn` — they run on `tokio::task::LocalSet` +
//! `spawn_local`, which is the standard way to get genuinely concurrent
//! (cooperatively-scheduled, interleaved) execution of `!Send` futures on
//! one thread. Multiple `Session`s built from the same cloned `Engine`
//! share one connection pool, exactly like multiple request handlers in a
//! real application would.

use std::rc::Rc;

use rusty_db::prelude::*;
use tokio::sync::oneshot;
use tokio::task::{spawn_local, LocalSet};

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

/// A file-backed database (not `:memory:`, whose pool is forced to a
/// single connection) so multiple sessions can each hold their own
/// connection at the same time, like they would in a real deployment.
async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_concurrent_sessions_{name}_{}.sqlite3",
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
async fn concurrent_sessions_commit_independently_on_a_shared_pool() -> rusty_db::Result<()> {
    let engine = file_engine("independent_commits").await?;
    let local = LocalSet::new();

    local
        .run_until(async {
            let e1 = engine.clone();
            let h1 = spawn_local(async move {
                let mut session = e1.session();
                session.add(&User {
                    id: 1,
                    name: "ada".to_string(),
                });
                session.commit().await
            });

            let e2 = engine.clone();
            let h2 = spawn_local(async move {
                let mut session = e2.session();
                session.add(&User {
                    id: 2,
                    name: "grace".to_string(),
                });
                session.commit().await
            });

            h1.await.unwrap()?;
            h2.await.unwrap()
        })
        .await?;

    let mut rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    rows.sort_by_key(|u| u.id);
    assert_eq!(
        rows,
        vec![
            User {
                id: 1,
                name: "ada".to_string()
            },
            User {
                id: 2,
                name: "grace".to_string()
            }
        ]
    );

    Ok(())
}

#[tokio::test]
async fn a_burst_of_concurrent_sessions_all_land_their_writes() -> rusty_db::Result<()> {
    const COUNT: i64 = 10;
    let engine = file_engine("burst").await?;
    let local = LocalSet::new();

    local
        .run_until(async {
            let mut handles = Vec::with_capacity(COUNT as usize);
            for id in 0..COUNT {
                let engine = engine.clone();
                handles.push(spawn_local(async move {
                    let mut session = engine.session();
                    session.add(&User {
                        id,
                        name: format!("user-{id}"),
                    });
                    session.commit().await
                }));
            }
            for handle in handles {
                handle.await.unwrap()?;
            }
            rusty_db::Result::Ok(())
        })
        .await?;

    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(rows.len(), COUNT as usize);

    Ok(())
}

#[tokio::test]
async fn one_sessions_uncommitted_write_is_invisible_to_a_concurrent_reader() -> rusty_db::Result<()>
{
    let engine = file_engine("concurrent_isolation").await?;
    let local = LocalSet::new();

    local
        .run_until(async {
            let (flushed_tx, flushed_rx) = oneshot::channel::<()>();
            let (checked_tx, checked_rx) = oneshot::channel::<()>();

            let writer_engine = engine.clone();
            let writer = spawn_local(async move {
                let mut session = writer_engine.session();
                session.add(&User {
                    id: 1,
                    name: "ada".to_string(),
                });
                // Autoflush via get(), without ever calling commit() yet.
                session.get::<User>(1_i64).await?;

                flushed_tx.send(()).unwrap();
                checked_rx.await.unwrap();

                session.commit().await
            });

            let reader_engine = engine.clone();
            let reader = spawn_local(async move {
                flushed_rx.await.unwrap();

                // A concurrent, completely independent connection sees
                // nothing: the write is flushed into the writer's open
                // transaction, not committed.
                let seen: Vec<User> = reader_engine
                    .fetch_all_as(&Select::from(&User::table()))
                    .await
                    .unwrap();
                assert!(seen.is_empty());

                checked_tx.send(()).unwrap();
            });

            reader.await.unwrap();
            writer.await.unwrap()
        })
        .await?;

    let seen: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(
        seen,
        vec![User {
            id: 1,
            name: "ada".to_string()
        }]
    );

    Ok(())
}

#[tokio::test]
async fn concurrent_sessions_have_independent_identity_maps() -> rusty_db::Result<()> {
    let engine = file_engine("independent_identity_maps").await?;
    engine
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 1_i64)
                .value("name", "ada"),
        )
        .await?;

    let local = LocalSet::new();
    let (session_a_mutated_tx, session_a_mutated_rx) = oneshot::channel::<()>();

    let e1 = engine.clone();
    let e2 = engine.clone();

    local
        .run_until(async move {
            let task_a = spawn_local(async move {
                let mut session = e1.session();
                let handle = session.get::<User>(1_i64).await.unwrap().unwrap();
                handle.borrow_mut().name = "changed by session a".to_string();
                session_a_mutated_tx.send(()).unwrap();
                // Keep the handle (and session) alive until task B is done checking.
                handle
            });

            let task_b = spawn_local(async move {
                session_a_mutated_rx.await.unwrap();
                let mut session = e2.session();
                let handle = session.get::<User>(1_i64).await.unwrap().unwrap();
                // Session B's own identity map decoded this fresh from the
                // database — it has no way to see session A's in-memory-only
                // mutation, since the two sessions never share any state.
                let name = handle.borrow().name.clone();
                name
            });

            let name_seen_by_b = task_b.await.unwrap();
            let handle_a = task_a.await.unwrap();

            assert_eq!(name_seen_by_b, "ada");
            assert_eq!(handle_a.borrow().name, "changed by session a");
            assert_eq!(Rc::strong_count(&handle_a), 1);
        })
        .await;

    Ok(())
}
