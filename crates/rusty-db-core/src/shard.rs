//! Shard routing: pick which of several `Engine`s (each backing an
//! independent shard) a query should run against, based on a
//! caller-supplied key — the common "split rows across N databases by a
//! tenant/customer id" topology.
//!
//! This module has no way to move a row from one shard to another, no
//! cross-shard `JOIN`/aggregation, and no cross-shard transaction — it's
//! a router, not a distributed query planner. Every operation here always
//! talks to exactly one shard, chosen up front from the key you pass.
//!
//! Two routing strategies are available, chosen at construction time and
//! fixed for a `ShardRouter`'s lifetime — there's no `add_shard`/
//! `remove_shard` on an existing one, since neither strategy moves a row's
//! actual data for you when the shard count changes, only decides where a
//! *new* `ShardRouter` would now look for it:
//!
//! - [`ShardRouter::new`] — naive modulo hashing (`hash(key) %
//!   shard_count()`), the simpler default. Changing the shard count
//!   remaps nearly every key to a different shard (modulo a different
//!   number scrambles almost everything), so this is only a good fit when
//!   the shard count is fixed for good.
//! - [`ShardRouter::new_consistent`] — a hash ring with several virtual
//!   nodes per shard. Building a new `ShardRouter` with one additional
//!   shard (same virtual node count, existing shards passed in the same
//!   order) only remaps roughly `1 / (shard_count + 1)` of keys — the
//!   ones that happen to land near the new shard's ring positions — to
//!   the new shard, leaving the rest routed exactly where they already
//!   were. This bounds how much data a resharding migration has to move;
//!   it doesn't perform that migration itself.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::connection::Connection;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mapping::FromRow;
use crate::query::ToSql;
use crate::row::Row;
use crate::session::Session;

#[derive(Clone)]
enum RoutingStrategy {
    Modulo,
    /// Ring positions sorted ascending, each paired with the shard index
    /// it belongs to. A key's shard is whichever entry is the first at or
    /// after its own hash, wrapping around to the first entry past the
    /// end of the ring — the standard consistent-hashing lookup.
    Consistent {
        ring: Vec<(u64, usize)>,
    },
}

/// Routes to one of several shards' `Engine`s by hashing a caller-supplied
/// key. See the module docs for the two routing strategies and what this
/// deliberately doesn't do (cross-shard queries, moving data during a
/// reshard).
#[derive(Clone)]
pub struct ShardRouter {
    shards: Vec<Engine>,
    strategy: RoutingStrategy,
}

impl ShardRouter {
    /// `shards` is fixed for this `ShardRouter`'s lifetime — build a new
    /// one (and migrate data yourself) if the shard count ever needs to
    /// change. Uses naive modulo hashing — see `ShardRouter::new_consistent`
    /// for a routing strategy that remaps far fewer keys when the shard
    /// count changes. `Err(Error::QueryBuilder(_))` if `shards` is empty.
    pub fn new(shards: Vec<Engine>) -> Result<Self> {
        if shards.is_empty() {
            return Err(Error::QueryBuilder(
                "ShardRouter needs at least one shard".to_string(),
            ));
        }
        Ok(ShardRouter {
            shards,
            strategy: RoutingStrategy::Modulo,
        })
    }

    /// Like `new`, but routes via a consistent-hash ring instead of naive
    /// modulo hashing: `virtual_nodes` positions on the ring per shard
    /// (a higher count spreads keys more evenly across shards at the cost
    /// of a slightly larger ring to search; a few hundred is a reasonable
    /// default). Building a *new* `ShardRouter::new_consistent` with one
    /// more shard appended (same existing shards, same `virtual_nodes`)
    /// only remaps the minority of keys that happen to land near the new
    /// shard's ring positions — see the module docs. `Err(_)` if `shards`
    /// is empty or `virtual_nodes` is zero.
    pub fn new_consistent(shards: Vec<Engine>, virtual_nodes: usize) -> Result<Self> {
        if shards.is_empty() {
            return Err(Error::QueryBuilder(
                "ShardRouter needs at least one shard".to_string(),
            ));
        }
        if virtual_nodes == 0 {
            return Err(Error::QueryBuilder(
                "ShardRouter::new_consistent needs at least one virtual node per shard".to_string(),
            ));
        }

        let ring = Self::build_ring(shards.len(), virtual_nodes);
        Ok(ShardRouter {
            shards,
            strategy: RoutingStrategy::Consistent { ring },
        })
    }

    fn build_ring(shard_count: usize, virtual_nodes: usize) -> Vec<(u64, usize)> {
        let mut ring = Vec::with_capacity(shard_count * virtual_nodes);
        for shard_index in 0..shard_count {
            for replica in 0..virtual_nodes {
                let mut hasher = DefaultHasher::new();
                (shard_index, replica).hash(&mut hasher);
                ring.push((hasher.finish(), shard_index));
            }
        }
        ring.sort_unstable_by_key(|(position, _)| *position);
        ring
    }

    fn ring_lookup(ring: &[(u64, usize)], hash: u64) -> usize {
        match ring.binary_search_by(|(position, _)| position.cmp(&hash)) {
            Ok(i) => ring[i].1,
            Err(i) if i < ring.len() => ring[i].1,
            // Past every ring position — wrap around to the first one.
            Err(_) => ring[0].1,
        }
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// The shard index `key` routes to — `hash(key) % shard_count()` for a
    /// `new`-built router, or a ring lookup for a `new_consistent`-built
    /// one.
    pub fn shard_index(&self, key: impl Hash) -> usize {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let hash = hasher.finish();
        match &self.strategy {
            RoutingStrategy::Modulo => (hash % self.shards.len() as u64) as usize,
            RoutingStrategy::Consistent { ring } => Self::ring_lookup(ring, hash),
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hash_of(key: i32) -> u64 {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn ring_lookup_finds_the_first_position_at_or_after_the_hash_and_wraps_around() {
        let ring = vec![(10, 0), (20, 1), (30, 2)];
        assert_eq!(ShardRouter::ring_lookup(&ring, 5), 0); // before the first entry
        assert_eq!(ShardRouter::ring_lookup(&ring, 10), 0); // exact match
        assert_eq!(ShardRouter::ring_lookup(&ring, 25), 2); // between entries
        assert_eq!(ShardRouter::ring_lookup(&ring, 31), 0); // past the last entry, wraps
    }

    #[test]
    fn build_ring_produces_virtual_nodes_times_shard_count_entries_sorted_ascending() {
        let ring = ShardRouter::build_ring(3, 50);
        assert_eq!(ring.len(), 150);
        assert!(ring.windows(2).all(|pair| pair[0].0 <= pair[1].0));
    }

    #[test]
    fn appending_one_shard_to_a_consistent_ring_remaps_only_a_minority_of_keys_and_only_onto_the_new_shard(
    ) {
        let virtual_nodes = 200;
        let before = ShardRouter::build_ring(4, virtual_nodes);
        let after = ShardRouter::build_ring(5, virtual_nodes);

        let sample_size = 2000;
        let mut moved = 0;
        for key in 0..sample_size {
            let hash = hash_of(key);
            let before_idx = ShardRouter::ring_lookup(&before, hash);
            let after_idx = ShardRouter::ring_lookup(&after, hash);
            if before_idx != after_idx {
                moved += 1;
                // Consistent hashing never reshuffles keys among the
                // *existing* shards when only appending one more — a
                // moved key must always land specifically on it.
                assert_eq!(
                    after_idx, 4,
                    "a moved key must land on the newly added shard"
                );
            }
        }

        let moved_fraction = moved as f64 / sample_size as f64;
        assert!(
            moved_fraction < 0.35,
            "expected well under half of keys to move when appending a 5th shard to 4, moved {moved_fraction:.2}"
        );
    }

    #[test]
    fn naive_modulo_hashing_remaps_the_vast_majority_of_keys_when_a_shard_is_added() {
        let sample_size = 2000;
        let mut moved = 0;
        for key in 0..sample_size {
            let hash = hash_of(key);
            if hash % 4 != hash % 5 {
                moved += 1;
            }
        }

        let moved_fraction = moved as f64 / sample_size as f64;
        assert!(
            moved_fraction > 0.6,
            "expected the vast majority of keys to remap under naive modulo hashing, only {moved_fraction:.2} did"
        );
    }

    #[test]
    fn new_consistent_rejects_an_empty_shard_list() {
        let outcome = ShardRouter::new_consistent(Vec::<Engine>::new(), 10);
        assert!(outcome.is_err(), "an empty shard list is still rejected");
    }
}
