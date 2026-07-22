#![cfg(all(feature = "postgres", feature = "derive"))]

//! Exercises `Engine::autogenerate_migration` against a real PostgreSQL
//! server — a reduced version of `autogenerate.rs`: just confirming the
//! generated DDL is genuinely dialect-correct and applies cleanly here too.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "ag_pg_customers")]
struct Customer {
    #[table(primary_key)]
    id: i64,
    name: String,
    signup: Option<Uuid>,
}

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
async fn generated_ddl_applies_and_converges_on_postgres() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .execute(&DropTable::new("ag_pg_customers").if_exists())
        .await?;

    let expected = vec![TableSpec::of::<Customer>()];
    let statements = engine
        .autogenerate_migration(&expected, &AutogenerateOptions::default())
        .await?;
    assert_eq!(statements.len(), 1);
    assert!(statements[0].contains("UUID"));

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }

    let mut session = engine.session();
    session.add(&Customer {
        id: 1,
        name: "ada".to_string(),
        signup: Some(Uuid::new_v4()),
    });
    session.commit().await?;
    let rows: Vec<Customer> = engine
        .fetch_all_as(&Select::from(&Customer::table()))
        .await?;
    assert_eq!(rows.len(), 1);

    assert_eq!(
        engine
            .autogenerate_migration(&expected, &AutogenerateOptions::default())
            .await?,
        Vec::<String>::new()
    );

    engine.execute(&DropTable::new("ag_pg_customers")).await?;
    Ok(())
}
