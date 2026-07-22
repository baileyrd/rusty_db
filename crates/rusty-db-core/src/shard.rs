//! Shard routing: pick which of several `Engine`s (each backing an
//! independent shard) a query should run against, based on a
//! caller-supplied key — the common "split rows across N databases by a
//! tenant/customer id" topology.
//!
//! This module has no way to move a row from one shard to another, no
//! cross-shard `JOIN`/aggregation, and no cross-shard transaction — it's
//! a router, not a distributed query planner. Every operation here always
//! talks to exactly one shard, chosen up front from the key you pass.
//! Routing itself is naive modulo hashing (`hash(key) % shard_count()`),
//! not consistent hashing: changing the shard count remaps most keys to a
//! different shard, so growing or shrinking a `ShardRouter` without a
//! migration step of your own will make existing rows look like they
//! vanished from wherever a key used to route. Because of that,
//! `ShardRouter::new` is the only way to build one — there's
//! deliberately no `add_shard`, to avoid making "just add one more shard"
//! look like a safe, casual operation the way it is for `ReplicaSet`.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::connection::Connection;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mapping::FromRow;
use crate::query::ToSql;
use crate::row::Row;
use crate::session::Session;

/// Routes to one of several shards' `Engine`s by hashing a caller-supplied
/// key. See the module docs for what this deliberately doesn't do
/// (cross-shard queries, resharding).
#[derive(Clone)]
pub struct ShardRouter {
    shards: Vec<Engine>,
}

impl ShardRouter {
    /// `shards` is fixed for this `ShardRouter`'s lifetime — build a new
    /// one (and migrate data yourself) if the shard count ever needs to
    /// change. `Err(Error::QueryBuilder(_))` if `shards` is empty.
    pub fn new(shards: Vec<Engine>) -> Result<Self> {
        if shards.is_empty() {
            return Err(Error::QueryBuilder(
                "ShardRouter needs at least one shard".to_string(),
            ));
        }
        Ok(ShardRouter { shards })
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// The shard index `key` routes to: `hash(key) % shard_count()`.
    pub fn shard_index(&self, key: impl Hash) -> usize {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        (hasher.finish() % self.shards.len() as u64) as usize
    }

    /// The `Engine` for the shard `key` routes to.
    pub fn shard_for(&self, key: impl Hash) -> &Engine {
        &self.shards[self.shard_index(key)]
    }

    /// The `Engine` at a specific 0-based shard index, or `None` if out of
    /// range — for maintenance across every shard by index (running the
    /// same `Migrator::up`/`CreateTable` against each one, say) rather
    /// than key-based routing.
    pub fn shard(&self, index: usize) -> Option<&Engine> {
        self.shards.get(index)
    }

    /// Every shard's `Engine`, in index order — for the same
    /// fan-out-maintenance use as `shard(index)`.
    pub fn shards(&self) -> &[Engine] {
        &self.shards
    }

    /// Run a statement that doesn't return rows against the shard `key`
    /// routes to; returns rows affected.
    pub async fn execute(&self, key: impl Hash, query: &dyn ToSql) -> Result<u64> {
        self.shard_for(key).execute(query).await
    }

    pub async fn fetch_all(&self, key: impl Hash, query: &dyn ToSql) -> Result<Vec<Row>> {
        self.shard_for(key).fetch_all(query).await
    }

    pub async fn fetch_optional(&self, key: impl Hash, query: &dyn ToSql) -> Result<Option<Row>> {
        self.shard_for(key).fetch_optional(query).await
    }

    pub async fn fetch_one(&self, key: impl Hash, query: &dyn ToSql) -> Result<Row> {
        self.shard_for(key).fetch_one(query).await
    }

    /// Like `fetch_all`, decoding each row into a `#[derive(Mapped)]` type.
    pub async fn fetch_all_as<T: FromRow>(
        &self,
        key: impl Hash,
        query: &dyn ToSql,
    ) -> Result<Vec<T>> {
        self.shard_for(key).fetch_all_as(query).await
    }

    /// Like `fetch_optional`, decoding the row into a `#[derive(Mapped)]` type.
    pub async fn fetch_optional_as<T: FromRow>(
        &self,
        key: impl Hash,
        query: &dyn ToSql,
    ) -> Result<Option<T>> {
        self.shard_for(key).fetch_optional_as(query).await
    }

    /// Like `fetch_one`, decoding the row into a `#[derive(Mapped)]` type.
    pub async fn fetch_one_as<T: FromRow>(&self, key: impl Hash, query: &dyn ToSql) -> Result<T> {
        self.shard_for(key).fetch_one_as(query).await
    }

    /// A raw connection to the shard `key` routes to, for cases the query
    /// builder doesn't cover.
    pub async fn connect(&self, key: impl Hash) -> Result<Box<dyn Connection>> {
        self.shard_for(key).connect().await
    }

    /// A unit-of-work `Session` against the shard `key` routes to — note
    /// that, like everything else here, a `Session`'s transaction/identity
    /// map is entirely local to that one shard; nothing coordinates across
    /// shards.
    pub fn session(&self, key: impl Hash) -> Session {
        self.shard_for(key).session()
    }
}
