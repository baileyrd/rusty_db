#![cfg(feature = "postgres")]

//! A reduced version of `query_builder_extras.rs` (SQLite) against a real
//! Postgres server — just the things that are actually Postgres-specific:
//! `ILIKE` as a native keyword (vs. SQLite's fallback to plain `LIKE`),
//! `RETURNING` on `UPDATE`/`DELETE` (SQLite's dialect doesn't support
//! `RETURNING` at all, so there's nothing to prove there), and a
//! `SetOperation`'s (and a subquery's) bind parameters actually landing
//! correctly with Postgres's numbered `$1, $2, ...` placeholders
//! (SQLite/MySQL's `?` placeholders don't encode a position at all, so
//! this is the one part of set operations/subqueries with any real
//! per-dialect risk). `DISTINCT`/`BETWEEN` have no dialect-specific
//! behavior and are already covered against a real SQL engine there.

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

#[tokio::test]
async fn set_operation_bind_parameters_are_numbered_correctly_across_both_arms(
) -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS query_extras_pg_set_ops", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE query_extras_pg_set_ops (id BIGINT PRIMARY KEY, amount BIGINT NOT NULL)",
            &[],
        )
        .await?;

    let orders = Table::new("query_extras_pg_set_ops");
    for (id, amount) in [(1_i64, 10_i64), (2, 60), (3, 200)] {
        engine
            .execute(
                &Insert::into_table(&orders)
                    .value("id", id)
                    .value("amount", amount),
            )
            .await?;
    }

    // Each arm binds its own literal via a placeholder Postgres numbers
    // globally across the whole statement ($1 for the first arm, $2 for
    // the second) -- if SetOperation instead let each arm restart from $1
    // independently, this would either fail outright (Postgres rejecting
    // a duplicate/missing parameter position) or silently bind the wrong
    // value into the second arm's filter.
    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([orders.col("id")])
                .filter(orders.col("amount").eq(10_i64))
                .union(
                    Select::from(&orders)
                        .columns([orders.col("id")])
                        .filter(orders.col("amount").eq(200_i64)),
                ),
        )
        .await?;
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| r.get::<i64>(0))
        .collect::<rusty_db::Result<_>>()?;
    ids.sort();
    assert_eq!(ids, vec![1, 3]);

    engine
        .connect()
        .await?
        .execute("DROP TABLE query_extras_pg_set_ops", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn in_subquery_bind_parameters_are_numbered_correctly_across_the_outer_and_nested_query(
) -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS query_extras_pg_subquery", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE query_extras_pg_subquery (id BIGINT PRIMARY KEY, customer TEXT NOT NULL, amount BIGINT NOT NULL)",
            &[],
        )
        .await?;

    let orders = Table::new("query_extras_pg_subquery");
    for (id, customer, amount) in [(1_i64, "Ada", 10_i64), (2, "Grace", 60), (3, "Grace", 200)] {
        engine
            .execute(
                &Insert::into_table(&orders)
                    .value("id", id)
                    .value("customer", customer)
                    .value("amount", amount),
            )
            .await?;
    }

    // The outer filter binds one placeholder ($1) before the nested
    // subquery's own filter binds a second ($2) -- if `IN (subquery)`
    // instead rendered the subquery with a fresh, independent parameter
    // list, Postgres would see two `$1`s and either reject the statement
    // outright or bind the wrong value into one of them.
    let big_spenders = Select::from(&orders)
        .columns([orders.col("customer")])
        .filter(orders.col("amount").gt(100_i64));
    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([orders.col("id")])
                .filter(orders.col("amount").gt(0_i64))
                .filter(orders.col("customer").in_subquery(big_spenders)),
        )
        .await?;
    let mut ids: Vec<i64> = rows
        .iter()
        .map(|r| r.get::<i64>(0))
        .collect::<rusty_db::Result<_>>()?;
    ids.sort();
    assert_eq!(
        ids,
        vec![2, 3],
        "both of Grace's orders match, since Grace has one order over 100"
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE query_extras_pg_subquery", &[])
        .await?;
    Ok(())
}
