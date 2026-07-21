#![cfg(all(feature = "postgres", feature = "derive"))]

//! Same coverage as `replica_set.rs` (SQLite), against a real Postgres
//! server. There's no way to spin up real Postgres streaming replication
//! in this sandbox (that's the server's own feature, not something this
//! crate does), so "replicas" here are just separate tables on the same
//! live server, each seeded with its own marker row so a read's origin
//! can be identified from its result. A "down" replica is simulated with
//! a fake `Driver` whose `connect()` always fails — the same shape of
//! failure a real unreachable server would produce (`Error::Connection`).

use async_trait::async_trait;
use rusty_db::prelude::*;
use rusty_db::{Connection, Driver, Error};

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

/// A table standing in for one server in a replica topology, seeded with
/// a single marker row identifying it.
async fn node_table(engine: &Engine, table: &str, label: &str) -> rusty_db::Result<()> {
    engine
        .connect()
        .await?
        .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            &format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, label TEXT NOT NULL)"),
            &[],
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&Table::new(table))
                .value("id", 1_i64)
                .value("label", label),
        )
        .await?;
    Ok(())
}

async fn label_in(engine: &Engine, table: &str) -> rusty_db::Result<String> {
    let table = Table::new(table);
    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    row.get_by_name::<String>("label")
}

/// A `Driver` that always fails to connect — standing in for an
/// unreachable server, without needing to actually take one down.
struct DownDriver;

#[async_trait]
impl Driver for DownDriver {
    async fn connect(&self) -> rusty_db::Result<Box<dyn Connection>> {
        Err(Error::Connection("simulated outage".to_string()))
    }

    fn dialect(&self) -> &dyn rusty_db::Dialect {
        static DIALECT: rusty_db::dialect::NumberedDialect = rusty_db::dialect::NumberedDialect;
        &DIALECT
    }
}

fn down_engine() -> Engine {
    Engine::new(std::sync::Arc::new(DownDriver))
}

#[tokio::test]
async fn a_down_replica_fails_over_to_the_next_healthy_one() -> rusty_db::Result<()> {
    let Some(primary) = test_engine().await else {
        return Ok(());
    };
    let Some(healthy_replica) = test_engine().await else {
        return Ok(());
    };
    node_table(&primary, "replica_set_pg_primary", "primary").await?;
    node_table(
        &healthy_replica,
        "replica_set_pg_healthy",
        "replica-healthy",
    )
    .await?;

    let set = ReplicaSet::with_replicas(primary, vec![down_engine(), healthy_replica]);

    let table = Table::new("replica_set_pg_healthy");
    for _ in 0..4 {
        let row = set
            .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
            .await?;
        assert_eq!(row.get_by_name::<String>("label")?, "replica-healthy");
    }

    set.primary()
        .connect()
        .await?
        .execute("DROP TABLE replica_set_pg_primary", &[])
        .await?;
    set.primary()
        .connect()
        .await?
        .execute("DROP TABLE replica_set_pg_healthy", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn all_replicas_down_falls_back_to_the_primary() -> rusty_db::Result<()> {
    let Some(primary) = test_engine().await else {
        return Ok(());
    };
    node_table(&primary, "replica_set_pg_all_down", "primary").await?;

    let set = ReplicaSet::with_replicas(primary, vec![down_engine(), down_engine()]);

    assert_eq!(
        label_in(set.primary(), "replica_set_pg_all_down").await?,
        "primary"
    );
    let table = Table::new("replica_set_pg_all_down");
    let row = set
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(row.get_by_name::<String>("label")?, "primary");

    set.primary()
        .connect()
        .await?
        .execute("DROP TABLE replica_set_pg_all_down", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn writes_always_go_to_the_primary_never_a_replica() -> rusty_db::Result<()> {
    let Some(primary) = test_engine().await else {
        return Ok(());
    };
    let Some(replica) = test_engine().await else {
        return Ok(());
    };
    node_table(&primary, "replica_set_pg_writes_primary", "primary").await?;
    node_table(&replica, "replica_set_pg_writes_replica", "replica").await?;
    let replica_handle = replica.clone();

    let set = ReplicaSet::with_replicas(primary, vec![replica]);

    let table = Table::new("replica_set_pg_writes_primary");
    set.execute(
        &Insert::into_table(&table)
            .value("id", 2_i64)
            .value("label", "written-via-execute"),
    )
    .await?;

    let rows = set.primary().fetch_all(&Select::from(&table)).await?;
    assert_eq!(rows.len(), 2);

    let replica_table = Table::new("replica_set_pg_writes_replica");
    let replica_rows = replica_handle
        .fetch_all(&Select::from(&replica_table))
        .await?;
    assert_eq!(replica_rows.len(), 1);

    set.primary()
        .connect()
        .await?
        .execute("DROP TABLE replica_set_pg_writes_primary", &[])
        .await?;
    replica_handle
        .connect()
        .await?
        .execute("DROP TABLE replica_set_pg_writes_replica", &[])
        .await?;

    Ok(())
}
