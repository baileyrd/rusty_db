//! Read-replica routing: spread reads round-robin across a set of replica
//! `Engine`s, and send writes to a single primary — the common "one
//! writer, many readers" topology most databases scale reads with.
//!
//! This module has no way to make one database's data show up on another
//! server; that's the database's own replication feature (Postgres
//! streaming replication, MySQL/MariaDB replication, etc). `ReplicaSet`
//! only routes traffic across an already-replicated set of `Engine`s —
//! one per server — that the application constructs and hands it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use crate::connection::Connection;
use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mapping::FromRow;
use crate::query::ToSql;
use crate::row::Row;
use crate::session::Session;

/// Routes reads round-robin across zero or more replicas, and writes
/// straight to a single primary.
///
/// If a replica's connection attempt fails, a read automatically retries
/// the next replica in rotation instead of failing outright, and falls
/// back to the primary if every replica turns out to be unreachable (or
/// none are configured at all) — so a `ReplicaSet` with no replicas is a
/// primary-only, always-available fallback rather than a degenerate case
/// callers need to special-case.
///
/// Reads never go through `Session`: a session's autoflush/identity-map
/// guarantees depend on reading back its own not-yet-committed writes,
/// which a replica — lagging behind the primary by design — can't
/// promise. `ReplicaSet::session()` deliberately hands back a
/// primary-backed `Session` rather than trying to route through this at
/// all.
#[derive(Clone)]
pub struct ReplicaSet {
    primary: Engine,
    replicas: Vec<Engine>,
    next: Arc<AtomicUsize>,
}

impl ReplicaSet {
    /// A `ReplicaSet` with no replicas yet — every read falls back to the
    /// primary until some are added with `add_replica`.
    pub fn new(primary: Engine) -> Self {
        ReplicaSet {
            primary,
            replicas: Vec::new(),
            next: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn with_replicas(primary: Engine, replicas: Vec<Engine>) -> Self {
        ReplicaSet {
            primary,
            replicas,
            next: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn add_replica(&mut self, replica: Engine) {
        self.replicas.push(replica);
    }

    pub fn primary(&self) -> &Engine {
        &self.primary
    }

    pub fn replica_count(&self) -> usize {
        self.replicas.len()
    }

    /// The replica indices to try for one logical read, in order,
    /// starting from the next round-robin slot.
    fn rotation(&self) -> Vec<usize> {
        let len = self.replicas.len();
        if len == 0 {
            return Vec::new();
        }
        let start = self.next.fetch_add(1, Ordering::Relaxed) % len;
        (0..len).map(|offset| (start + offset) % len).collect()
    }

    pub async fn fetch_all(&self, query: &dyn ToSql) -> Result<Vec<Row>> {
        for index in self.rotation() {
            match self.replicas[index].fetch_all(query).await {
                Ok(rows) => return Ok(rows),
                Err(Error::Connection(_)) => continue,
                Err(other) => return Err(other),
            }
        }
        self.primary.fetch_all(query).await
    }

    pub async fn fetch_optional(&self, query: &dyn ToSql) -> Result<Option<Row>> {
        for index in self.rotation() {
            match self.replicas[index].fetch_optional(query).await {
                Ok(row) => return Ok(row),
                Err(Error::Connection(_)) => continue,
                Err(other) => return Err(other),
            }
        }
        self.primary.fetch_optional(query).await
    }

    pub async fn fetch_one(&self, query: &dyn ToSql) -> Result<Row> {
        for index in self.rotation() {
            match self.replicas[index].fetch_one(query).await {
                Ok(row) => return Ok(row),
                Err(Error::Connection(_)) => continue,
                Err(other) => return Err(other),
            }
        }
        self.primary.fetch_one(query).await
    }

    /// Like `fetch_all`, decoding each row into a `#[derive(Mapped)]` type.
    pub async fn fetch_all_as<T: FromRow>(&self, query: &dyn ToSql) -> Result<Vec<T>> {
        self.fetch_all(query)
            .await?
            .iter()
            .map(T::from_row)
            .collect()
    }

    /// Like `fetch_optional`, decoding the row into a `#[derive(Mapped)]` type.
    pub async fn fetch_optional_as<T: FromRow>(&self, query: &dyn ToSql) -> Result<Option<T>> {
        self.fetch_optional(query)
            .await?
            .as_ref()
            .map(T::from_row)
            .transpose()
    }

    /// Like `fetch_one`, decoding the row into a `#[derive(Mapped)]` type.
    pub async fn fetch_one_as<T: FromRow>(&self, query: &dyn ToSql) -> Result<T> {
        T::from_row(&self.fetch_one(query).await?)
    }

    /// Writes always run against the primary — replicas are read-only.
    pub async fn execute(&self, query: &dyn ToSql) -> Result<u64> {
        self.primary.execute(query).await
    }

    /// A raw connection to the primary, for cases the query builder
    /// doesn't cover.
    pub async fn connect(&self) -> Result<Box<dyn Connection>> {
        self.primary.connect().await
    }

    /// A unit-of-work session against the primary (see the type-level docs
    /// for why sessions never read from replicas).
    pub fn session(&self) -> Session {
        self.primary.session()
    }
}
