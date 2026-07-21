//! PostgreSQL `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, JsonValue, Uuid};
use sqlx::{Column as _, Postgres, Row as _, TypeInfo as _};

use rusty_db_core::dialect::NumberedDialect;
use rusty_db_core::{Connection, Dialect, Driver, Engine, Error, Executor, Result, Row, Value};

static DIALECT: NumberedDialect = NumberedDialect;

/// A `Driver` backed by a pooled PostgreSQL connection (via `sqlx::PgPool`).
pub struct PostgresDriver {
    pool: sqlx::PgPool,
}

impl PostgresDriver {
    /// Connect using a `postgres://user:pass@host/db` URL.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
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
impl Driver for PostgresDriver {
    async fn connect(&self) -> Result<Box<dyn Connection>> {
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        Ok(Box::new(PostgresConnection { conn }))
    }

    fn dialect(&self) -> &dyn Dialect {
        &DIALECT
    }
}

pub struct PostgresConnection {
    conn: PoolConnection<Postgres>,
}

fn to_core_err(e: sqlx::Error) -> Error {
    Error::Database(e.to_string())
}

fn row_from_postgres(row: &PgRow) -> Result<Row> {
    let columns: Arc<[String]> = row
        .columns()
        .iter()
        .map(|c| c.name().to_string())
        .collect::<Vec<_>>()
        .into();

    let mut values = Vec::with_capacity(columns.len());
    for (i, col) in row.columns().iter().enumerate() {
        let value = match col.type_info().name() {
            "BOOL" => row
                .try_get::<Option<bool>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Bool)
                .unwrap_or(Value::Null),
            "INT2" => row
                .try_get::<Option<i16>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::I64(v as i64))
                .unwrap_or(Value::Null),
            "INT4" => row
                .try_get::<Option<i32>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::I64(v as i64))
                .unwrap_or(Value::Null),
            "INT8" => row
                .try_get::<Option<i64>, _>(i)
                .map_err(to_core_err)?
                .map(Value::I64)
                .unwrap_or(Value::Null),
            "FLOAT4" => row
                .try_get::<Option<f32>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::F64(v as f64))
                .unwrap_or(Value::Null),
            "FLOAT8" => row
                .try_get::<Option<f64>, _>(i)
                .map_err(to_core_err)?
                .map(Value::F64)
                .unwrap_or(Value::Null),
            "BYTEA" => row
                .try_get::<Option<Vec<u8>>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Bytes)
                .unwrap_or(Value::Null),
            // These are all sent in Postgres's own binary wire formats, not
            // text (sqlx always requests binary result format), so each
            // needs its own typed decode; formatted to text afterward,
            // since `Value` has no dedicated numeric/temporal/JSON variant.
            "NUMERIC" => row
                .try_get::<Option<BigDecimal>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
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
            "TIMESTAMP" => row
                .try_get::<Option<NaiveDateTime>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
            "TIMESTAMPTZ" => row
                .try_get::<Option<DateTime<Utc>>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_rfc3339()))
                .unwrap_or(Value::Null),
            "UUID" => row
                .try_get::<Option<Uuid>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
            "JSON" | "JSONB" => row
                .try_get::<Option<JsonValue>, _>(i)
                .map_err(to_core_err)?
                .map(|v| Value::Text(v.to_string()))
                .unwrap_or(Value::Null),
            // TEXT, VARCHAR, CHAR(N)/BPCHAR, NAME, citext, etc. all decode
            // fine as strings (these genuinely are sent as text).
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
impl Executor for PostgresConnection {
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
        rows.iter().map(row_from_postgres).collect()
    }

    async fn fetch_optional(&mut self, sql: &str, params: &[Value]) -> Result<Option<Row>> {
        let query = bind_params!(sqlx::query(sql), params);
        let row = query
            .fetch_optional(&mut *self.conn)
            .await
            .map_err(to_core_err)?;
        row.as_ref().map(row_from_postgres).transpose()
    }
}

impl Connection for PostgresConnection {}
