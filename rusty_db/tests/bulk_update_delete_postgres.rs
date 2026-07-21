#![cfg(all(feature = "postgres", feature = "derive"))]

//! A reduced version of `bulk_update_delete.rs` (SQLite) against a real
//! Postgres server — just the two round-trip tests, since the
//! identity-map-bypass/audit-log/rollback behavior is `Session`-level
//! logic that doesn't depend on which driver is underneath and is already
//! covered there. Each test uses its own table (`#[derive(Mapped)]`'s
//! table name is a compile-time constant, so distinct tests need distinct
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

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "bulk_update_delete_pg_update_users")]
struct UpdateUser {
    #[table(primary_key)]
    id: i64,
    name: String,
    active: bool,
}

#[tokio::test]
async fn bulk_update_changes_every_matching_row_in_one_statement() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute(
            "DROP TABLE IF EXISTS bulk_update_delete_pg_update_users",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE bulk_update_delete_pg_update_users \
                 (id BIGINT PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;

    let mut session = engine.session();
    session.add_all(&[
        UpdateUser {
            id: 1,
            name: "ada".to_string(),
            active: true,
        },
        UpdateUser {
            id: 2,
            name: "grace".to_string(),
            active: true,
        },
        UpdateUser {
            id: 3,
            name: "linus".to_string(),
            active: false,
        },
    ]);
    session.commit().await?;

    let table = UpdateUser::table();
    session.bulk_update::<UpdateUser>(
        Update::table(&table)
            .set("active", false)
            .filter(table.col("active").eq(true)),
    );
    session.commit().await?;

    let users: Vec<UpdateUser> = engine.fetch_all_as(&Select::from(&table)).await?;
    assert!(users.iter().all(|u| !u.active));

    engine
        .connect()
        .await?
        .execute("DROP TABLE bulk_update_delete_pg_update_users", &[])
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "bulk_update_delete_pg_delete_users")]
struct DeleteUser {
    #[table(primary_key)]
    id: i64,
    name: String,
    active: bool,
}

#[tokio::test]
async fn bulk_delete_removes_every_matching_row_in_one_statement() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute(
            "DROP TABLE IF EXISTS bulk_update_delete_pg_delete_users",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE bulk_update_delete_pg_delete_users \
                 (id BIGINT PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;

    let mut session = engine.session();
    session.add_all(&[
        DeleteUser {
            id: 1,
            name: "ada".to_string(),
            active: true,
        },
        DeleteUser {
            id: 2,
            name: "grace".to_string(),
            active: true,
        },
        DeleteUser {
            id: 3,
            name: "linus".to_string(),
            active: false,
        },
    ]);
    session.commit().await?;

    let table = DeleteUser::table();
    session.bulk_delete::<DeleteUser>(Delete::from(&table).filter(table.col("active").eq(true)));
    session.commit().await?;

    let users: Vec<DeleteUser> = engine.fetch_all_as(&Select::from(&table)).await?;
    assert_eq!(
        users,
        vec![DeleteUser {
            id: 3,
            name: "linus".to_string(),
            active: false,
        }]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE bulk_update_delete_pg_delete_users", &[])
        .await?;
    Ok(())
}
