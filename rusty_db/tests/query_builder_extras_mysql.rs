#![cfg(feature = "mysql")]

//! A reduced version of `query_builder_extras.rs` (SQLite) against a real
//! MySQL/MariaDB server — just the things that are actually
//! MySQL-specific: `Column::concat`'s fallback to `CONCAT(a, b)` instead
//! of `a || b`, since MySQL/MariaDB's `||` operator means logical `OR`
//! under the default `sql_mode`, not concatenation (confirmed against
//! this exact server: `SELECT 'foo' || 'bar'` returns `0`, not
//! `"foobar"`) — silently returning a boolean instead of erroring is
//! exactly the kind of wrong-not-broken bug that's worth a real,
//! executed check rather than trusting the rendered SQL string alone;
//! and window functions, which need a modern-enough server (MySQL
//! 8.0+/MariaDB 10.2+) to work at all.

use rusty_db::prelude::*;

/// Connects to a real MySQL/MariaDB server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `MYSQL_TEST_URL` at a scratch database (its schema
/// is created and dropped by this test) or the test skips itself instead of
/// failing when no server is reachable.
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

#[tokio::test]
async fn concat_uses_the_concat_function_not_the_or_meaning_double_pipe() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS query_extras_mysql_concat", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE query_extras_mysql_concat (id BIGINT PRIMARY KEY, first_name TEXT NOT \
             NULL, last_name TEXT NOT NULL)",
            &[],
        )
        .await?;

    let users = Table::new("query_extras_mysql_concat");
    engine
        .execute(
            &Insert::into_table(&users)
                .value("id", 1_i64)
                .value("first_name", "Ada")
                .value("last_name", "Lovelace"),
        )
        .await?;

    let row = engine
        .fetch_one(
            &Select::from(&users).columns([SelectExpr::from(
                users
                    .col("first_name")
                    .concat(Expr::lit(" "))
                    .concat(Expr::col(users.col("last_name"))),
            )
            .alias("full_name")]),
        )
        .await?;
    assert_eq!(
        row.get_by_name::<String>("full_name")?,
        "Ada Lovelace",
        "should be the actual concatenated string, not a boolean OR result"
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE query_extras_mysql_concat", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn window_functions_execute_correctly_on_a_real_server() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    // Window functions render identically on every dialect this crate
    // supports (no `Dialect` hook needed, unlike `.concat`), but they need
    // MySQL 8.0+/MariaDB 10.2+ -- an older server would surface a plain
    // SQL syntax error rather than silently computing something wrong, so
    // this just confirms it actually works against whatever real server
    // this environment has, rather than only ever rendering the SQL string.
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS query_extras_mysql_window", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE query_extras_mysql_window (id BIGINT PRIMARY KEY, amount BIGINT NOT NULL)",
            &[],
        )
        .await?;

    let orders = Table::new("query_extras_mysql_window");
    for (id, amount) in [(1_i64, 10_i64), (2, 50), (3, 50), (4, 200)] {
        engine
            .execute(
                &Insert::into_table(&orders)
                    .value("id", id)
                    .value("amount", amount),
            )
            .await?;
    }

    let running_total = orders
        .col("amount")
        .sum()
        .over(Window::new().order_by(orders.col("id").asc()));
    let rows = engine
        .fetch_all(
            &Select::from(&orders)
                .columns([SelectExpr::from(running_total).alias("running_total")])
                .order_by(orders.col("id").asc()),
        )
        .await?;
    // MySQL's `SUM` over a `BIGINT` column returns `DECIMAL` (sent over
    // the wire as text), not `BIGINT`, hence `BigDecimal` here rather than
    // the `i64` the SQLite version of this same query
    // (`sum_as_a_window_function_...` in `query_builder_extras.rs`) decodes as.
    let totals: Vec<BigDecimal> = rows
        .iter()
        .map(|r| r.get_by_name::<BigDecimal>("running_total"))
        .collect::<rusty_db::Result<_>>()?;
    assert_eq!(
        totals,
        vec![10, 60, 110, 310]
            .into_iter()
            .map(BigDecimal::from)
            .collect::<Vec<_>>()
    );

    engine
        .connect()
        .await?
        .execute("DROP TABLE query_extras_mysql_window", &[])
        .await?;
    Ok(())
}
