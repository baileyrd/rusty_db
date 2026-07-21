#![cfg(feature = "postgres")]

//! A reduced version of `query_builder_extras.rs` (SQLite) against a real
//! Postgres server — just the two things that are actually
//! Postgres-specific: `ILIKE` as a native keyword (vs. SQLite's fallback to
//! plain `LIKE`), and `RETURNING` on `UPDATE`/`DELETE` (SQLite's dialect
//! doesn't support `RETURNING` at all, so there's nothing to prove there).
//! `DISTINCT`/`BETWEEN` have no dialect-specific behavior and are already
//! covered against a real SQL engine there.

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

#[tokio::test]
async fn ilike_uses_the_native_keyword_and_matches_case_insensitively() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS query_extras_pg_ilike", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE query_extras_pg_ilike (id BIGINT PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;

    let users = Table::new("query_extras_pg_ilike");
    for (id, name) in [(1_i64, "Ada"), (2, "grace")] {
        engine
            .execute(
                &Insert::into_table(&users)
                    .value("id", id)
                    .value("name", name),
            )
            .await?;
    }

    let rows = engine
        .fetch_all(
            &Select::from(&users)
                .columns([users.col("id")])
                .filter(users.col("name").ilike("ADA")),
        )
        .await?;
    assert_eq!(
        rows.len(),
        1,
        "ILIKE should match \"Ada\" case-insensitively"
    );
    assert_eq!(rows[0].get::<i64>(0)?, 1);

    engine
        .connect()
        .await?
        .execute("DROP TABLE query_extras_pg_ilike", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn returning_is_honored_on_update_and_delete() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS query_extras_pg_returning", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE query_extras_pg_returning (id BIGINT PRIMARY KEY, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;

    let users = Table::new("query_extras_pg_returning");
    engine
        .execute(
            &Insert::into_table(&users)
                .value("id", 1_i64)
                .value("active", false),
        )
        .await?;

    let updated = engine
        .fetch_one(
            &Update::table(&users)
                .set("active", true)
                .filter(users.col("id").eq(1_i64))
                .returning(["id", "active"]),
        )
        .await?;
    assert_eq!(updated.get_by_name::<i64>("id")?, 1);
    assert!(updated.get_by_name::<bool>("active")?);

    let deleted = engine
        .fetch_one(
            &Delete::from(&users)
                .filter(users.col("id").eq(1_i64))
                .returning(["id"]),
        )
        .await?;
    assert_eq!(deleted.get_by_name::<i64>("id")?, 1);

    let remaining = engine.fetch_all(&Select::from(&users)).await?;
    assert!(remaining.is_empty(), "the row should be genuinely deleted");

    engine
        .connect()
        .await?
        .execute("DROP TABLE query_extras_pg_returning", &[])
        .await?;
    Ok(())
}
