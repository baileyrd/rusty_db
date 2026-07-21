#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `ReplicaSet`: round-robin read routing across replicas, and
//! failover when one is unreachable.
//!
//! There's no way to spin up real database replication in this sandbox
//! (that's the database server's own feature, not something this crate
//! does), so these use independent file-backed SQLite databases as
//! stand-ins for replicas — each seeded with its own marker row so a
//! read's origin can be identified from its result, without `ReplicaSet`
//! needing to expose any routing internals for tests to peek at. A "down"
//! replica/primary is simulated with a fake `Driver` whose `connect()`
//! always fails — the same shape of failure a real unreachable server
//! would produce (`Error::Connection`), without needing to actually take
//! a live server down mid-suite.

use async_trait::async_trait;
use rusty_db::prelude::*;
use rusty_db::{Connection, Driver, Error};

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "nodes")]
struct Node {
    #[table(primary_key)]
    id: i64,
    label: String,
}

/// A file-backed database (not `:memory:`) seeded with a single marker row
/// identifying it, standing in for one server in a replica topology.
async fn node_engine(name: &str, label: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_replica_set_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE nodes (id INTEGER PRIMARY KEY, label TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&Node::table())
                .value("id", 1_i64)
                .value("label", label),
        )
        .await?;
    Ok(engine)
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
        static DIALECT: rusty_db::dialect::QuestionMarkDialect =
            rusty_db::dialect::QuestionMarkDialect;
        &DIALECT
    }
}

fn down_engine() -> Engine {
    Engine::new(std::sync::Arc::new(DownDriver))
}

fn label_of(rows: Vec<Node>) -> String {
    rows.into_iter().next().unwrap().label
}

#[tokio::test]
async fn reads_round_robin_across_healthy_replicas() -> rusty_db::Result<()> {
    let primary = node_engine("round_robin_primary", "primary").await?;
    let replica_a = node_engine("round_robin_a", "replica-a").await?;
    let replica_b = node_engine("round_robin_b", "replica-b").await?;

    let set = ReplicaSet::with_replicas(primary, vec![replica_a, replica_b]);

    let mut seen = Vec::new();
    for _ in 0..4 {
        let rows: Vec<Node> = set.fetch_all_as(&Select::from(&Node::table())).await?;
        seen.push(label_of(rows));
    }

    // Two replicas, round-robin: the 4 reads alternate between them,
    // never touching the primary and never repeating the same replica
    // twice in a row.
    assert_eq!(
        seen,
        vec!["replica-a", "replica-b", "replica-a", "replica-b"]
    );

    Ok(())
}

#[tokio::test]
async fn a_down_replica_fails_over_to_the_next_healthy_one() -> rusty_db::Result<()> {
    let primary = node_engine("failover_primary", "primary").await?;
    let healthy_replica = node_engine("failover_healthy", "replica-healthy").await?;

    let set = ReplicaSet::with_replicas(primary, vec![down_engine(), healthy_replica]);

    // Every read's rotation includes the down replica at some point, but
    // it should never surface as an error or an empty result — it always
    // fails over to the one healthy replica.
    for _ in 0..4 {
        let rows: Vec<Node> = set.fetch_all_as(&Select::from(&Node::table())).await?;
        assert_eq!(label_of(rows), "replica-healthy");
    }

    Ok(())
}

#[tokio::test]
async fn all_replicas_down_falls_back_to_the_primary() -> rusty_db::Result<()> {
    let primary = node_engine("all_down_primary", "primary").await?;

    let set = ReplicaSet::with_replicas(primary, vec![down_engine(), down_engine()]);

    let rows: Vec<Node> = set.fetch_all_as(&Select::from(&Node::table())).await?;
    assert_eq!(label_of(rows), "primary");

    Ok(())
}

#[tokio::test]
async fn a_replica_set_with_no_replicas_always_uses_the_primary() -> rusty_db::Result<()> {
    let primary = node_engine("no_replicas_primary", "primary").await?;
    let set = ReplicaSet::new(primary);

    assert_eq!(set.replica_count(), 0);
    let rows: Vec<Node> = set.fetch_all_as(&Select::from(&Node::table())).await?;
    assert_eq!(label_of(rows), "primary");

    Ok(())
}

#[tokio::test]
async fn writes_always_go_to_the_primary_never_a_replica() -> rusty_db::Result<()> {
    let primary = node_engine("writes_primary", "primary").await?;
    let replica = node_engine("writes_replica", "replica").await?;
    let replica_handle = replica.clone();
    let set = ReplicaSet::with_replicas(primary, vec![replica]);

    set.execute(
        &Insert::into_table(&Node::table())
            .value("id", 2_i64)
            .value("label", "written-via-execute"),
    )
    .await?;

    let mut session = set.session();
    session.add(&Node {
        id: 3,
        label: "written-via-session".to_string(),
    });
    session.commit().await?;

    // Both writes landed on the primary (the seed row plus these two)...
    let mut on_primary: Vec<Node> = set
        .primary()
        .fetch_all_as(&Select::from(&Node::table()))
        .await?;
    on_primary.sort_by_key(|n| n.id);
    assert_eq!(on_primary.len(), 3);

    // ...and never touched the replica, which still only has its seed row.
    let on_replica: Vec<Node> = replica_handle
        .fetch_all_as(&Select::from(&Node::table()))
        .await?;
    assert_eq!(on_replica.len(), 1);

    Ok(())
}
