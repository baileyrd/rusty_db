//! PostgreSQL `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::pool::PoolConnection;
use sqlx::postgres::{PgPoolOptions, PgRow};
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::types::{BigDecimal, JsonValue, Uuid};
use sqlx::{Column as _, Postgres, Row as _, TypeInfo as _};

use rusty_db_core::dialect::NumberedDialect;
use rusty_db_core::{
    CheckConstraint, ColumnInfo, Connection, Dialect, Driver, Engine, Error, Executor, ForeignKey,
    PoolConfig, PoolMetrics, PoolStats, Result, Row, TableSchema, UniqueConstraint, Value,
};

static DIALECT: NumberedDialect = NumberedDialect;

/// A `Driver` backed by a pooled PostgreSQL connection (via `sqlx::PgPool`).
pub struct PostgresDriver {
    pool: sqlx::PgPool,
    metrics: Arc<PoolMetrics>,
}

impl PostgresDriver {
    /// Connect using a `postgres://user:pass@host/db` URL.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
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
        let mut options = PgPoolOptions::new().max_connections(config.max_connections);
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
impl Driver for PostgresDriver {
    async fn connect(&self) -> Result<Box<dyn Connection>> {
        let guard = self.metrics.track_acquire();
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        guard.succeeded();
        Ok(Box::new(PostgresConnection { conn }))
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
                "SELECT table_name FROM information_schema.tables \
                 WHERE table_schema = 'public' AND table_type = 'BASE TABLE' \
                 ORDER BY table_name",
                &[],
            )
            .await?;
        rows.iter()
            .map(|row| row.get_by_name::<String>("table_name"))
            .collect()
    }

    async fn table_schema(&self, table: &str) -> Result<Option<TableSchema>> {
        let mut conn = self.connect().await?;
        let table_value = Value::Text(table.to_string());

        let rows = conn
            .fetch_all(
                "SELECT column_name, data_type, is_nullable, column_default \
                 FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1 \
                 ORDER BY ordinal_position",
                std::slice::from_ref(&table_value),
            )
            .await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let pk_rows = conn
            .fetch_all(
                "SELECT kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                   AND tc.table_schema = kcu.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = 'public' \
                   AND tc.table_name = $1",
                std::slice::from_ref(&table_value),
            )
            .await?;
        let primary_keys = pk_rows
            .iter()
            .map(|row| row.get_by_name::<String>("column_name"))
            .collect::<Result<std::collections::HashSet<_>>>()?;

        let columns = rows
            .iter()
            .map(|row| {
                let name = row.get_by_name::<String>("column_name")?;
                Ok(ColumnInfo {
                    primary_key: primary_keys.contains(&name),
                    name,
                    type_name: row.get_by_name::<String>("data_type")?,
                    nullable: row.get_by_name::<String>("is_nullable")? == "YES",
                    default: row.get_by_name::<Option<String>>("column_default")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let unique_rows = conn
            .fetch_all(
                "SELECT tc.constraint_name, kcu.column_name \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                   AND tc.table_schema = kcu.table_schema \
                 WHERE tc.constraint_type = 'UNIQUE' \
                   AND tc.table_schema = 'public' \
                   AND tc.table_name = $1 \
                 ORDER BY tc.constraint_name, kcu.ordinal_position",
                std::slice::from_ref(&table_value),
            )
            .await?;
        let unique_constraints = group_unique_constraints(&unique_rows)?;

        // `information_schema.check_constraints` also includes a synthetic
        // entry per `NOT NULL` column (Postgres's catalog-level way of
        // representing `NOT NULL`, already captured by `ColumnInfo::
        // nullable`) — `pg_catalog.pg_constraint.contype = 'c'` is the
        // reliable way to get only genuine, user-written `CHECK`
        // constraints (`contype = 'n'` is where those synthetic entries
        // actually live). `pg_get_expr` reconstructs just the boolean
        // expression, with none of `CHECK`'s own keyword or the extra
        // parens `information_schema` adds around it.
        let check_rows = conn
            .fetch_all(
                "SELECT con.conname AS constraint_name, \
                        pg_get_expr(con.conbin, con.conrelid) AS expression \
                 FROM pg_catalog.pg_constraint con \
                 JOIN pg_catalog.pg_class rel ON rel.oid = con.conrelid \
                 JOIN pg_catalog.pg_namespace ns ON ns.oid = rel.relnamespace \
                 WHERE con.contype = 'c' \
                   AND ns.nspname = 'public' \
                   AND rel.relname = $1",
                std::slice::from_ref(&table_value),
            )
            .await?;
        let check_constraints = check_rows
            .iter()
            .map(|row| {
                Ok(CheckConstraint {
                    name: row.get_by_name::<String>("constraint_name")?,
                    expression: row.get_by_name::<String>("expression")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // Composite foreign keys can't be correctly reconstructed from
        // `information_schema` alone (joining `key_column_usage` to
        // `constraint_column_usage` by constraint name, with no shared
        // ordinal, cross-joins every local column with every referenced
        // one) — `pg_catalog.pg_constraint`'s own `conkey`/`confkey` arrays
        // are already correctly ordered pairs, so `unnest(...) WITH
        // ORDINALITY` pairs them up correctly regardless of how many
        // columns are involved. `string_agg` (not `array_agg`) so the
        // result comes back as plain text `Value`s — there's no dedicated
        // array `Value` variant.
        let fk_rows = conn
            .fetch_all(
                "SELECT con.conname AS constraint_name, \
                        string_agg(att.attname, ',' ORDER BY u.ord) AS columns, \
                        frel.relname AS referenced_table, \
                        string_agg(fatt.attname, ',' ORDER BY u.ord) AS referenced_columns \
                 FROM pg_catalog.pg_constraint con \
                 JOIN pg_catalog.pg_class rel ON rel.oid = con.conrelid \
                 JOIN pg_catalog.pg_namespace ns ON ns.oid = rel.relnamespace \
                 JOIN pg_catalog.pg_class frel ON frel.oid = con.confrelid \
                 JOIN LATERAL unnest(con.conkey, con.confkey) \
                   WITH ORDINALITY AS u(local_attnum, foreign_attnum, ord) ON true \
                 JOIN pg_catalog.pg_attribute att \
                   ON att.attrelid = con.conrelid AND att.attnum = u.local_attnum \
                 JOIN pg_catalog.pg_attribute fatt \
                   ON fatt.attrelid = con.confrelid AND fatt.attnum = u.foreign_attnum \
                 WHERE con.contype = 'f' \
                   AND ns.nspname = 'public' \
                   AND rel.relname = $1 \
                 GROUP BY con.conname, frel.relname \
                 ORDER BY con.conname",
                &[table_value],
            )
            .await?;
        let foreign_keys = fk_rows
            .iter()
            .map(|row| {
                Ok(ForeignKey {
                    name: row.get_by_name::<String>("constraint_name")?,
                    columns: row
                        .get_by_name::<String>("columns")?
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                    referenced_table: row.get_by_name::<String>("referenced_table")?,
                    referenced_columns: row
                        .get_by_name::<String>("referenced_columns")?
                        .split(',')
                        .map(str::to_string)
                        .collect(),
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(Some(TableSchema {
            name: table.to_string(),
            columns,
            unique_constraints,
            check_constraints,
            foreign_keys,
        }))
    }
}

/// Groups rows of `(constraint_name, column_name)` — ordered by
/// `constraint_name` then column position — into one `UniqueConstraint`
/// per distinct name.
fn group_unique_constraints(rows: &[Row]) -> Result<Vec<UniqueConstraint>> {
    let mut result: Vec<UniqueConstraint> = Vec::new();
    for row in rows {
        let name = row.get_by_name::<String>("constraint_name")?;
        let column = row.get_by_name::<String>("column_name")?;
        match result.last_mut() {
            Some(last) if last.name == name => last.columns.push(column),
            _ => result.push(UniqueConstraint {
                name,
                columns: vec![column],
            }),
        }
    }
    Ok(result)
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
