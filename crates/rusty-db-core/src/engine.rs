use std::sync::Arc;

use crate::connection::{Connection, Driver};
use crate::dialect::Dialect;
use crate::error::Result;
use crate::mapping::FromRow;
use crate::migration::Migrator;
use crate::query::ToSql;
use crate::row::Row;
use crate::schema::TableSchema;
use crate::session::Session;
use crate::value::Value;

/// The single entry point applications use: wraps a `Driver` (Postgres,
/// SQLite, ...) behind a database-agnostic API. Code written against
/// `Engine` and the query builder is portable across every backend that
/// has a `Driver` implementation — swapping databases is just constructing
/// a different `Engine`.
#[derive(Clone)]
pub struct Engine {
    driver: Arc<dyn Driver>,
}

impl Engine {
    pub fn new(driver: Arc<dyn Driver>) -> Self {
        Engine { driver }
    }

    pub fn dialect(&self) -> &dyn Dialect {
        self.driver.dialect()
    }

    /// Check out a raw connection, for cases the query builder doesn't cover.
    pub async fn connect(&self) -> Result<Box<dyn Connection>> {
        self.driver.connect().await
    }

    pub async fn fetch_all(&self, query: &dyn ToSql) -> Result<Vec<Row>> {
        let (sql, params) = query.to_sql(self.dialect());
        let mut conn = self.connect().await?;
        conn.fetch_all(&sql, &params).await
    }

    pub async fn fetch_optional(&self, query: &dyn ToSql) -> Result<Option<Row>> {
        let (sql, params) = query.to_sql(self.dialect());
        let mut conn = self.connect().await?;
        conn.fetch_optional(&sql, &params).await
    }

    pub async fn fetch_one(&self, query: &dyn ToSql) -> Result<Row> {
        let (sql, params) = query.to_sql(self.dialect());
        let mut conn = self.connect().await?;
        conn.fetch_one(&sql, &params).await
    }

    /// Run a statement that doesn't return rows; returns rows affected.
    pub async fn execute(&self, query: &dyn ToSql) -> Result<u64> {
        let (sql, params) = query.to_sql(self.dialect());
        let mut conn = self.connect().await?;
        conn.execute(&sql, &params).await
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

    /// Check out a connection and issue `BEGIN` on it. Run statements
    /// through `Transaction::connection`, then finish with `commit()` or
    /// `rollback()`.
    pub async fn begin(&self) -> Result<Transaction> {
        let mut conn = self.connect().await?;
        conn.execute("BEGIN", &[]).await?;
        Ok(Transaction { conn: Some(conn) })
    }

    /// A unit-of-work session backed by this engine (see `Session`).
    pub fn session(&self) -> Session {
        Session::new(self.clone())
    }

    /// A migration runner backed by this engine (see `Migrator`).
    pub fn migrator(&self) -> Migrator<'_> {
        Migrator::new(self)
    }

    /// List every user table in the database, in name order (schema
    /// introspection/reflection). `Err(Error::Unsupported)` if the
    /// underlying driver doesn't implement this.
    pub async fn list_tables(&self) -> Result<Vec<String>> {
        self.driver.list_tables().await
    }

    /// Look up one table's columns straight from the database's own
    /// catalog; `Ok(None)` if no such table exists.
    /// `Err(Error::Unsupported)` if the underlying driver doesn't
    /// implement this.
    pub async fn table_schema(&self, table: &str) -> Result<Option<TableSchema>> {
        self.driver.table_schema(table).await
    }
}

/// A transaction checked out from an `Engine`.
///
/// If dropped without calling `commit()` or `rollback()`, the underlying
/// connection is simply returned to the pool with the transaction still
/// open; it is not rolled back automatically (stable Rust has no async
/// `Drop`). Always finish with one of those two methods.
pub struct Transaction {
    conn: Option<Box<dyn Connection>>,
}

impl Transaction {
    /// The connection this transaction is running on, for use with the
    /// query builder (`engine`-style calls) or raw SQL.
    pub fn connection(&mut self) -> &mut Box<dyn Connection> {
        self.conn.as_mut().expect("transaction already finished")
    }

    pub async fn fetch_all(
        &mut self,
        query: &dyn ToSql,
        dialect: &dyn Dialect,
    ) -> Result<Vec<Row>> {
        let (sql, params) = query.to_sql(dialect);
        self.connection().fetch_all(&sql, &params).await
    }

    pub async fn fetch_optional(
        &mut self,
        query: &dyn ToSql,
        dialect: &dyn Dialect,
    ) -> Result<Option<Row>> {
        let (sql, params) = query.to_sql(dialect);
        self.connection().fetch_optional(&sql, &params).await
    }

    pub async fn execute_query(&mut self, query: &dyn ToSql, dialect: &dyn Dialect) -> Result<u64> {
        let (sql, params) = query.to_sql(dialect);
        self.connection().execute(&sql, &params).await
    }

    pub async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        self.connection().execute(sql, params).await
    }

    pub async fn commit(mut self) -> Result<()> {
        let mut conn = self.conn.take().expect("transaction already finished");
        conn.execute("COMMIT", &[]).await?;
        Ok(())
    }

    pub async fn rollback(mut self) -> Result<()> {
        let mut conn = self.conn.take().expect("transaction already finished");
        conn.execute("ROLLBACK", &[]).await?;
        Ok(())
    }
}
