#![cfg(all(feature = "mysql", feature = "derive"))]

//! Exercises `Engine::autogenerate_migration` against a real MySQL/MariaDB
//! server — a reduced version of `autogenerate.rs`: just confirming the
//! generated DDL is genuinely dialect-correct and applies cleanly here too.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "ag_mysql_customers")]
struct Customer {
    #[table(primary_key)]
    id: i64,
    name: String,
    payload: Option<Json>,
}

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
async fn generated_ddl_applies_and_converges_on_mysql() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .execute(&DropTable::new("ag_mysql_customers").if_exists())
        .await?;

    let expected = vec![TableSpec::of::<Customer>()];
    let statements = engine.autogenerate_migration(&expected).await?;
    assert_eq!(statements.len(), 1);
    assert!(statements[0].contains("JSON"));

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }

    let mut session = engine.session();
    session.add(&Customer {
        id: 1,
        name: "ada".to_string(),
        payload: Some(serde_json::json!({"k": "v"})),
    });
    session.commit().await?;
    let rows: Vec<Customer> = engine
        .fetch_all_as(&Select::from(&Customer::table()))
        .await?;
    assert_eq!(rows.len(), 1);

    assert_eq!(
        engine.autogenerate_migration(&expected).await?,
        Vec::<String>::new()
    );

    engine
        .execute(&DropTable::new("ag_mysql_customers"))
        .await?;
    Ok(())
}
