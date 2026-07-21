//! MySQL/MariaDB `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::mysql::{MySqlPoolOptions, MySqlRow};
use sqlx::pool::PoolConnection;
use sqlx::types::chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use sqlx::{Column as _, MySql, MySqlPool, Row as _, TypeInfo as _};

use rusty_db_core::dialect::MySqlDialect;
use rusty_db_core::{
    CheckConstraint, ColumnInfo, Connection, Dialect, Driver, Engine, Error, Executor, ForeignKey,
    IndexInfo, PoolConfig, PoolMetrics, PoolStats, Result, Row, TableSchema, UniqueConstraint,
    Value,
};

static DIALECT: MySqlDialect = MySqlDialect;

/// A `Driver` backed by a pooled MySQL/MariaDB connection (via `sqlx::MySqlPool`).
pub struct MySqlDriver {
    pool: MySqlPool,
    metrics: Arc<PoolMetrics>,
}

impl MySqlDriver {
    /// Connect using a `mysql://user:pass@host/db` URL.
    pub async fn connect(url: &str) -> Result<Self> {
        let pool = MySqlPoolOptions::new()
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
        let mut options = MySqlPoolOptions::new().max_connections(config.max_connections);
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
impl Driver for MySqlDriver {
    async fn connect(&self) -> Result<Box<dyn Connection>> {
        let guard = self.metrics.track_acquire();
        let conn = self
            .pool
            .acquire()
            .await
            .map_err(|e| Error::Connection(e.to_string()))?;
        guard.succeeded();
        Ok(Box::new(MySqlConnection { conn }))
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
                 WHERE table_schema = DATABASE() AND table_type = 'BASE TABLE' \
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
                "SELECT column_name, column_type, is_nullable, column_key, column_default \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 ORDER BY ordinal_position",
                std::slice::from_ref(&table_value),
            )
            .await?;
        if rows.is_empty() {
            return Ok(None);
        }

        let columns = rows
            .iter()
            .map(|row| {
                Ok(ColumnInfo {
                    name: row.get_by_name::<String>("column_name")?,
                    type_name: row.get_by_name::<String>("column_type")?,
                    nullable: row.get_by_name::<String>("is_nullable")? == "YES",
                    primary_key: row.get_by_name::<String>("column_key")? == "PRI",
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
                   AND tc.table_name = kcu.table_name \
                 WHERE tc.constraint_type = 'UNIQUE' \
                   AND tc.table_schema = DATABASE() \
                   AND tc.table_name = ? \
                 ORDER BY tc.constraint_name, kcu.ordinal_position",
                std::slice::from_ref(&table_value),
            )
            .await?;
        let unique_constraints = group_unique_constraints(&unique_rows)?;

        let check_rows = conn
            .fetch_all(
                "SELECT tc.constraint_name, cc.check_clause \
                 FROM information_schema.table_constraints tc \
                 JOIN information_schema.check_constraints cc \
                   ON tc.constraint_schema = cc.constraint_schema \
                   AND tc.constraint_name = cc.constraint_name \
                 WHERE tc.constraint_type = 'CHECK' \
                   AND tc.table_schema = DATABASE() \
                   AND tc.table_name = ?",
                std::slice::from_ref(&table_value),
            )
            .await?;
        let check_constraints = check_rows
            .iter()
            .map(|row| {
                Ok(CheckConstraint {
                    name: row.get_by_name::<String>("constraint_name")?,
                    expression: row.get_by_name::<String>("check_clause")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // MySQL's `key_column_usage` already pairs each local column with
        // its referenced column on the very same row (`referenced_table_name`/
        // `referenced_column_name`, a MySQL-specific extension beyond
        // standard `information_schema`), so composite foreign keys don't
        // need the extra care Postgres's reflection does.
        let fk_rows = conn
            .fetch_all(
                "SELECT constraint_name, column_name, referenced_table_name, referenced_column_name \
                 FROM information_schema.key_column_usage \
                 WHERE table_schema = DATABASE() \
                   AND table_name = ? \
                   AND referenced_table_name IS NOT NULL \
                 ORDER BY constraint_name, ordinal_position",
                std::slice::from_ref(&table_value),
            )
            .await?;
        let foreign_keys = group_foreign_keys(&fk_rows)?;

        // `index_name = "PRIMARY"` is the primary key's own backing index
        // (already `ColumnInfo::primary_key`), so it's excluded here.
        let index_rows = conn
            .fetch_all(
                "SELECT index_name, non_unique, column_name \
                 FROM information_schema.statistics \
                 WHERE table_schema = DATABASE() \
                   AND table_name = ? \
                   AND index_name != 'PRIMARY' \
                 ORDER BY index_name, seq_in_index",
                &[table_value],
            )
            .await?;
        let indexes = group_indexes(&index_rows)?;

        Ok(Some(TableSchema {
            name: table.to_string(),
            columns,
            unique_constraints,
            check_constraints,
            foreign_keys,
            indexes,
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

/// Groups rows of `(constraint_name, column_name, referenced_table_name,
/// referenced_column_name)` — ordered by `constraint_name` then ordinal
/// position — into one `ForeignKey` per distinct name.
fn group_foreign_keys(rows: &[Row]) -> Result<Vec<ForeignKey>> {
    let mut result: Vec<ForeignKey> = Vec::new();
    for row in rows {
        let name = row.get_by_name::<String>("constraint_name")?;
        let column = row.get_by_name::<String>("column_name")?;
        let referenced_column = row.get_by_name::<String>("referenced_column_name")?;
        match result.last_mut() {
            Some(last) if last.name == name => {
                last.columns.push(column);
                last.referenced_columns.push(referenced_column);
            }
            _ => result.push(ForeignKey {
                name,
                columns: vec![column],
                referenced_table: row.get_by_name::<String>("referenced_table_name")?,
                referenced_columns: vec![referenced_column],
            }),
        }
    }
    Ok(result)
}

/// Groups rows of `(index_name, non_unique, column_name)` — ordered by
/// `index_name` then `seq_in_index` — into one `IndexInfo` per distinct
/// name. `non_unique` is MySQL's own inverted spelling: `0` means unique.
fn group_indexes(rows: &[Row]) -> Result<Vec<IndexInfo>> {
    let mut result: Vec<IndexInfo> = Vec::new();
    for row in rows {
        let name = row.get_by_name::<String>("index_name")?;
        let column = row.get_by_name::<String>("column_name")?;
        match result.last_mut() {
            Some(last) if last.name == name => last.columns.push(column),
            _ => {
                let non_unique = row.get_by_name::<i64>("non_unique")?;
                result.push(IndexInfo {
                    name,
                    columns: vec![column],
                    unique: non_unique == 0,
                })
            }
        }
    }
    Ok(result)
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
