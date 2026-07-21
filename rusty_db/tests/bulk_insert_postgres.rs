#![cfg(all(feature = "postgres", feature = "derive"))]

//! Same coverage as `bulk_insert.rs` (SQLite), against a real Postgres
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
            &format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, name TEXT NOT NULL)"),
            &[],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "bulk_insert_pg_round_trip")]
struct RoundTripUser {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn session_add_all_inserts_every_row_in_one_round_trip() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "bulk_insert_pg_round_trip").await?;

    let mut session = engine.session();
    let users = vec![
        RoundTripUser {
            id: 1,
            name: "ada".to_string(),
        },
        RoundTripUser {
            id: 2,
            name: "grace".to_string(),
        },
        RoundTripUser {
            id: 3,
            name: "linus".to_string(),
        },
    ];
    session.add_all(&users);
    assert_eq!(session.pending_len(), 1);
    session.commit().await?;

    let mut rows: Vec<RoundTripUser> = engine
        .fetch_all_as(&Select::from(&RoundTripUser::table()))
        .await?;
    rows.sort_by_key(|u| u.id);
    assert_eq!(rows, users);

    engine
        .connect()
        .await?
        .execute("DROP TABLE bulk_insert_pg_round_trip", &[])
        .await?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "bulk_insert_pg_conflict")]
struct ConflictUser {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn a_failing_bulk_insert_rolls_back_the_whole_batch() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "bulk_insert_pg_conflict").await?;

    let mut session = engine.session();
    session.add(&ConflictUser {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    let mut new_session = engine.session();
    new_session.add_all(&[
        ConflictUser {
            id: 2,
            name: "grace".to_string(),
        },
        ConflictUser {
            id: 1,
            name: "duplicate".to_string(),
        },
    ]);
    let outcome = new_session.commit().await;
    assert!(outcome.is_err(), "expected the bulk insert to fail");

    let rows: Vec<ConflictUser> = engine
        .fetch_all_as(&Select::from(&ConflictUser::table()))
        .await?;
    assert_eq!(
        rows,
        vec![ConflictUser {
            id: 1,
            name: "ada".to_string()
        }]
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE bulk_insert_pg_conflict", &[])
        .await?;

    Ok(())
}
