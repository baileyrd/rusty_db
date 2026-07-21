//! SQLite `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqlitePoolOptions, SqliteRow};
use sqlx::{Column as _, Row as _, Sqlite, SqlitePool, TypeInfo as _, ValueRef as _};

use rusty_db_core::dialect::QuestionMarkDialect;
use rusty_db_core::{
    ColumnInfo, Connection, Dialect, Driver, Engine, Error, Executor, PoolConfig, PoolMetrics,
    PoolStats, Result, Row, TableSchema, Value,
};

static DIALECT: QuestionMarkDialect = QuestionMarkDialect;

/// A `Driver` backed by a pooled SQLite connection (via `sqlx::SqlitePool`).
pub struct SqliteDriver {
    pool: SqlitePool,
    metrics: Arc<PoolMetrics>,
}

impl SqliteDriver {
    /// Connect using an sqlx-style URL, e.g. `sqlite::memory:` or `sqlite://path/to.db`.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .connect(url)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self {
            pool,
            metrics: Arc::new(PoolMetrics::new()),
        })
    }

    /// Connect with explicit pool tuning (e.g. a constrained
    /// `max_connections` and/or `acquire_timeout`) instead of the
    /// underlying driver's defaults.
    pub async fn connect_with(url: &str, config: PoolConfig) -> Result<Self> {
        let mut options = SqlitePoolOptions::new().max_connections(config.max_connections);
        if let Some(timeout) = config.acquire_timeout {
            options = options.acquire_timeout(timeout);
        }
        let pool = options
            .connect(url)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self {
            pool,
            metrics: Arc::new(PoolMetrics::new()),
        })
    }

    /// Convenience: connect and wrap directly in an `Engine`.
    pub async fn engine(url: &str) -> Result<Engine> {
        Ok(Engine::new(Arc::new(Self::connect(url).await?)))
    }

    /// Convenience: `connect_with` and wrap directly in an `Engine`.
    pub async fn engine_with(url: &str, config: PoolConfig) -> Result<Engine> {
        Ok(Engine::new(Arc::new(
            Self::connect_with(url, config).await?,
        )))
    }
}

#[async_trait]
impl Driver for SqliteDriver {
    async fn connect(&self) -> Result<Box<dyn Connection>> {
        let guard = self.metrics.track_acquire();
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        guard.succeeded();
        Ok(Box::new(SqliteConnection { conn }))
    }

    fn dialect(&self) -> &dyn Dialect {
        &DIALECT
    }

    fn pool_stats(&self) -> PoolStats {
        let idle = self.pool.num_idle() as u32;
        let active = self.pool.size();
        PoolStats {
            max_connections: self.pool.options().get_max_connections(),
            active,
            idle,
            in_use: active.saturating_sub(idle),
            waiters: self.metrics.waiters(),
            total_acquires: self.metrics.total_acquires(),
        }
    }

    async fn list_tables(&self) -> Result<Vec<String>> {
        let mut conn = self.connect().await?;
        let rows = conn
            .fetch_all(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
                 ORDER BY name",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| row.get_by_name::<String>("name"))
            .collect()
    }

    async fn table_schema(&self, table: &str) -> Result<Option<TableSchema>> {
        if !self.list_tables().await?.iter().any(|t| t == table) {
            return Ok(None);
        }

        let mut conn = self.connect().await?;
        // PRAGMA doesn't support bound parameters for the table name;
        // quoting it is the standard mitigation for identifiers that
        // have to be interpolated directly into SQL text.
        let sql = format!("PRAGMA table_info({})", DIALECT.quote_ident(table));
        let rows = conn.fetch_all(&sql, &[]).await?;

        let columns = rows
            .iter()
            .map(|row| {
                Ok(ColumnInfo {
                    name: row.get_by_name::<String>("name")?,
                    type_name: row.get_by_name::<String>("type")?,
                    nullable: row.get_by_name::<i64>("notnull")? == 0,
                    primary_key: row.get_by_name::<i64>("pk")? > 0,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Some(TableSchema {
            name: table.to_string(),
            columns,
        }))
    }
}

pub struct SqliteConnection {
    conn: PoolConnection<Sqlite>,
}

fn to_core_err(e: sqlx::Error) -> Error {
    Error::Database(e.to_string())
}

fn row_from_sqlite(row: &SqliteRow) -> Result<Row> {
    let columns: Arc<[String]> = row
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect::<Vec<_>>()
        .into();

    let mut values = Vec::with_capacity(columns.len());
    for (i, col) in row.columns().iter().enumerate() {
        // The column's own declared type is "NULL" when SQLite couldn't
        // infer a static type for it at prepare time — not "this column
        // is always NULL". This happens for query results that aren't
        // plain table columns (PRAGMA output, computed expressions,
        // `SELECT`s combined by `UNION`, ...), and SQLite is dynamically
        // typed on a *per-value* basis anyway, so fall back to this row's
        // actual runtime type for that case instead of assuming NULL.
        let declared_type = col.type_info().name();
        let type_name = if declared_type == "NULL" {
            row.try_get_raw(i)
                .map_err(to_core_err)?
                .type_info()
                .name()
                .to_string()
        } else {
            declared_type.to_string()
        };

        let value = match type_name.as_str() {
            "INTEGER" | "BIGINT" | "INT" => row
                .try_get::<Option<i64>, _>(i)
                .map_err(to_core_err)?
                .map(Value::I64)
                .unwrap_or(Value::Null),
            "REAL" | "DOUBLE" | "FLOAT" => row
                .try_get::<Option<f64>, _>(i)
                .map_err(to_core_err)?
                .map(Value::F64)
                .unwrap_or(Value::Null),
            "BOOLEAN" => row
                .try_get::<Option<bool>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Bool)
                .unwrap_or(Value::Null),
            "BLOB" => row
                .try_get::<Option<Vec<u8>>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Bytes)
                .unwrap_or(Value::Null),
            "NULL" => Value::Null,
            // TEXT, VARCHAR, CLOB, and anything else SQLite's dynamic typing
            // didn't already narrow above.
            _ => row
                .try_get::<Option<String>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Text)
                .unwrap_or(Value::Null),
        };
        values.push(value);
    }

    Ok(Row::new(columns, values))
}

macro_rules! bind_params {
    ($query:expr, $params:expr) => {{
        let mut query = $query;
        for p in $params {
            query = match p {
                Value::Null => query.bind(None::<i64>),
                Value::Bool(b) => query.bind(*b),
                Value::I64(i) => query.bind(*i),
                Value::F64(f) => query.bind(*f),
                Value::Text(s) => query.bind(s.clone()),
                Value::Bytes(b) => query.bind(b.clone()),
            };
        }
        query
    }};
}

#[async_trait]
impl Executor for SqliteConnection {
    async fn execute(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        let query = bind_params!(sqlx::query(sql), params);
        let result = query.execute(&mut *self.conn).await.map_err(to_core_err)?;
        Ok(result.rows_affected())
    }

    async fn fetch_all(&mut self, sql: &str, params: &[Value]) -> Result<Vec<Row>> {
        let query = bind_params!(sqlx::query(sql), params);
        let rows = query
            .fetch_all(&mut *self.conn)
            .await
            .map_err(to_core_err)?;
        rows.iter().map(row_from_sqlite).collect()
    }

    async fn fetch_optional(&mut self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        let query = bind_params!(sqlx::query(sql), params);
        let row = query
            .fetch_optional(&mut *self.conn)
            .await
            .map_err(to_core_err)?;
        row.as_ref().map(row_from_sqlite).transpose()
    }
}

impl Connection for SqliteConnection {}
