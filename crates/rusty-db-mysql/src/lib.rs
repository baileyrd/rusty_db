//! MySQL/MariaDB `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::mysql::{MySqlPoolOptions, MySqlRow};
use sqlx::pool::PoolConnection;
use sqlx::types::chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use sqlx::{Column as _, MySql, MySqlPool, Row as _, TypeInfo as _};

use rusty_db_core::dialect::MySqlDialect;
use rusty_db_core::{Connection, Dialect, Driver, Engine, Error, Executor, Result, Row, Value};

static DIALECT: MySqlDialect = MySqlDialect;

/// A `Driver` backed by a pooled MySQL/MariaDB connection (via `sqlx::MySqlPool`).
pub struct MySqlDriver {
    pool: MySqlPool,
}

impl MySqlDriver {
    /// Connect using a `mysql://user:pass@host/db` URL.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = MySqlPoolOptions::new()
            .connect(url)
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Self { pool })
    }

    /// Convenience: connect and wrap directly in an `Engine`.
    pub async fn engine(url: &str) -> Result<Engine> {
        Ok(Engine::new(Arc::new(Self::connect(url).await?)))
    }
}

#[async_trait]
impl Driver for MySqlDriver {
    async fn connect(&self) -> Result<Box<dyn Connection>> {
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Box::new(MySqlConnection { conn }))
    }

    fn dialect(&self) -> &dyn Dialect {
        &DIALECT
    }
}

pub struct MySqlConnection {
    conn: PoolConnection<MySql>,
}

fn to_core_err(e: sqlx::Error) -> Error {
    Error::Database(e.to_string())
}

fn row_from_mysql(row: &MySqlRow) -> Result<Row> {
    let columns: Arc<[String]> = row
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect::<Vec<_>>()
        .into();

    let mut values = Vec::with_capacity(columns.len());
    for (i, col) in row.columns().iter().enumerate() {
        let value = match col.type_info().name() {
            "BOOLEAN" => row
                .try_get::<Option<bool>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Bool)
                .unwrap_or(Value::Null),
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "INTEGER" | "BIGINT" => row
                .try_get::<Option<i64>, _>(i)
                .map_err(to_core_err)?
                .map(Value::I64)
                .unwrap_or(Value::Null),
            // YEAR is unsigned-only in MySQL's wire protocol, same as the
            // "* UNSIGNED" integer variants below.
            "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
            | "BIGINT UNSIGNED" | "YEAR" => row
                .try_get::<Option<u64>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::I64(v as i64))
                .unwrap_or(Value::Null),
            "FLOAT" => row
                .try_get::<Option<f32>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::F64(v as f64))
                .unwrap_or(Value::Null),
            "DOUBLE" => row
                .try_get::<Option<f64>, _>(i)
                .map_err(to_core_err)?
                .map(Value::F64)
                .unwrap_or(Value::Null),
            "BLOB" | "VARBINARY" | "BINARY" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" => row
                .try_get::<Option<Vec<u8>>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Bytes)
                .unwrap_or(Value::Null),
            // DATE/TIME/DATETIME/TIMESTAMP are packed binary structures on
            // MySQL's wire protocol, not text (unlike DECIMAL/JSON below) —
            // decode via `chrono` and format to text, since `Value` has no
            // dedicated temporal variant.
            "DATE" => row
                .try_get::<Option<NaiveDate>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
            "TIME" => row
                .try_get::<Option<NaiveTime>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
            "DATETIME" | "TIMESTAMP" => row
                .try_get::<Option<NaiveDateTime>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
            "NULL" => Value::Null,
            // VARCHAR, TEXT, CHAR, DECIMAL, JSON, ENUM, and anything else
            // decode as strings. This uses the unchecked getter
            // deliberately: sqlx's `String` only declares itself statically
            // compatible with a handful of column types (VARCHAR/TEXT/
            // CHAR/ENUM and friends), but DECIMAL and JSON are sent as text
            // on MySQL's wire protocol too — `try_get` would reject them
            // before ever looking at the bytes. GEOMETRY/BIT/SET land here
            // as well and may not actually be valid UTF-8; that surfaces as
            // a decode error rather than a panic.
            _ => row
                .try_get_unchecked::<Option<String>, _>(i)
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
impl Executor for MySqlConnection {
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
        rows.iter().map(row_from_mysql).collect()
    }

    async fn fetch_optional(&mut self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        let query = bind_params!(sqlx::query(sql), params);
        let row = query
            .fetch_optional(&mut *self.conn)
            .await
            .map_err(to_core_err)?;
        row.as_ref().map(row_from_mysql).transpose()
    }
}

impl Connection for MySqlConnection {}
