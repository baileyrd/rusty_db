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

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "ag_pg_type_change")]
struct Counter {
    #[table(primary_key)]
    id: i64,
    count: i64,
}

#[tokio::test]
async fn a_hinted_column_type_change_produces_an_alter_column_type_and_applies_cleanly(
) -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .execute(&DropTable::new("ag_pg_type_change").if_exists())
        .await?;
    // "count" is deliberately a narrower live type (INTEGER) than what
    // `Counter` (ColumnType::I64 -> BIGINT) expects.
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE ag_pg_type_change (id BIGINT PRIMARY KEY, count INTEGER NOT NULL)",
            &[],
        )
        .await?;

    let expected = vec![TableSpec::of::<Counter>()];

    // With no hint, the type mismatch isn't detected at all — same as
    // any other unhinted column, matching what's already documented.
    assert_eq!(
        engine
            .autogenerate_migration(&expected, &AutogenerateOptions::default())
            .await?,
        Vec::<String>::new()
    );

    let options = AutogenerateOptions {
        changed_column_types: vec![("ag_pg_type_change".to_string(), "count".to_string())],
        ..Default::default()
    };
    let statements = engine.autogenerate_migration(&expected, &options).await?;
    assert_eq!(
        statements,
        vec![
            r#"ALTER TABLE "ag_pg_type_change" ALTER COLUMN "count" TYPE BIGINT USING "count"::BIGINT"#
                .to_string()
        ]
    );

    // Seed a row before altering, to prove the cast preserves existing
    // data rather than just working on an empty table.
    engine
        .connect()
        .await?
        .execute(
            "INSERT INTO ag_pg_type_change (id, count) VALUES (1, 42)",
            &[],
        )
        .await?;

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }

    let counters: Vec<Counter> = engine
        .fetch_all_as(&Select::from(&Counter::table()))
        .await?;
    assert_eq!(counters, vec![Counter { id: 1, count: 42 }]);

    let schema = engine
        .table_schema("ag_pg_type_change")
        .await?
        .expect("table exists");
    let count_column = schema
        .columns
        .iter()
        .find(|c| c.name == "count")
        .expect("count column exists");
    assert!(
        count_column.type_name.to_lowercase().contains("big"),
        "expected count's live type to now be bigint, got {:?}",
        count_column.type_name
    );

    // Now up to date — re-running with the same hint still active
    // produces the same statement again (the hint doesn't self-invalidate
    // the way a rename hint does, since there's no cheap way to tell
    // "already applied" from "still needs applying" without comparing
    // dialect-native type strings, exactly the gap this feature exists
    // to route around via explicit confirmation instead).
    assert_eq!(
        engine.autogenerate_migration(&expected, &options).await?,
        statements
    );

    engine.execute(&DropTable::new("ag_pg_type_change")).await?;
    Ok(())
}
