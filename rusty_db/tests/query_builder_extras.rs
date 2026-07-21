#![cfg(feature = "sqlite")]

//! Exercises the newer query-builder additions against a real (if
//! in-memory) SQL engine, not just checking rendered SQL strings:
//! `Select::distinct`, `Column::between`, `Column::ilike`'s portable
//! fallback to plain `LIKE` on backends without a native `ILIKE` keyword,
//! `Table::alias` for self-joins, `Expr::text` for raw SQL fragments
//! composed into the builder, aggregate functions/expression columns via
//! `SelectExpr`, `Select::group_by`/`.having`, `Select::union`/
//! `union_all`/`intersect`/`except`, `Column::lower`/`upper`/`concat`/
//! `add`/`sub`/`mul`/`div`, `Expr::now`/`coalesce`, `Case`, subqueries
//! (`Column::in_subquery`, `Expr::exists`, `Expr::subquery`), and CTEs
//! (`Select::with`/`.with_recursive` via `Cte`).
//! `RETURNING` on `UPDATE`/`DELETE` has no SQLite coverage here since
//! SQLite's dialect doesn't support it (see
//! `query_builder_extras_postgres.rs`); `Column::concat`'s `CONCAT(...)`
//! fallback on MySQL/MariaDB (vs. `||` elsewhere) is dialect-specific
//! enough to get its own live-server coverage there too (see
//! `query_builder_extras_mysql.rs`).

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

#[tokio::test]
async fn string_functions_and_arithmetic_execute_and_decode_correctly() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    // id=1: customer "Ada", amount 10.
    let row = engine
        .fetch_one(
            &Select::from(&orders)
                .columns([
                    SelectExpr::from(orders.col("customer").lower()).alias("lower_name"),
                    SelectExpr::from(orders.col("customer").upper()).alias("upper_name"),
                    SelectExpr::from(orders.col("customer").concat(Expr::lit("!"))).alias("shout"),
                    SelectExpr::from(orders.col("amount").mul(Expr::lit(2_i64))).alias("doubled"),
                ])
                .filter(orders.col("id").eq(1_i64)),
        )
        .await?;
    assert_eq!(row.get_by_name::<String>("lower_name")?, "ada");
    assert_eq!(row.get_by_name::<String>("upper_name")?, "ADA");
    assert_eq!(row.get_by_name::<String>("shout")?, "Ada!");
    assert_eq!(row.get_by_name::<i64>("doubled")?, 20);

    Ok(())
}

#[tokio::test]
async fn case_and_coalesce_execute_and_decode_correctly() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, nickname TEXT, name TEXT NOT NULL, \
             amount INTEGER NOT NULL)",
            &[],
        )
        .await?;

    let customers = Table::new("customers");
    engine
        .execute(
            &Insert::into_table(&customers)
                .value("id", 1_i64)
                .value("nickname", Value::Null)
                .value("name", "Ada")
                .value("amount", 200_i64),
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&customers)
                .value("id", 2_i64)
                .value("nickname", "Gracie")
                .value("name", "Grace")
                .value("amount", 30_i64),
        )
        .await?;

    let tier = Case::new()
        .when(customers.col("amount").gt(100_i64), Expr::lit("gold"))
        .when(customers.col("amount").gt(50_i64), Expr::lit("silver"))
        .otherwise(Expr::lit("bronze"));

    let rows = engine
        .fetch_all(
            &Select::from(&customers)
                .columns([
                    SelectExpr::from(Expr::coalesce([
                        Expr::col(customers.col("nickname")),
                        Expr::col(customers.col("name")),
                    ]))
                    .alias("display_name"),
                    SelectExpr::from(tier).alias("tier"),
                ])
                .order_by(customers.col("id").asc()),
        )
        .await?;
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0].get_by_name::<String>("display_name")?,
        "Ada",
        "nickname is NULL, so COALESCE falls back to name"
    );
    assert_eq!(rows[0].get_by_name::<String>("tier")?, "gold");
    assert_eq!(
        rows[1].get_by_name::<String>("display_name")?,
        "Gracie",
        "nickname is present, so COALESCE uses it directly"
    );
    assert_eq!(rows[1].get_by_name::<String>("tier")?, "bronze");

    Ok(())
}

async fn customers_and_orders_engine() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let mut conn = engine.connect().await?;
    conn.execute(
        "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
        &[],
    )
    .await?;
    conn.execute(
        "CREATE TABLE orders (id INTEGER PRIMARY KEY, customer_id INTEGER NOT NULL, amount INTEGER NOT NULL)",
        &[],
    )
    .await?;

    let customers = Table::new("customers");
    for (id, name) in [(1_i64, "Ada"), (2, "Grace"), (3, "Zoe")] {
        engine
            .execute(
                &Insert::into_table(&customers)
                    .value("id", id)
                    .value("name", name),
            )
            .await?;
    }

    let orders = Table::new("orders");
    // Ada: one small order. Grace: two orders. Zoe: no orders at all —
    // the case an INNER JOIN would silently drop but EXISTS/a LEFT-JOIN-
    // shaped scalar subquery should still surface.
    for (id, customer_id, amount) in [(1_i64, 1_i64, 30_i64), (2, 2, 100), (3, 2, 50)] {
        engine
            .execute(
                &Insert::into_table(&orders)
                    .value("id", id)
                    .value("customer_id", customer_id)
                    .value("amount", amount),
            )
            .await?;
    }

    Ok(engine)
}

#[tokio::test]
async fn in_subquery_filters_rows_against_a_grouped_nested_select() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    // Grace's orders (50 + 200 = 250) clear 100 total; Ada's ("Ada" and
    // "ada" are distinct, case-sensitive groups: 10 and 50) don't.
    let big_spenders = Select::from(&orders)
        .columns([orders.col("customer")])
        .group_by([orders.col("customer")])
        .having(orders.col("amount").sum().gt(100_i64));

    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .filter(orders.col("customer").in_subquery(big_spenders))
                .order_by(orders.col("id").asc()),
        )
        .await?;
    assert_eq!(
        rows.len(),
        2,
        "only Grace's two orders clear the subquery's HAVING SUM(amount) > 100"
    );
    for row in &rows {
        assert_eq!(row.get_by_name::<String>("customer")?, "Grace");
    }

    Ok(())
}

#[tokio::test]
async fn exists_and_not_exists_correlate_against_the_outer_table() -> rusty_db::Result<()> {
    let engine = customers_and_orders_engine().await?;
    let customers = Table::new("customers");
    let orders = Table::new("orders");

    let has_orders =
        Select::from(&orders).filter(orders.col("customer_id").eq_col(&customers.col("id")));
    let with_orders = engine
        .fetch_all(
            &Select::from(&customers)
                .columns([customers.col("name")])
                .filter(Expr::exists(has_orders))
                .order_by(customers.col("id").asc()),
        )
        .await?;
    assert_eq!(
        with_orders
            .iter()
            .map(|r| r.get_by_name::<String>("name"))
            .collect::<Result<Vec<_>, _>>()?,
        vec!["Ada", "Grace"],
        "Zoe has no orders, so EXISTS excludes her"
    );

    let has_orders_again =
        Select::from(&orders).filter(orders.col("customer_id").eq_col(&customers.col("id")));
    let without_orders = engine
        .fetch_all(
            &Select::from(&customers)
                .columns([customers.col("name")])
                .filter(Expr::exists(has_orders_again).not()),
        )
        .await?;
    assert_eq!(without_orders.len(), 1);
    assert_eq!(without_orders[0].get_by_name::<String>("name")?, "Zoe");

    Ok(())
}

#[tokio::test]
async fn scalar_subquery_computes_a_per_row_aggregate_and_decodes_null_for_no_match(
) -> rusty_db::Result<()> {
    let engine = customers_and_orders_engine().await?;
    let customers = Table::new("customers");
    let orders = Table::new("orders");

    let order_total = Select::from(&orders)
        .columns([SelectExpr::from(orders.col("amount").sum())])
        .filter(orders.col("customer_id").eq_col(&customers.col("id")));

    let rows = engine
        .fetch_all(
            &Select::from(&customers)
                .columns([
                    SelectExpr::from(customers.col("name")),
                    SelectExpr::from(Expr::subquery(order_total)).alias("total"),
                ])
                .order_by(customers.col("id").asc()),
        )
        .await?;
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get_by_name::<String>("name")?, "Ada");
    assert_eq!(rows[0].get_by_name::<Option<i64>>("total")?, Some(30));
    assert_eq!(rows[1].get_by_name::<String>("name")?, "Grace");
    assert_eq!(rows[1].get_by_name::<Option<i64>>("total")?, Some(150));
    assert_eq!(rows[2].get_by_name::<String>("name")?, "Zoe");
    assert_eq!(
        rows[2].get_by_name::<Option<i64>>("total")?,
        None,
        "SUM over zero matching orders is NULL, not 0"
    );

    Ok(())
}

#[tokio::test]
async fn with_filters_rows_through_a_named_cte() -> rusty_db::Result<()> {
    let engine = seeded_engine().await?;
    let orders = Table::new("orders");

    // amounts: Ada/10, ada/50, Grace/50, Grace/200 -- only amount > 40 clears the CTE's filter.
    let big_orders = Cte::new(
        "big_orders",
        Select::from(&orders)
            .columns([orders.col("id"), orders.col("customer")])
            .filter(orders.col("amount").gt(40_i64)),
    );

    let cte_ref = Table::new("big_orders");
    let rows = engine
        .fetch_all(
            &Select::from(&cte_ref)
                .with([big_orders])
                .order_by(cte_ref.col("id").asc()),
        )
        .await?;
    assert_eq!(
        rows.iter()
            .map(|r| r.get_by_name::<String>("customer"))
            .collect::<Result<Vec<_>, _>>()?,
        vec!["ada", "Grace", "Grace"],
        "Ada's amount=10 order doesn't clear the CTE's own amount > 40 filter"
    );

    Ok(())
}

#[tokio::test]
async fn with_recursive_walks_a_management_hierarchy() -> rusty_db::Result<()> {
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
    // ada is the root; grace and linus report to ada; zoe reports to grace.
    for (id, name, manager_id) in [
        (1_i64, "ada", None::<i64>),
        (2, "grace", Some(1)),
        (3, "linus", Some(1)),
        (4, "zoe", Some(2)),
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

    let org_chart = Table::new("org_chart");
    let anchor = Select::from(&employees)
        .columns([
            SelectExpr::from(employees.col("id")),
            SelectExpr::from(employees.col("name")),
            SelectExpr::from(Expr::lit(1_i64)).alias("depth"),
        ])
        .filter(employees.col("manager_id").is_null());
    let recursive_term = Select::from(&employees)
        .columns([
            SelectExpr::from(employees.col("id")),
            SelectExpr::from(employees.col("name")),
            SelectExpr::from(org_chart.col("depth").add(Expr::lit(1_i64))).alias("depth"),
        ])
        .join(
            &org_chart,
            employees.col("manager_id").eq_col(&org_chart.col("id")),
        );
    let cte = Cte::recursive_union_all("org_chart", anchor, recursive_term);

    let rows = engine
        .fetch_all(
            &Select::from(&org_chart)
                .with_recursive([cte])
                .order_by(org_chart.col("depth").asc())
                .order_by(org_chart.col("name").asc()),
        )
        .await?;
    let names_and_depths: Vec<(String, i64)> = rows
        .iter()
        .map(|r| {
            Ok((
                r.get_by_name::<String>("name")?,
                r.get_by_name::<i64>("depth")?,
            ))
        })
        .collect::<rusty_db::Result<_>>()?;
    assert_eq!(
        names_and_depths,
        vec![
            ("ada".to_string(), 1),
            ("grace".to_string(), 2),
            ("linus".to_string(), 2),
            ("zoe".to_string(), 3),
        ]
    );

    Ok(())
}
