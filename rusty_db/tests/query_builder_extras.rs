#![cfg(feature = "sqlite")]

//! Exercises the newer query-builder additions against a real (if
//! in-memory) SQL engine, not just checking rendered SQL strings:
//! `Select::distinct`, `Column::between`, `Column::ilike`'s portable
//! fallback to plain `LIKE` on backends without a native `ILIKE` keyword,
//! `Table::alias` for self-joins, `Expr::text` for raw SQL fragments
//! composed into the builder, aggregate functions/expression columns via
//! `SelectExpr`, `Select::group_by`/`.having`, and `Select::union`/
//! `union_all`/`intersect`/`except`. `RETURNING` on `UPDATE`/`DELETE` has
//! no SQLite coverage here since SQLite's dialect doesn't support it (see
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

#[tokio::test]
async fn aggregate_functions_execute_and_decode_correctly() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    // amounts: 10, 50, 50, 200
    let row = engine
        .fetch_one(&Select::from(&orders).columns([
            SelectExpr::from(Expr::count_all()).alias("n"),
            SelectExpr::from(orders.col("amount").sum()).alias("total"),
            SelectExpr::from(orders.col("amount").avg()).alias("average"),
            SelectExpr::from(orders.col("amount").min()).alias("smallest"),
            SelectExpr::from(orders.col("amount").max()).alias("largest"),
        ]))
        .await?;
    assert_eq!(row.get_by_name::<i64>("n")?, 4);
    assert_eq!(row.get_by_name::<i64>("total")?, 310);
    assert_eq!(row.get_by_name::<f64>("average")?, 77.5);
    assert_eq!(row.get_by_name::<i64>("smallest")?, 10);
    assert_eq!(row.get_by_name::<i64>("largest")?, 200);

    Ok(())
}

#[tokio::test]
async fn plain_and_aggregate_columns_compose_and_an_expression_column_is_aliased(
) -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    let row = engine
        .fetch_one(
            &Select::from(&orders)
                .columns([
                    SelectExpr::from(orders.col("customer")),
                    SelectExpr::from(Expr::count_all()).alias("n"),
                ])
                .filter(orders.col("id").eq(1_i64)),
        )
        .await?;
    assert_eq!(row.get_by_name::<String>("customer")?, "Ada");
    // COUNT(*) here is scoped by the same WHERE id = 1 filter as the rest
    // of the query, not the whole table.
    assert_eq!(row.get_by_name::<i64>("n")?, 1);

    // An arbitrary expression column, not just an aggregate.
    let row = engine
        .fetch_one(
            &Select::from(&orders)
                .columns([SelectExpr::from(Expr::text("amount * 2", [])).alias("doubled")])
                .filter(orders.col("id").eq(1_i64)),
        )
        .await?;
    assert_eq!(row.get_by_name::<i64>("doubled")?, 20);

    Ok(())
}

#[tokio::test]
async fn group_by_and_having_execute_and_decode_correctly() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    // Ada: 10; ada: 50; Grace: 50 + 200 = 250 ("Ada"/"ada" are distinct,
    // case-sensitive groups).
    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([
                    SelectExpr::from(orders.col("customer")),
                    SelectExpr::from(orders.col("amount").sum()).alias("total"),
                ])
                .group_by([orders.col("customer")])
                .order_by(orders.col("customer").asc()),
        )
        .await?;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_by_name::<String>("customer")?, "Ada");
    assert_eq!(rows[0].get_by_name::<i64>("total")?, 10);
    assert_eq!(rows[1].get_by_name::<String>("customer")?, "Grace");
    assert_eq!(rows[1].get_by_name::<i64>("total")?, 250);
    assert_eq!(rows[2].get_by_name::<String>("customer")?, "ada");
    assert_eq!(rows[2].get_by_name::<i64>("total")?, 50);

    // HAVING narrows to groups whose total exceeds 100 -- only Grace's.
    let filtered = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([
                    SelectExpr::from(orders.col("customer")),
                    SelectExpr::from(orders.col("amount").sum()).alias("total"),
                ])
                .group_by([orders.col("customer")])
                .having(orders.col("amount").sum().gt(100_i64)),
        )
        .await?;
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].get_by_name::<String>("customer")?, "Grace");
    assert_eq!(filtered[0].get_by_name::<i64>("total")?, 250);

    Ok(())
}

#[tokio::test]
async fn set_operations_execute_and_decode_correctly() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    // low: Ada/10, ada/50, Grace/50 -- 3 rows, one duplicate value (none).
    // high: ada/50, Grace/50, Grace/200 -- 3 rows, "Grace" appears twice.
    let low = Select::from(&orders)
        .columns([orders.col("customer")])
        .filter(orders.col("amount").lt(60_i64));
    let high = Select::from(&orders)
        .columns([orders.col("customer")])
        .filter(orders.col("amount").gte(50_i64));

    let mut union_rows: Vec<String> = engine
        .fetch_all(&low.clone().union(high.clone()))
        .await?
        .iter()
        .map(|r| r.get::<String>(0))
        .collect::<rusty_db::Result<_>>()?;
    union_rows.sort();
    assert_eq!(
        union_rows,
        vec!["Ada".to_string(), "Grace".to_string(), "ada".to_string()],
        "UNION dedupes the repeated \"Grace\"/\"ada\" values"
    );

    let union_all_rows = engine
        .fetch_all(&low.clone().union_all(high.clone()))
        .await?;
    assert_eq!(
        union_all_rows.len(),
        6,
        "UNION ALL keeps every row from both arms, duplicates included"
    );

    let mut intersect_rows: Vec<String> = engine
        .fetch_all(&low.clone().intersect(high.clone()))
        .await?
        .iter()
        .map(|r| r.get::<String>(0))
        .collect::<rusty_db::Result<_>>()?;
    intersect_rows.sort();
    assert_eq!(
        intersect_rows,
        vec!["Grace".to_string(), "ada".to_string()],
        "only values present in both arms survive INTERSECT"
    );

    let except_rows: Vec<String> = engine
        .fetch_all(&low.except(high))
        .await?
        .iter()
        .map(|r| r.get::<String>(0))
        .collect::<rusty_db::Result<_>>()?;
    assert_eq!(
        except_rows,
        vec!["Ada".to_string()],
        "only values in the first arm but not the second survive EXCEPT"
    );

    Ok(())
}

#[tokio::test]
async fn set_operations_chain_to_combine_more_than_two_selects() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    let ada = Select::from(&orders)
        .columns([orders.col("customer")])
        .filter(orders.col("customer").eq("Ada"));
    let lower_ada = Select::from(&orders)
        .columns([orders.col("customer")])
        .filter(orders.col("customer").eq("ada"));
    let grace = Select::from(&orders)
        .columns([orders.col("customer")])
        .filter(orders.col("customer").eq("Grace"));

    let mut rows: Vec<String> = engine
        .fetch_all(&ada.union(lower_ada).union(grace))
        .await?
        .iter()
        .map(|r| r.get::<String>(0))
        .collect::<rusty_db::Result<_>>()?;
    rows.sort();
    // "Grace" appears twice in the underlying table but only once here,
    // since each arm (and the UNION combining them) still dedupes.
    assert_eq!(
        rows,
        vec!["Ada".to_string(), "Grace".to_string(), "ada".to_string()]
    );

    Ok(())
}
