/// Describes the SQL syntax quirks of a specific backend so the query
/// builder can render portable statements without knowing which database
/// it's talking to.
///
/// Each driver crate provides one `Dialect` implementation (e.g. Postgres
/// uses `$1, $2, ...` placeholders, SQLite uses `?`).
pub trait Dialect: Send + Sync {
    /// Human-readable name, e.g. "postgres", "sqlite".
    fn name(&self) -> &'static str;

    /// Quote an identifier (table/column name) for safe inclusion in SQL.
    fn quote_ident(&self, ident: &str) -> String {
        format!("\"{}\"", ident.replace('"', "\"\""))
    }

    /// Render the placeholder for the `n`th (1-indexed) bound parameter.
    fn placeholder(&self, position: usize) -> String;

    /// Whether this dialect supports `RETURNING` clauses on INSERT/UPDATE/DELETE.
    fn supports_returning(&self) -> bool {
        false
    }

    /// The operator `Column::ilike`/`Expr`'s case-insensitive `LIKE` renders
    /// as. Postgres has a native `ILIKE` keyword; everywhere else this falls
    /// back to plain `LIKE`, which is already case-insensitive for common
    /// default collations/encodings on SQLite and MySQL/MariaDB (though not
    /// guaranteed if a case-sensitive collation is configured) — a portable
    /// approximation rather than a guaranteed-identical match everywhere.
    fn ilike_operator(&self) -> &'static str {
        "LIKE"
    }

    /// Whether this dialect supports two-phase (prepared) commit — a
    /// transaction that's durably prepared on one call and only later,
    /// possibly from an entirely different connection, either finalized or
    /// discarded. Postgres (`PREPARE TRANSACTION`) and MySQL/MariaDB (`XA`)
    /// both support it; SQLite doesn't (there's no concept of a prepared
    /// transaction surviving independently of its connection), so it keeps
    /// the default of `false`.
    fn supports_two_phase_commit(&self) -> bool {
        false
    }

    /// The statement that begins a transaction meant to later be prepared
    /// under `gid` (the two-phase commit's caller-chosen global
    /// transaction id). Defaults to plain `BEGIN`, since most dialects only
    /// need the id at prepare time; MySQL's `XA` transactions need it from
    /// the very start instead (`XA START '<gid>'`).
    fn begin_two_phase_sql(&self, gid: &str) -> String {
        let _ = gid;
        "BEGIN".to_string()
    }

    /// The statement(s) that prepare (first phase) a transaction started
    /// via `begin_two_phase_sql`, for `gid`. More than one statement for
    /// dialects (MySQL) that require ending the transaction proper before
    /// preparing it.
    fn prepare_two_phase_sql(&self, gid: &str) -> Vec<String> {
        vec![format!(
            "PREPARE TRANSACTION '{}'",
            escape_sql_string_literal(gid)
        )]
    }

    /// Second phase: durably finalizes the transaction already prepared
    /// under `gid`. Addressed purely by id — no connection or in-memory
    /// transaction handle needed, since a prepared transaction survives
    /// independently of both.
    fn commit_prepared_sql(&self, gid: &str) -> String {
        format!("COMMIT PREPARED '{}'", escape_sql_string_literal(gid))
    }

    /// Second phase: discards the transaction already prepared under
    /// `gid`, undoing its changes. Same no-connection-required shape as
    /// `commit_prepared_sql`.
    fn rollback_prepared_sql(&self, gid: &str) -> String {
        format!("ROLLBACK PREPARED '{}'", escape_sql_string_literal(gid))
    }
}

/// Escapes a string for safe inclusion as a single-quoted SQL string
/// literal (doubling embedded `'` characters) — used for two-phase commit
/// global transaction ids, which some dialects (MySQL's `XA`, Postgres'
/// `PREPARE TRANSACTION`) only accept as literal text, not a bound
/// parameter.
pub(crate) fn escape_sql_string_literal(s: &str) -> String {
    s.replace('\'', "''")
}

/// `$1`, `$2`, ... style placeholders (PostgreSQL).
#[derive(Debug, Default, Clone, Copy)]
pub struct NumberedDialect;

impl Dialect for NumberedDialect {
    fn name(&self) -> &'static str {
        "postgres"
    }

    fn placeholder(&self, position: usize) -> String {
        format!("${position}")
    }

    fn supports_returning(&self) -> bool {
        true
    }

    fn ilike_operator(&self) -> &'static str {
        "ILIKE"
    }

    fn supports_two_phase_commit(&self) -> bool {
        true
    }
}

/// `?` style placeholders (SQLite).
#[derive(Debug, Default, Clone, Copy)]
pub struct QuestionMarkDialect;

impl Dialect for QuestionMarkDialect {
    fn name(&self) -> &'static str {
        "sqlite"
    }

    fn placeholder(&self, _position: usize) -> String {
        "?".to_string()
    }
}

/// `?` style placeholders with backtick-quoted identifiers (MySQL/MariaDB).
#[derive(Debug, Default, Clone, Copy)]
pub struct MySqlDialect;

impl Dialect for MySqlDialect {
    fn name(&self) -> &'static str {
        "mysql"
    }

    fn quote_ident(&self, ident: &str) -> String {
        format!("`{}`", ident.replace('`', "``"))
    }

    fn placeholder(&self, _position: usize) -> String {
        "?".to_string()
    }

    fn supports_two_phase_commit(&self) -> bool {
        true
    }

    fn begin_two_phase_sql(&self, gid: &str) -> String {
        format!("XA START '{}'", escape_sql_string_literal(gid))
    }

    fn prepare_two_phase_sql(&self, gid: &str) -> Vec<String> {
        let gid = escape_sql_string_literal(gid);
        vec![format!("XA END '{gid}'"), format!("XA PREPARE '{gid}'")]
    }

    fn commit_prepared_sql(&self, gid: &str) -> String {
        format!("XA COMMIT '{}'", escape_sql_string_literal(gid))
    }

    fn rollback_prepared_sql(&self, gid: &str) -> String {
        format!("XA ROLLBACK '{}'", escape_sql_string_literal(gid))
    }
}
