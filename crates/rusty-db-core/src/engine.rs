use std::sync::Arc;

use futures_core::stream::BoxStream;
use futures_util::StreamExt;

use crate::backup::{DatabaseDump, TableDump};
use crate::connection::{Connection, Driver};
use crate::dialect::Dialect;
use crate::error::{Error, Result};
use crate::mapping::FromRow;
use crate::migration::Migrator;
use crate::pool::PoolStats;
use crate::query::{Delete, Insert, Select, Table, ToSql};
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

    /// Like `fetch_all`, but yields rows one at a time instead of
    /// collecting the entire result set into a `Vec<Row>` first — for a
    /// large export/report where materializing everything up front would
    /// be a real memory ceiling. The returned stream owns the connection
    /// it checked out (kept alive for as long as the stream is), so
    /// there's nothing further to release explicitly — dropping the
    /// stream (e.g. ending iteration early) drops the connection too.
    pub async fn fetch_stream(&self, query: &dyn ToSql) -> Result<BoxStream<'static, Result<Row>>> {
        let (sql, params) = query.to_sql(self.dialect());
        let conn = self.connect().await?;
        Ok(conn.fetch_stream(sql, params))
    }

    /// Like `fetch_stream`, decoding each row into a `#[derive(Mapped)]`
    /// type as it arrives.
    pub async fn fetch_stream_as<T: FromRow + Send + 'static>(
        &self,
        query: &dyn ToSql,
    ) -> Result<BoxStream<'static, Result<T>>> {
        Ok(self
            .fetch_stream(query)
            .await?
            .map(|row| T::from_row(&row?))
            .boxed())
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
        Ok(Transaction {
            conn: Some(conn),
            two_phase_gid: None,
        })
    }

    /// Begins a transaction that participates in two-phase commit,
    /// identified by `gid` — a caller-chosen id, unique among whatever
    /// else might be prepared at the same time, since it's what
    /// `commit_prepared`/`rollback_prepared` use to find this transaction
    /// again later, possibly from an entirely different connection or
    /// process. That's the point of the second phase: a coordinator only
    /// decides commit-or-rollback once every participant has durably
    /// prepared, and "durably prepared" has to outlive any one connection
    /// for that to mean anything.
    ///
    /// Run statements through `Transaction::connection` as usual, then
    /// call `Transaction::prepare` (first phase) instead of `commit`.
    /// `Err(Error::Unsupported)` if the underlying dialect doesn't support
    /// it — currently: SQLite.
    pub async fn begin_two_phase(&self, gid: &str) -> Result<Transaction> {
        if !self.dialect().supports_two_phase_commit() {
            return Err(Error::Unsupported(
                "two-phase commit is not supported by this dialect".to_string(),
            ));
        }
        let mut conn = self.connect().await?;
        conn.execute_unprepared(&self.dialect().begin_two_phase_sql(gid))
            .await?;
        Ok(Transaction {
            conn: Some(conn),
            two_phase_gid: Some(gid.to_string()),
        })
    }

    /// Second phase: durably finalizes the transaction already prepared
    /// (via `Transaction::prepare`) under `gid`. Takes just the id — no
    /// connection or in-memory `Transaction` handle required, since a
    /// prepared transaction survives independently of both; this is
    /// typically called well after `prepare()`, once every other
    /// participant a coordinator is waiting on has also prepared
    /// successfully.
    pub async fn commit_prepared(&self, gid: &str) -> Result<()> {
        if !self.dialect().supports_two_phase_commit() {
            return Err(Error::Unsupported(
                "two-phase commit is not supported by this dialect".to_string(),
            ));
        }
        let mut conn = self.connect().await?;
        conn.execute_unprepared(&self.dialect().commit_prepared_sql(gid))
            .await?;
        Ok(())
    }

    /// Second phase: discards the transaction already prepared (via
    /// `Transaction::prepare`) under `gid`, undoing its changes. Same
    /// no-connection-required shape as `commit_prepared` — this is what a
    /// coordinator calls instead, for every participant, if even one of
    /// them failed to prepare.
    pub async fn rollback_prepared(&self, gid: &str) -> Result<()> {
        if !self.dialect().supports_two_phase_commit() {
            return Err(Error::Unsupported(
                "two-phase commit is not supported by this dialect".to_string(),
            ));
        }
        let mut conn = self.connect().await?;
        conn.execute_unprepared(&self.dialect().rollback_prepared_sql(gid))
            .await?;
        Ok(())
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

    /// A snapshot of the underlying connection pool: how many connections
    /// are open, idle, and in use against `max_connections`, how many
    /// callers are waiting on one right now, and how many acquires have
    /// ever succeeded. See `PoolStats`.
    pub fn pool_stats(&self) -> PoolStats {
        self.driver.pool_stats()
    }

    /// A logical backup: every row of every table this database has
    /// (per `list_tables`), captured as plain data. See `DatabaseDump`
    /// for what this can and can't do.
    pub async fn backup(&self) -> Result<DatabaseDump> {
        let tables = self.list_tables().await?;
        let table_refs: Vec<&str> = tables.iter().map(String::as_str).collect();
        self.backup_tables(&table_refs).await
    }

    /// Like `backup`, but only for the named tables — useful when you
    /// don't want (or, e.g. in a test sharing a live server with other
    /// work, can't safely risk) a `restore` touching every table in the
    /// database.
    pub async fn backup_tables(&self, tables: &[&str]) -> Result<DatabaseDump> {
        let mut dumped = Vec::with_capacity(tables.len());

        for &name in tables {
            let schema = self.table_schema(name).await?.ok_or_else(|| {
                crate::error::Error::Database(format!("table {name:?} does not exist"))
            })?;
            let columns: Vec<String> = schema.columns.into_iter().map(|c| c.name).collect();

            let table = Table::new(name);
            let raw_rows = self.fetch_all(&Select::from(&table)).await?;
            let rows = raw_rows
                .iter()
                .map(|row| {
                    (0..columns.len())
                        .map(|i| row.value(i).cloned().unwrap_or(Value::Null))
                        .collect()
                })
                .collect();

            dumped.push(TableDump {
                table: name.to_string(),
                columns,
                rows,
            });
        }

        Ok(DatabaseDump { tables: dumped })
    }

    /// Restores a `DatabaseDump`, replacing whatever is currently in each
    /// dumped table: every row is deleted, then every dumped row is
    /// re-inserted, all inside one transaction — a failure partway
    /// through rolls the whole restore back, leaving the database exactly
    /// as it was before `restore` was called.
    pub async fn restore(&self, dump: &DatabaseDump) -> Result<()> {
        let mut txn = self.begin().await?;

        for table_dump in &dump.tables {
            let table = Table::new(&table_dump.table);

            if let Err(err) = txn
                .execute_query(&Delete::from(&table), self.dialect())
                .await
            {
                txn.rollback().await?;
                return Err(err);
            }

            for row in &table_dump.rows {
                let insert = table_dump
                    .columns
                    .iter()
                    .zip(row.iter())
                    .fold(Insert::into_table(&table), |insert, (column, value)| {
                        insert.value(column.clone(), value.clone())
                    });

                if let Err(err) = txn.execute_query(&insert, self.dialect()).await {
                    txn.rollback().await?;
                    return Err(err);
                }
            }
        }

        txn.commit().await
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
    two_phase_gid: Option<String>,
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

    /// First phase of a two-phase commit, for a transaction started via
    /// `Engine::begin_two_phase`: durably records this transaction's
    /// changes without finalizing them — they aren't visible/committed
    /// until a later `Engine::commit_prepared`, and are undone instead by
    /// `Engine::rollback_prepared` if that's what the coordinator decides.
    ///
    /// Consumes this handle, same as `commit`/`rollback`: once prepared, a
    /// transaction's fate no longer depends on this connection at all, so
    /// there's nothing left for it to do — and on some dialects (MySQL's
    /// `XA`), the underlying session is left unable to do anything else
    /// anyway until this very transaction is resolved, so the connection
    /// is closed outright here rather than returned to a pool where
    /// reusing it would break whoever got it next.
    ///
    /// Panics if this transaction wasn't started via `begin_two_phase`.
    pub async fn prepare(mut self, dialect: &dyn Dialect) -> Result<()> {
        let gid = self
            .two_phase_gid
            .take()
            .expect("prepare() called on a transaction not started via begin_two_phase");
        let mut conn = self.conn.take().expect("transaction already finished");
        for stmt in dialect.prepare_two_phase_sql(&gid) {
            conn.execute_unprepared(&stmt).await?;
        }
        conn.close().await
    }
}
