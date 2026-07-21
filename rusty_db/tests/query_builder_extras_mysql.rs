#![cfg(feature = "mysql")]

//! A reduced version of `query_builder_extras.rs` (SQLite) against a real
//! MySQL/MariaDB server — just the one thing that's actually
//! MySQL-specific: `Column::concat`'s fallback to `CONCAT(a, b)` instead
//! of `a || b`, since MySQL/MariaDB's `||` operator means logical `OR`
//! under the default `sql_mode`, not concatenation (confirmed against
//! this exact server: `SELECT 'foo' || 'bar'` returns `0`, not
//! `"foobar"`) — silently returning a boolean instead of erroring is
//! exactly the kind of wrong-not-broken bug that's worth a real,
//! executed check rather than trusting the rendered SQL string alone.

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
