use async_trait::async_trait;

use crate::error::Result;
use crate::row::Row;
use crate::value::Value;

/// The lowest-level operation set every driver must provide: run SQL text
/// with positional parameters, get rows back. Everything above this
/// (the query builder, `Engine`) is built entirely in terms of `Executor`,
/// so a new backend only has to implement this trait plus `Driver`.
#[async_trait]
pub trait Executor: Send {
    /// Run a statement that doesn't return rows (INSERT/UPDATE/DELETE/DDL).
    /// Returns the number of rows affected.
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64>;

    /// Run a query and collect every row.
    async fn fetch_all(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>>;

    /// Run a query expected to return at most one row.
    async fn fetch_optional(&mut self, sql: &str, params: &[Value]) -> Result<Option<Row>>;

    /// Run a query expected to return exactly one row.
    async fn fetch_one(&mut self, sql: &str, params: &[Value]) -> Result<Row> {
        match self.fetch_optional(sql, params).await? {
            Some(row) => Ok(row),
            None => Err(crate::error::Error::RowNotFound),
        }
    }
}

/// A single, live connection to a database. Driver crates implement this
/// over their underlying client (e.g. an `sqlx::PoolConnection`).
#[async_trait]
pub trait Connection: Executor {}

/// A source of `Connection`s for one database plus the `Dialect` needed to
/// render portable SQL for it. This is the trait object that lets rusty_db
/// swap backends: an `Engine` just holds an `Arc<dyn Driver>`.
#[async_trait]
pub trait Driver: Send + Sync {
    async fn connect(&self) -> Result<Box<dyn Connection>>;

    fn dialect(&self) -> &dyn crate::dialect::Dialect;
}
