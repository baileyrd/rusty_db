#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `ShardRouter`: routing by a hashed key to one of several
//! independent `Engine`s, and that every convenience method routes
//! consistently. Each shard is a separate file-backed SQLite database
//! seeded with its own marker row, so a query's origin can be identified
//! from its result without `ShardRouter` needing to expose any routing
//! internals for tests to peek at — the same technique `replica_set.rs`
//! uses for simulated replicas.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "nodes")]
struct Node {
    #[table(primary_key)]
    id: i64,
    label: String,
}

/// A file-backed database (not `:memory:`) seeded with a single marker row
/// identifying it, standing in for one shard.
async fn shard_engine(name: &str, label: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_shard_{name}_{}.sqlite3",
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

async fn three_shards(test_name: &str) -> rusty_db::Result<Vec<Engine>> {
    Ok(vec![
        shard_engine(&format!("{test_name}_a"), "shard-a").await?,
        shard_engine(&format!("{test_name}_b"), "shard-b").await?,
        shard_engine(&format!("{test_name}_c"), "shard-c").await?,
    ])
}

/// Two `i64` keys, searched among small integers, that the given router
/// routes to two *different* shard indices — needed since `ShardRouter`'s
/// hashing is deliberately opaque (no guarantee which literal key lands
/// on which shard), so a test can't just hardcode "1 goes to shard 0".
fn two_keys_on_different_shards(router: &ShardRouter) -> (i64, i64) {
    let first = 0_i64;
    let first_index = router.shard_index(first);
    let second = (1..100)
        .find(|k| router.shard_index(*k) != first_index)
        .expect("with 3 shards, some small key should land elsewhere");
    (first, second)
}

#[tokio::test]
async fn a_write_lands_on_exactly_the_shard_its_key_routes_to() -> rusty_db::Result<()> {
    let shards = three_shards("write_lands").await?;
    let shard_handles = shards.clone();
    let router = ShardRouter::new(shards)?;

    let (key_x, key_y) = two_keys_on_different_shards(&router);
    let index_x = router.shard_index(key_x);
    let index_y = router.shard_index(key_y);

    router
        .execute(
            key_x,
            &Insert::into_table(&Node::table())
                .value("id", 2_i64)
                .value("label", "written-for-x"),
        )
        .await?;
    router
        .execute(
            key_y,
            &Insert::into_table(&Node::table())
                .value("id", 3_i64)
                .value("label", "written-for-y"),
        )
        .await?;

    // Each write landed on exactly the shard its own key routes to...
    let rows_x: Vec<Node> = shard_handles[index_x]
        .fetch_all_as(&Select::from(&Node::table()))
        .await?;
    assert!(rows_x.iter().any(|n| n.label == "written-for-x"));

    let rows_y: Vec<Node> = shard_handles[index_y]
        .fetch_all_as(&Select::from(&Node::table()))
        .await?;
    assert!(rows_y.iter().any(|n| n.label == "written-for-y"));

    // ...and, since the two keys route to different shards, neither
    // write shows up on the other one.
    assert!(!rows_x.iter().any(|n| n.label == "written-for-y"));
    assert!(!rows_y.iter().any(|n| n.label == "written-for-x"));

    Ok(())
}

#[tokio::test]
async fn the_same_key_always_routes_to_the_same_shard() -> rusty_db::Result<()> {
    let router = ShardRouter::new(three_shards("same_key").await?)?;

    let first = router.shard_index(42_i64);
    for _ in 0..10 {
        assert_eq!(router.shard_index(42_i64), first);
    }

    Ok(())
}

#[tokio::test]
async fn fetch_and_session_helpers_all_route_the_same_key_consistently() -> rusty_db::Result<()> {
    let router = ShardRouter::new(three_shards("fetch_session").await?)?;
    let key = "customer-123";

    let mut session = router.session(key);
    session.add(&Node {
        id: 2,
        label: "via-session".to_string(),
    });
    session.commit().await?;

    // A plain fetch_all_as with the same key sees the row session() wrote,
    // proving both routed to the same shard.
    let rows: Vec<Node> = router
        .fetch_all_as(key, &Select::from(&Node::table()))
        .await?;
    assert!(rows.iter().any(|n| n.label == "via-session"));

    let one: Node = router
        .fetch_one_as(
            key,
            &Select::from(&Node::table()).filter(Node::table().col("id").eq(2_i64)),
        )
        .await?;
    assert_eq!(one.label, "via-session");

    Ok(())
}

#[tokio::test]
async fn shard_and_shards_expose_every_engine_by_index() -> rusty_db::Result<()> {
    let router = ShardRouter::new(three_shards("expose_by_index").await?)?;

    assert_eq!(router.shard_count(), 3);
    assert_eq!(router.shards().len(), 3);
    assert!(router.shard(0).is_some());
    assert!(router.shard(1).is_some());
    assert!(router.shard(2).is_some());
    assert!(router.shard(3).is_none());

    Ok(())
}

#[tokio::test]
async fn new_rejects_an_empty_shard_list() {
    let outcome = ShardRouter::new(Vec::new());
    assert!(outcome.is_err(), "a router needs at least one shard");
}

#[tokio::test]
async fn a_consistent_hash_router_routes_writes_the_same_way_a_modulo_router_does(
) -> rusty_db::Result<()> {
    let shards = three_shards("consistent_write").await?;
    let shard_handles = shards.clone();
    let router = ShardRouter::new_consistent(shards, 100)?;

    let (key_x, key_y) = two_keys_on_different_shards(&router);
    let index_x = router.shard_index(key_x);
    let index_y = router.shard_index(key_y);

    router
        .execute(
            key_x,
            &Insert::into_table(&Node::table())
                .value("id", 2_i64)
                .value("label", "written-for-x"),
        )
        .await?;
    router
        .execute(
            key_y,
            &Insert::into_table(&Node::table())
                .value("id", 3_i64)
                .value("label", "written-for-y"),
        )
        .await?;

    let rows_x: Vec<Node> = shard_handles[index_x]
        .fetch_all_as(&Select::from(&Node::table()))
        .await?;
    assert!(rows_x.iter().any(|n| n.label == "written-for-x"));
    assert!(!rows_x.iter().any(|n| n.label == "written-for-y"));

    let rows_y: Vec<Node> = shard_handles[index_y]
        .fetch_all_as(&Select::from(&Node::table()))
        .await?;
    assert!(rows_y.iter().any(|n| n.label == "written-for-y"));
    assert!(!rows_y.iter().any(|n| n.label == "written-for-x"));

    Ok(())
}

#[tokio::test]
async fn new_consistent_rejects_an_empty_shard_list() {
    let outcome = ShardRouter::new_consistent(Vec::new(), 100);
    assert!(outcome.is_err(), "a router needs at least one shard");
}

#[tokio::test]
async fn new_consistent_rejects_zero_virtual_nodes() -> rusty_db::Result<()> {
    let outcome = ShardRouter::new_consistent(three_shards("zero_virtual_nodes").await?, 0);
    assert!(
        outcome.is_err(),
        "a ring with zero virtual nodes per shard can't route anything"
    );
    Ok(())
}
