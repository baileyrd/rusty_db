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
}
