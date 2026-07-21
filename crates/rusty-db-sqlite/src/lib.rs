//! SQLite `Driver` implementation for rusty_db, built on `sqlx`.

use std::sync::Arc;

use async_trait::async_trait;
use sqlx::pool::PoolConnection;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{
    Column as _, Connection as _, Row as _, Sqlite, SqlitePool, TypeInfo as _, ValueRef as _,
};

use rusty_db_core::dialect::QuestionMarkDialect;
use rusty_db_core::value::array_to_json;
use rusty_db_core::{
    ColumnInfo, Connection, Dialect, Driver, Engine, Error, Executor, ForeignKey, IndexInfo,
    PoolConfig, PoolMetrics, PoolStats, Result, Row, TableSchema, UniqueConstraint, Value,
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
                let connect_options: SqliteConnectOptions = url
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
        let quoted_table = DIALECT.quote_ident(table);
        let rows = conn
            .fetch_all(&format!("PRAGMA table_info({quoted_table})"), &[])
            .await?;

        let columns = rows
            .iter()
            .map(|row| {
                Ok(ColumnInfo {
                    name: row.get_by_name::<String>("name")?,
                    type_name: row.get_by_name::<String>("type")?,
                    nullable: row.get_by_name::<i64>("notnull")? == 0,
                    primary_key: row.get_by_name::<i64>("pk")? > 0,
                    default: row.get_by_name::<Option<String>>("dflt_value")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        // SQLite implements UNIQUE constraints as unique indexes; the one
        // backing the primary key itself (origin = "pk") is excluded from
        // both `indexes` and `unique_constraints`, since that's already
        // `ColumnInfo::primary_key`.
        let index_rows = conn
            .fetch_all(&format!("PRAGMA index_list({quoted_table})"), &[])
            .await?;
        let mut unique_constraints = Vec::new();
        let mut indexes = Vec::new();
        for index_row in &index_rows {
            let is_unique = index_row.get_by_name::<i64>("unique")? != 0;
            let origin = index_row.get_by_name::<String>("origin")?;
            if origin == "pk" {
                continue;
            }
            let index_name = index_row.get_by_name::<String>("name")?;
            let info_rows = conn
                .fetch_all(
                    &format!("PRAGMA index_info({})", DIALECT.quote_ident(&index_name)),
                    &[],
                )
                .await?;
            let mut columns = Vec::with_capacity(info_rows.len());
            for info_row in &info_rows {
                if let Some(name) = info_row.get_by_name::<Option<String>>("name")? {
                    columns.push(name);
                }
            }
            if columns.is_empty() {
                continue;
            }
            indexes.push(IndexInfo {
                name: index_name.clone(),
                columns: columns.clone(),
                unique: is_unique,
            });
            if is_unique {
                unique_constraints.push(UniqueConstraint {
                    name: index_name,
                    columns,
                });
            }
        }

        // SQLite has no catalog for CHECK constraints at all — the only
        // place they exist is the table's own CREATE TABLE text.
        let ddl_row = conn
            .fetch_optional(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = ?",
                &[Value::Text(table.to_string())],
            )
            .await?;
        let check_constraints = match ddl_row
            .map(|row| row.get_by_name::<Option<String>>("sql"))
            .transpose()?
            .flatten()
        {
            Some(ddl) => check_constraints::parse(&ddl),
            None => Vec::new(),
        };

        // SQLite doesn't name foreign keys at all; rows sharing the same
        // `id` belong to the same (possibly composite) key, ordered by
        // `seq`, so a synthetic, positional name is the best available.
        let fk_rows = conn
            .fetch_all(&format!("PRAGMA foreign_key_list({quoted_table})"), &[])
            .await?;
        let mut foreign_keys: Vec<ForeignKey> = Vec::new();
        let mut last_id: Option<i64> = None;
        for fk_row in &fk_rows {
            let id = fk_row.get_by_name::<i64>("id")?;
            let column = fk_row.get_by_name::<String>("from")?;
            let referenced_column = fk_row.get_by_name::<String>("to")?;
            if last_id == Some(id) {
                let last = foreign_keys.last_mut().expect("last_id implies an entry");
                last.columns.push(column);
                last.referenced_columns.push(referenced_column);
            } else {
                foreign_keys.push(ForeignKey {
                    name: format!("fk_{}", foreign_keys.len() + 1),
                    columns: vec![column],
                    referenced_table: fk_row.get_by_name::<String>("table")?,
                    referenced_columns: vec![referenced_column],
                });
                last_id = Some(id);
            }
        }

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

/// A best-effort text scan for `CHECK (...)` clauses in a `CREATE TABLE`
/// statement's own text — SQLite has no catalog view for these the way
/// Postgres/MySQL do, so this is the only source available. This is a
/// small tokenizer, not a full SQL parser: it understands quoted string/
/// identifier literals (so a `CHECK` keyword or stray paren inside one is
/// never mistaken for a real clause) and balanced parentheses, but nothing
/// more exotic about SQLite's grammar. An inline `CHECK (...)` without an
/// explicit `CONSTRAINT <name>` gets a synthetic, positional name
/// (`"check_1"`, `"check_2"`, ...), since SQLite doesn't require one.
mod check_constraints {
    use rusty_db_core::CheckConstraint;

    #[derive(Debug)]
    enum Tok {
        Word(String),
        Quoted(String),
        Punct(char),
    }

    struct Token {
        kind: Tok,
        start: usize,
        end: usize,
    }

    fn tokenize(chars: &[char]) -> Vec<Token> {
        let n = chars.len();
        let mut i = 0;
        let mut tokens = Vec::new();
        while i < n {
            let c = chars[i];
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            if c == '\'' || c == '"' || c == '`' {
                let start = i;
                let end = skip_quoted(chars, i, c);
                tokens.push(Token {
                    kind: Tok::Quoted(
                        chars[start + 1..end.saturating_sub(1).max(start + 1)]
                            .iter()
                            .collect(),
                    ),
                    start,
                    end,
                });
                i = end;
                continue;
            }
            if c == '[' {
                let start = i;
                let end = skip_through(chars, i, ']');
                tokens.push(Token {
                    kind: Tok::Quoted(
                        chars[start + 1..end.saturating_sub(1).max(start + 1)]
                            .iter()
                            .collect(),
                    ),
                    start,
                    end,
                });
                i = end;
                continue;
            }
            if c.is_alphanumeric() || c == '_' {
                let start = i;
                while i < n && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                tokens.push(Token {
                    kind: Tok::Word(chars[start..i].iter().collect()),
                    start,
                    end: i,
                });
                continue;
            }
            tokens.push(Token {
                kind: Tok::Punct(c),
                start: i,
                end: i + 1,
            });
            i += 1;
        }
        tokens
    }

    /// `chars[open]` is the opening quote; returns the index just past the
    /// matching close, handling `''`/`""`/` `` ` as an escaped literal quote.
    fn skip_quoted(chars: &[char], open: usize, quote: char) -> usize {
        let n = chars.len();
        let mut i = open + 1;
        while i < n {
            if chars[i] == quote {
                if i + 1 < n && chars[i + 1] == quote {
                    i += 2;
                    continue;
                }
                return i + 1;
            }
            i += 1;
        }
        n
    }

    fn skip_through(chars: &[char], open: usize, close: char) -> usize {
        let n = chars.len();
        let mut i = open + 1;
        while i < n && chars[i] != close {
            i += 1;
        }
        if i < n {
            i + 1
        } else {
            n
        }
    }

    fn matching_paren(tokens: &[Token], open_idx: usize) -> Option<usize> {
        let mut depth = 0;
        for (i, tok) in tokens.iter().enumerate().skip(open_idx) {
            match tok.kind {
                Tok::Punct('(') => depth += 1,
                Tok::Punct(')') => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(i);
                    }
                }
                _ => {}
            }
        }
        None
    }

    /// If `tokens[check_idx]` (the `CHECK` keyword) is immediately preceded
    /// by `CONSTRAINT <name>`, that name; otherwise `None`.
    fn preceding_constraint_name(tokens: &[Token], check_idx: usize) -> Option<String> {
        if check_idx < 2 {
            return None;
        }
        let name = match &tokens[check_idx - 1].kind {
            Tok::Word(w) => w.clone(),
            Tok::Quoted(q) => q.clone(),
            Tok::Punct(_) => return None,
        };
        match &tokens[check_idx - 2].kind {
            Tok::Word(w) if w.eq_ignore_ascii_case("CONSTRAINT") => Some(name),
            _ => None,
        }
    }

    pub(crate) fn parse(ddl: &str) -> Vec<CheckConstraint> {
        let chars: Vec<char> = ddl.chars().collect();
        let tokens = tokenize(&chars);
        let mut constraints = Vec::new();
        let mut anonymous = 0;

        let mut idx = 0;
        while idx < tokens.len() {
            let is_check =
                matches!(&tokens[idx].kind, Tok::Word(w) if w.eq_ignore_ascii_case("CHECK"));
            if is_check {
                if let Some(open_tok) = tokens.get(idx + 1) {
                    if matches!(open_tok.kind, Tok::Punct('(')) {
                        if let Some(close_idx) = matching_paren(&tokens, idx + 1) {
                            let expr_start = tokens[idx + 1].end;
                            let expr_end = tokens[close_idx].start;
                            let expression: String = chars[expr_start..expr_end].iter().collect();
                            let name =
                                preceding_constraint_name(&tokens, idx).unwrap_or_else(|| {
                                    anonymous += 1;
                                    format!("check_{anonymous}")
                                });
                            constraints.push(CheckConstraint {
                                name,
                                expression: expression.trim().to_string(),
                            });
                            idx = close_idx + 1;
                            continue;
                        }
                    }
                }
            }
            idx += 1;
        }

        constraints
    }

    #[cfg(test)]
    mod tests {
        use super::parse;

        #[test]
        fn extracts_a_named_check_constraint() {
            let ddl = "CREATE TABLE t (id INTEGER, age INTEGER, \
                       CONSTRAINT age_check CHECK (age >= 0))";
            let constraints = parse(ddl);
            assert_eq!(constraints.len(), 1);
            assert_eq!(constraints[0].name, "age_check");
            assert_eq!(constraints[0].expression, "age >= 0");
        }

        #[test]
        fn assigns_synthetic_names_to_anonymous_constraints_in_order() {
            let ddl = "CREATE TABLE t (id INTEGER, age INTEGER, \
                       CHECK (age >= 0), CHECK (age < 150))";
            let constraints = parse(ddl);
            assert_eq!(constraints.len(), 2);
            assert_eq!(constraints[0].name, "check_1");
            assert_eq!(constraints[0].expression, "age >= 0");
            assert_eq!(constraints[1].name, "check_2");
            assert_eq!(constraints[1].expression, "age < 150");
        }

        #[test]
        fn an_inline_column_level_check_is_still_found() {
            let ddl = "CREATE TABLE t (age INTEGER CHECK (age >= 0))";
            let constraints = parse(ddl);
            assert_eq!(constraints.len(), 1);
            assert_eq!(constraints[0].expression, "age >= 0");
        }

        #[test]
        fn handles_nested_parens_and_string_literals_inside_the_expression() {
            let ddl = "CREATE TABLE t (status TEXT, \
                       CHECK (status IN ('a)b', 'c') AND (1 = 1)))";
            let constraints = parse(ddl);
            assert_eq!(constraints.len(), 1);
            assert_eq!(
                constraints[0].expression,
                "status IN ('a)b', 'c') AND (1 = 1)"
            );
        }

        #[test]
        fn a_check_keyword_inside_a_string_literal_is_not_mistaken_for_a_constraint() {
            let ddl = "CREATE TABLE t (note TEXT DEFAULT 'please CHECK (this) later')";
            assert!(parse(ddl).is_empty());
        }

        #[test]
        fn a_table_with_no_check_constraints_yields_none() {
            let ddl = "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT NOT NULL)";
            assert!(parse(ddl).is_empty());
        }
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
                // SQLite has no native UUID type; bind its hyphenated
                // string form, same as any other text value.
                Value::Uuid(u) => query.bind(u.to_string()),
                // SQLite has no native NUMERIC/DECIMAL type either; bind
                // its decimal text form, same as any other text value.
                Value::Decimal(d) => query.bind(d.to_string()),
                // SQLite has no native JSON type either; bind its text
                // form, same as any other text value.
                Value::Json(j) => query.bind(j.to_string()),
                // SQLite has no native DATE/TIME/DATETIME type either;
                // bind each's ISO 8601 text form, same as any other text
                // value — `FromValue` parses that same form back.
                Value::Date(d) => query.bind(d.to_string()),
                Value::Time(t) => query.bind(t.to_string()),
                Value::DateTime(dt) => query.bind(dt.to_string()),
                // Same reasoning, but RFC 3339 (so the offset survives),
                // since SQLite has no native TIMESTAMPTZ-equivalent type.
                Value::Timestamp(ts) => query.bind(ts.to_rfc3339()),
                // SQLite has no native array column type at all either;
                // bind its JSON array text form, same as any other text
                // value.
                Value::Array(items) => query.bind(array_to_json(items).to_string()),
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

impl Connection for SqliteConnection {
    fn cached_statement_count(&self) -> usize {
        self.conn.cached_statements_size()
    }
}
