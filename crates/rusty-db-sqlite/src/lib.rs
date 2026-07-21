//! SQLite `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqlitePoolOptions, SqliteRow};
use sqlx::{Column as _, Row as _, Sqlite, SqlitePool, TypeInfo as _};

use rusty_db_core::dialect::QuestionMarkDialect;
use rusty_db_core::{
    Connection, Dialect, Driver, Engine, Error, Executor, PoolConfig, Result, Row, Value,
};

static DIALECT: QuestionMarkDialect = QuestionMarkDialect;

/// A `Driver` backed by a pooled SQLite connection (via `sqlx::SqlitePool`).
pub struct SqliteDriver {
    pool: SqlitePool,
}

impl SqliteDriver {
    /// Connect using an sqlx-style URL, e.g. `sqlite::memory:` or `sqlite://path/to.db`.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = SqlitePoolOptions::new()
            .connect(url)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self { pool })
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
        Ok(Self { pool })
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
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Box::new(SqliteConnection { conn }))
    }

    fn dialect(&self) -> &dyn Dialect {
        &DIALECT
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
        let value = match col.type_info().name() {
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
