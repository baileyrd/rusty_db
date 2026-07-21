//! MySQL/MariaDB `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow};
use sqlx::pool::PoolConnection;
use sqlx::types::chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use sqlx::{Column as _, Connection as _, MySql, MySqlPool, Row as _, TypeInfo as _};

use rusty_db_core::dialect::MySqlDialect;
use rusty_db_core::value::array_to_json;
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
        if let Some(sql) = config.on_connect {
            options = options.after_connect(move |conn, _meta| {
                let sql = Arc::clone(&sql);
                Box::pin(async move {
                    sqlx::query(&sql).execute(conn).await?;
                    Ok(())
                })
            });
        }
        if let Some(sql) = config.before_acquire {
            options = options.before_acquire(move |conn, _meta| {
                let sql = Arc::clone(&sql);
                Box::pin(async move {
                    sqlx::query(&sql).execute(conn).await?;
                    Ok(true)
                })
            });
        }
        if let Some(sql) = config.after_release {
            options = options.after_release(move |conn, _meta| {
                let sql = Arc::clone(&sql);
                Box::pin(async move {
                    sqlx::query(&sql).execute(conn).await?;
                    Ok(true)
                })
            });
        }
        let pool = match config.statement_cache_capacity {
            Some(capacity) => {
                let connect_options: MySqlConnectOptions = url
                    .parse()
                    .map_err(|e: sqlx::Error| Error::Connection(e.to_string()))?;
                options
                    .connect_with(connect_options.statement_cache_capacity(capacity))
                    .await
            }
            None => options.connect(url).await,
        }
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
                let nullable = row.get_by_name::<String>("is_nullable")? == "YES";
                Ok(ColumnInfo {
                    name: row.get_by_name::<String>("column_name")?,
                    type_name: row.get_by_name::<String>("column_type")?,
                    nullable,
                    primary_key: row.get_by_name::<String>("column_key")? == "PRI",
                    default: normalize_default(
                        nullable,
                        row.get_by_name::<Option<String>>("column_default")?,
                    ),
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

/// MariaDB/MySQL's `information_schema.columns.column_default` reports the
/// literal, unquoted text `"NULL"` for a nullable column with no
/// meaningful default — both when no `DEFAULT` clause was given at all,
/// and for an explicit `DEFAULT NULL` (the two are indistinguishable in
/// this text form, and behave identically anyway: a nullable column with
/// no value on `INSERT` becomes `NULL` regardless of which one it was).
/// Without this, `ColumnInfo::default` would report `Some("NULL")` for
/// every nullable column that has no real default, unlike Postgres and
/// SQLite, which both report genuine SQL `NULL` (i.e. `None`) for the
/// same case.
///
/// A *string* literal default of the four characters `NULL` is reported
/// quoted (`"'NULL'"`) and is left untouched here — it's a real default,
/// not this ambiguity.
fn normalize_default(nullable: bool, default: Option<String>) -> Option<String> {
    match default {
        Some(text) if nullable && text == "NULL" => None,
        other => other,
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
            // each needs its own typed decode via `chrono`.
            "DATE" => row
                .try_get::<Option<NaiveDate>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Date)
                .unwrap_or(Value::Null),
            "TIME" => row
                .try_get::<Option<NaiveTime>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Time)
                .unwrap_or(Value::Null),
            // DATETIME has no time zone at all in MySQL's own semantics;
            // TIMESTAMP, unlike DATETIME, always stores and reports as UTC
            // (converting to/from the session's own time zone on the
            // wire), so it maps to Value::Timestamp instead — same
            // Postgres TIMESTAMP-vs-TIMESTAMPTZ split, by name rather than
            // by wire format (both are the same packed bytes on MySQL).
            "DATETIME" => row
                .try_get::<Option<NaiveDateTime>, _>(i)
                .map_err(to_core_err)?
                .map(Value::DateTime)
                .unwrap_or(Value::Null),
            "TIMESTAMP" => row
                .try_get::<Option<DateTime<Utc>>, _>(i)
                .map_err(to_core_err)?
                .map(Value::Timestamp)
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
                // MySQL/MariaDB has no native UUID type; bind its
                // hyphenated string form, same as any other text value.
                Value::Uuid(u) => query.bind(u.to_string()),
                // MySQL/MariaDB sends DECIMAL as text on its own wire
                // protocol; bind its decimal text form directly.
                Value::Decimal(d) => query.bind(d.to_string()),
                // MySQL/MariaDB sends JSON as text on its own wire
                // protocol; bind its text form directly.
                Value::Json(j) => query.bind(j.to_string()),
                // DATE/TIME/DATETIME/TIMESTAMP are all native binary wire
                // types on MySQL/MariaDB too (see row_from_mysql above);
                // bind each directly rather than as text.
                Value::Date(d) => query.bind(*d),
                Value::Time(t) => query.bind(*t),
                Value::DateTime(dt) => query.bind(*dt),
                Value::Timestamp(ts) => query.bind(*ts),
                // MySQL/MariaDB has no native array column type at all;
                // bind its JSON array text form instead, same as any other
                // text value.
                Value::Array(items) => query.bind(array_to_json(items).to_string()),
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

    // MariaDB/MySQL's `XA COMMIT`/`XA ROLLBACK` don't reliably resolve a
    // transaction another connection prepared when sent through the
    // prepared-statement (`COM_STMT_PREPARE`/`COM_STMT_EXECUTE`) protocol
    // that `sqlx::query` always uses — observed directly against a real
    // server: the exact same statement text fails with `XAE04: Unknown
    // XID` over that protocol but succeeds immediately over plain text
    // SQL. `sqlx::raw_sql` sends statements as plain text (the same wire
    // format a `mysql` CLI session uses) and sidesteps it.
    async fn execute_unprepared(&mut self, sql: &str) -> Result<u64> {
        raw_execute(&mut self.conn, sql).await
    }
}

// A free function (rather than inlined into the `execute_unprepared` body
// above) sidesteps a higher-ranked-lifetime inference issue that otherwise
// shows up combining `#[async_trait]`'s boxed-future desugaring with
// `sqlx::raw_sql(..).execute(..)`'s own generic-over-`Executor` signature
// ("implementation of `sqlx::Executor` is not general enough").
async fn raw_execute(conn: &mut sqlx::MySqlConnection, sql: &str) -> Result<u64> {
    use sqlx::Executor as _;
    let result = conn
        .execute(sqlx::raw_sql(sql))
        .await
        .map_err(to_core_err)?;
    Ok(result.rows_affected())
}

#[async_trait]
impl Connection for MySqlConnection {
    // After `XA PREPARE`, MariaDB/MySQL leaves the preparing session stuck
    // — unable to run any further statement, even an unrelated one from an
    // unrelated caller — until it resolves its own prepared transaction.
    // Letting a connection in that state go back to the pool for reuse
    // would break whoever gets it next, so `Transaction::prepare` closes
    // the connection outright instead of just dropping it.
    fn cached_statement_count(&self) -> usize {
        self.conn.cached_statements_size()
    }

    async fn close(self: Box<Self>) -> Result<()> {
        let MySqlConnection { conn } = *self;
        conn.close().await.map_err(to_core_err)
    }
}
