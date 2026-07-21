#![cfg(all(feature = "postgres", feature = "derive"))]

//! Same coverage as `optimistic_locking.rs` (SQLite), against a real
//! Postgres server. Each test uses its own table (`#[derive(Mapped)]`'s
//! table name is a compile-time constant, so distinct tests need
//! distinct structs to avoid colliding when cargo runs them concurrently
//! against the same server).

use rusty_db::prelude::*;
use rusty_db::Error;

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
                "CREATE TABLE {table} (id BIGINT PRIMARY KEY, version BIGINT NOT NULL, title TEXT NOT NULL)"
            ),
            &[],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "optimistic_locking_pg_update")]
struct UpdateDocument {
    #[table(primary_key)]
    id: i64,
    #[table(version)]
    version: i64,
    title: String,
}

#[tokio::test]
async fn updating_with_a_stale_version_fails_with_conflict() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "optimistic_locking_pg_update").await?;

    let mut session = engine.session();
    session.add(&UpdateDocument {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    session.update(&UpdateDocument {
        id: 1,
        version: 1,
        title: "edited by someone else".to_string(),
    });
    session.commit().await?;

    session.update(&UpdateDocument {
        id: 1,
        version: 1,
        title: "clobbering edit".to_string(),
    });
    let outcome = session.commit().await;
    assert!(
        matches!(outcome, Err(Error::Conflict(_))),
        "expected a conflict, got {outcome:?}"
    );

    let table = UpdateDocument::table();
    let stored: UpdateDocument = engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(
        stored,
        UpdateDocument {
            id: 1,
            version: 2,
            title: "edited by someone else".to_string()
        }
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE optimistic_locking_pg_update", &[])
        .await?;

    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "optimistic_locking_pg_delete")]
struct DeleteDocument {
    #[table(primary_key)]
    id: i64,
    #[table(version)]
    version: i64,
    title: String,
}

#[tokio::test]
async fn deleting_with_a_stale_version_fails_with_conflict() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "optimistic_locking_pg_delete").await?;

    let mut session = engine.session();
    session.add(&DeleteDocument {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    session.update(&DeleteDocument {
        id: 1,
        version: 1,
        title: "edited".to_string(),
    });
    session.commit().await?;

    session.delete(&DeleteDocument {
        id: 1,
        version: 1,
        title: "edited".to_string(),
    });
    let outcome = session.commit().await;
    assert!(matches!(outcome, Err(Error::Conflict(_))));

    let table = DeleteDocument::table();
    let rows: Vec<DeleteDocument> = engine.fetch_all_as(&Select::from(&table)).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].version, 2);

    engine
        .connect()
        .await?
        .execute("DROP TABLE optimistic_locking_pg_delete", &[])
        .await?;

    Ok(())
}
