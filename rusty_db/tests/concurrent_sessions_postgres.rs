#![cfg(all(feature = "postgres", feature = "derive"))]

//! Same coverage as `concurrent_sessions.rs` (SQLite), against a real
//! Postgres server. `Session` is intentionally `!Send` (see its identity
//! map), so these run on `tokio::task::LocalSet` + `spawn_local` rather
//! than `tokio::spawn` — the standard way to get genuinely concurrent,
//! interleaved execution of `!Send` futures on one thread.
//!
//! Each test uses its own table (`#[derive(Mapped)]`'s table name is a
//! compile-time constant, so distinct tests need distinct structs to avoid
//! colliding when cargo runs them concurrently against the same server).

use std::rc::Rc;

use rusty_db::prelude::*;
use tokio::sync::oneshot;
use tokio::task::{spawn_local, LocalSet};

/// Connects to a real PostgreSQL server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `POSTGRES_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| "postgres://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match PostgresDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping Postgres test: could not connect to {url}: {err}");
            None
        }
    }
}

async fn recreate_table(engine: &Engine, table: &str) -> rusty_db::Result<()> {
    engine
        .connect()
        .await?
        .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            &format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, name TEXT NOT NULL)"),
            &[],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "concurrent_users_commits")]
struct UserCommits {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn concurrent_sessions_commit_independently_on_a_shared_pool() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "concurrent_users_commits").await?;
    let local = LocalSet::new();

    local
        .run_until(async {
            let e1 = engine.clone();
            let h1 = spawn_local(async move {
                let mut session = e1.session();
                session.add(&UserCommits {
                    id: 1,
                    name: "ada".to_string(),
                });
                session.commit().await
            });

            let e2 = engine.clone();
            let h2 = spawn_local(async move {
                let mut session = e2.session();
                session.add(&UserCommits {
                    id: 2,
                    name: "grace".to_string(),
                });
                session.commit().await
            });

            h1.await.unwrap()?;
            h2.await.unwrap()
        })
        .await?;

    let mut rows: Vec<UserCommits> = engine
        .fetch_all_as(&Select::from(&UserCommits::table()))
        .await?;
    rows.sort_by_key(|u| u.id);
    assert_eq!(
        rows,
        vec![
            UserCommits {
                id: 1,
                name: "ada".to_string()
            },
            UserCommits {
                id: 2,
                name: "grace".to_string()
            }
        ]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE concurrent_users_commits", &[])
        .await?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "concurrent_users_burst")]
struct UserBurst {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn a_burst_of_concurrent_sessions_all_land_their_writes() -> rusty_db::Result<()> {
    const COUNT: i64 = 10;
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "concurrent_users_burst").await?;
    let local = LocalSet::new();

    local
        .run_until(async {
            let mut handles = Vec::with_capacity(COUNT as usize);
            for id in 0..COUNT {
                let engine = engine.clone();
                handles.push(spawn_local(async move {
                    let mut session = engine.session();
                    session.add(&UserBurst {
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

    let rows: Vec<UserBurst> = engine
        .fetch_all_as(&Select::from(&UserBurst::table()))
        .await?;
    assert_eq!(rows.len(), COUNT as usize);

    engine
        .connect()
        .await?
        .execute("DROP TABLE concurrent_users_burst", &[])
        .await?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "concurrent_users_isolation")]
struct UserIsolation {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn one_sessions_uncommitted_write_is_invisible_to_a_concurrent_reader() -> rusty_db::Result<()>
{
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "concurrent_users_isolation").await?;
    let local = LocalSet::new();

    local
        .run_until(async {
            let (flushed_tx, flushed_rx) = oneshot::channel::<()>();
            let (checked_tx, checked_rx) = oneshot::channel::<()>();

            let writer_engine = engine.clone();
            let writer = spawn_local(async move {
                let mut session = writer_engine.session();
                session.add(&UserIsolation {
                    id: 1,
                    name: "ada".to_string(),
                });
                // Autoflush via get(), without ever calling commit() yet.
                session.get::<UserIsolation>(1_i64).await?;

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
                let seen: Vec<UserIsolation> = reader_engine
                    .fetch_all_as(&Select::from(&UserIsolation::table()))
                    .await
                    .unwrap();
                assert!(seen.is_empty());

                checked_tx.send(()).unwrap();
            });

            reader.await.unwrap();
            writer.await.unwrap()
        })
        .await?;

    let seen: Vec<UserIsolation> = engine
        .fetch_all_as(&Select::from(&UserIsolation::table()))
        .await?;
    assert_eq!(
        seen,
        vec![UserIsolation {
            id: 1,
            name: "ada".to_string()
        }]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE concurrent_users_isolation", &[])
        .await?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "concurrent_users_identity")]
struct UserIdentity {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn concurrent_sessions_have_independent_identity_maps() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "concurrent_users_identity").await?;
    engine
        .execute(
            &Insert::into_table(&UserIdentity::table())
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
                let handle = session.get::<UserIdentity>(1_i64).await.unwrap().unwrap();
                handle.borrow_mut().name = "changed by session a".to_string();
                session_a_mutated_tx.send(()).unwrap();
                // Keep the handle (and session) alive until task B is done checking.
                handle
            });

            let task_b = spawn_local(async move {
                session_a_mutated_rx.await.unwrap();
                let mut session = e2.session();
                let handle = session.get::<UserIdentity>(1_i64).await.unwrap().unwrap();
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

    engine
        .connect()
        .await?
        .execute("DROP TABLE concurrent_users_identity", &[])
        .await?;

    Ok(())
}
