#![cfg(all(feature = "mysql", feature = "derive"))]

//! A reduced version of `savepoints.rs` (SQLite) against a real
//! MySQL/MariaDB server — just the two tests that most directly prove
//! `SAVEPOINT`/`ROLLBACK TO SAVEPOINT`/`RELEASE SAVEPOINT` actually work
//! against a real server, since the rest (nested savepoints, discarding
//! still-queued writes, interaction with a full session rollback) is
//! `Session`-level logic that doesn't depend on which driver is
//! underneath and is already covered there.

use rusty_db::prelude::*;

/// Connects to a real MySQL/MariaDB server for this test. There's no way
/// to spin one up portably in every environment this test suite runs in,
/// so this is opt-in: point `MYSQL_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match MySqlDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL test: could not connect to {url}: {err}");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "savepoints_mysql_rollback_users")]
struct RollbackUser {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn rollback_to_savepoint_undoes_a_sub_unit_of_work_without_aborting_the_transaction(
) -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS savepoints_mysql_rollback_users", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE savepoints_mysql_rollback_users (id BIGINT PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;

    let table = RollbackUser::table();
    let mut session = engine.session();
    session.add(&RollbackUser {
        id: 1,
        name: "ada".to_string(),
    });
    session.flush().await?;

    let sp = session.savepoint().await?;
    session.add(&RollbackUser {
        id: 2,
        name: "bad-write".to_string(),
    });
    session.flush().await?;
    session.rollback_to_savepoint(&sp).await?;

    session.add(&RollbackUser {
        id: 3,
        name: "grace".to_string(),
    });
    session.commit().await?;

    let rows: Vec<RollbackUser> = engine.fetch_all_as(&Select::from(&table)).await?;
    assert_eq!(
        rows.into_iter().map(|u| u.name).collect::<Vec<_>>(),
        vec!["ada".to_string(), "grace".to_string()]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE savepoints_mysql_rollback_users", &[])
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "savepoints_mysql_release_users")]
struct ReleaseUser {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn release_savepoint_keeps_its_effects_and_the_transaction_continues() -> rusty_db::Result<()>
{
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS savepoints_mysql_release_users", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE savepoints_mysql_release_users (id BIGINT PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;

    let table = ReleaseUser::table();
    let mut session = engine.session();
    let sp = session.savepoint().await?;
    session.add(&ReleaseUser {
        id: 1,
        name: "ada".to_string(),
    });
    session.release_savepoint(sp).await?;

    session.add(&ReleaseUser {
        id: 2,
        name: "grace".to_string(),
    });
    session.commit().await?;

    let rows: Vec<ReleaseUser> = engine.fetch_all_as(&Select::from(&table)).await?;
    assert_eq!(
        rows.into_iter().map(|u| u.name).collect::<Vec<_>>(),
        vec!["ada".to_string(), "grace".to_string()]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE savepoints_mysql_release_users", &[])
        .await?;
    Ok(())
}
