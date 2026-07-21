#![cfg(all(feature = "postgres", feature = "derive"))]

//! Same coverage as `soft_delete.rs` (SQLite), against a real Postgres
//! server. Each test uses its own table (`#[derive(Mapped)]`'s table
//! name is a compile-time constant, so distinct tests need distinct
//! structs to avoid colliding when cargo runs them concurrently against
//! the same server).

use rusty_db::prelude::*;

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
            &format!(
                "CREATE TABLE {table} (id BIGINT PRIMARY KEY, deleted BOOLEAN NOT NULL, name TEXT NOT NULL)"
            ),
            &[],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "soft_delete_pg_marks_users")]
struct MarksUser {
    #[table(primary_key)]
    id: i64,
    #[table(soft_delete)]
    deleted: bool,
    name: String,
}

#[tokio::test]
async fn deleting_marks_the_row_instead_of_removing_it() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "soft_delete_pg_marks_users").await?;

    let entity = MarksUser {
        id: 1,
        deleted: false,
        name: "ada".to_string(),
    };
    let mut session = engine.session();
    session.add(&entity);
    session.commit().await?;

    session.delete(&entity);
    session.commit().await?;

    let rows: Vec<MarksUser> = engine
        .fetch_all_as(&Select::from(&MarksUser::table()))
        .await?;
    assert_eq!(
        rows,
        vec![MarksUser {
            id: 1,
            deleted: true,
            name: "ada".to_string()
        }]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE soft_delete_pg_marks_users", &[])
        .await?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "soft_delete_pg_get_users")]
struct GetUser {
    #[table(primary_key)]
    id: i64,
    #[table(soft_delete)]
    deleted: bool,
    name: String,
}

#[tokio::test]
async fn get_treats_a_soft_deleted_row_as_not_found() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "soft_delete_pg_get_users").await?;

    let entity = GetUser {
        id: 1,
        deleted: false,
        name: "ada".to_string(),
    };
    let mut session = engine.session();
    session.add(&entity);
    session.commit().await?;
    assert!(session.get::<GetUser>(1_i64).await?.is_some());

    session.delete(&entity);
    session.commit().await?;

    let mut fresh_session = engine.session();
    assert!(fresh_session.get::<GetUser>(1_i64).await?.is_none());

    engine
        .connect()
        .await?
        .execute("DROP TABLE soft_delete_pg_get_users", &[])
        .await?;

    Ok(())
}
