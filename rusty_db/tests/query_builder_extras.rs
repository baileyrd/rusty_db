#![cfg(feature = "sqlite")]

//! Exercises the newer query-builder additions against a real (if
//! in-memory) SQL engine, not just checking rendered SQL strings:
//! `Select::distinct`, `Column::between`, `Column::ilike`'s portable
//! fallback to plain `LIKE` on backends without a native `ILIKE` keyword,
//! `Table::alias` for self-joins, and `Expr::text` for raw SQL fragments
//! composed into the builder. `RETURNING` on `UPDATE`/`DELETE` has no
//! SQLite coverage here since SQLite's dialect doesn't support it (see
//! `query_builder_extras_postgres.rs`).

use rusty_db::prelude::*;

async fn seeded_engine() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT NOT NULL, amount INTEGER NOT NULL)",
            &[],
        )
        .await?;

    let orders = Table::new("orders");
    for (id, customer, amount) in [
        (1_i64, "Ada", 10_i64),
        (2, "ada", 50),
        (3, "Grace", 50),
        (4, "Grace", 200),
    ] {
        engine
            .execute(
                &Insert::into_table(&orders)
                    .value("id", id)
                    .value("customer", customer)
                    .value("amount", amount),
            )
            .await?;
    }

    Ok(engine)
}

#[tokio::test]
async fn distinct_dedupes_matching_rows() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    let all_amounts = engine
        .fetch_all(&Select::from(&orders).columns([orders.col("amount")]))
        .await?;
    assert_eq!(all_amounts.len(), 4);

    let distinct_amounts = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([orders.col("amount")])
                .distinct(),
        )
        .await?;
    let mut values: Vec<i64> = distinct_amounts
        .iter()
        .map(|r| r.get::<i64>(0))
        .collect::<rusty_db::Result<_>>()?;
    values.sort();
    assert_eq!(values, vec![10, 50, 200], "50 should be deduped to one row");

    Ok(())
}

#[tokio::test]
async fn between_includes_both_boundaries() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([orders.col("id")])
                .filter(orders.col("amount").between(10_i64, 50_i64))
                .order_by(orders.col("id").asc()),
        )
        .await?;
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| r.get::<i64>(0))
        .collect::<rusty_db::Result<_>>()?;
    // amount=10 and amount=50 (x2) are all within [10, 50]; amount=200 is not.
    assert_eq!(ids, vec![1, 2, 3]);

    Ok(())
}

#[tokio::test]
async fn ilike_matches_case_insensitively_via_its_portable_fallback() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([orders.col("id")])
                .filter(orders.col("customer").ilike("ada"))
                .order_by(orders.col("id").asc()),
        )
        .await?;
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| r.get::<i64>(0))
        .collect::<rusty_db::Result<_>>()?;
    assert_eq!(
        ids,
        vec![1, 2],
        "both \"Ada\" and \"ada\" should match a case-insensitive search"
    );

    Ok(())
}

#[tokio::test]
async fn table_alias_supports_a_real_self_join() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE employees (id INTEGER PRIMARY KEY, name TEXT NOT NULL, manager_id INTEGER)",
            &[],
        )
        .await?;

    let employees = Table::new("employees");
    for (id, name, manager_id) in [
        (1_i64, "ada", None::<i64>),
        (2, "grace", Some(1)),
        (3, "linus", Some(1)),
    ] {
        engine
            .execute(
                &Insert::into_table(&employees)
                    .value("id", id)
                    .value("name", name)
                    .value("manager_id", manager_id),
            )
            .await?;
    }

    // Self-join: each employee alongside their manager's name, via a
    // second, aliased reference to the same underlying table.
    let managers = employees.alias("managers");
    let rows = engine
        .fetch_all(
            &Select::from(&employees)
                .columns([employees.col("name"), managers.col("name")])
                .join(
                    &managers,
                    employees.col("manager_id").eq_col(&managers.col("id")),
                )
                .order_by(employees.col("id").asc()),
        )
        .await?;

    assert_eq!(rows.len(), 2, "only the two employees with a manager join");
    assert_eq!(rows[0].get::<String>(0)?, "grace");
    assert_eq!(rows[0].get::<String>(1)?, "ada");
    assert_eq!(rows[1].get::<String>(0)?, "linus");
    assert_eq!(rows[1].get::<String>(1)?, "ada");

    Ok(())
}

#[tokio::test]
async fn text_composes_a_raw_fragment_into_an_otherwise_builder_filter() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    // A raw fragment (with its own `?` placeholder) combined with an
    // ordinary builder-constructed filter via `.and(...)`.
    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([orders.col("id")])
                .filter(
                    Expr::text("lower(customer) = ?", [Value::Text("ada".to_string())])
                        .and(orders.col("amount").gt(20_i64)),
                )
                .order_by(orders.col("id").asc()),
        )
        .await?;
    let ids: Vec<i64> = rows
        .iter()
        .map(|r| r.get::<i64>(0))
        .collect::<rusty_db::Result<_>>()?;
    // id=1 (Ada, amount=10) fails the amount filter; id=2 (ada, amount=50) matches both.
    assert_eq!(ids, vec![2]);

    Ok(())
}
