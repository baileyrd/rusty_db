use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

/// A portable column type for `CreateTable`, translated to each dialect's
/// own `CREATE TABLE` spelling by `Dialect::column_type_sql` — the same
/// native-vs-fallback split already documented on `Value`'s own variants
/// (e.g. `Uuid` is a native column type on Postgres, `TEXT`/`CHAR(36)`
/// elsewhere).
#[derive(Debug, Clone, PartialEq)]
pub enum ColumnType {
    Bool,
    I64,
    F64,
    Text,
    /// `VARCHAR(n)` — a length-bounded text column.
    VarChar(u32),
    Bytes,
    Uuid,
    /// A fixed-point decimal with `precision` total digits and `scale`
    /// digits after the decimal point (matches `Value`'s `BigDecimal`
    /// variant).
    Decimal {
        precision: u32,
        scale: u32,
    },
    Json,
    Date,
    Time,
    /// A naive (timezone-less) date and time (matches `Value`'s
    /// `NaiveDateTime`-backed variant).
    DateTime,
    /// A UTC instant (matches `Value`'s `DateTime<Utc>`-backed variant).
    TimestampTz,
}

#[derive(Debug, Clone)]
struct ColumnDef {
    name: String,
    ty: ColumnType,
    nullable: bool,
    primary_key: bool,
    autoincrement: bool,
    unique: bool,
    default: Option<String>,
}

#[derive(Debug, Clone)]
struct ForeignKeyDef {
    columns: Vec<String>,
    referenced_table: String,
    referenced_columns: Vec<String>,
}

/// Builds a portable `CREATE TABLE` statement — the query builder's
/// counterpart to `Select`/`Insert`/`Update`/`Delete`, for schema
/// definition instead of data access. Column types are given as a
/// portable `ColumnType`; `Dialect::column_type_sql` translates each to
/// the concrete backend's own spelling.
///
/// ```
/// # use rusty_db_core::{ColumnType, CreateTable, ToSql};
/// # use rusty_db_core::dialect::QuestionMarkDialect;
/// let create = CreateTable::new("users")
///     .if_not_exists()
///     .column("id", ColumnType::I64).primary_key().autoincrement()
///     .column("email", ColumnType::VarChar(255)).not_null().unique()
///     .column("bio", ColumnType::Text);
/// let (sql, _) = create.to_sql(&QuestionMarkDialect);
/// # let _ = sql;
/// ```
#[derive(Debug, Clone)]
pub struct CreateTable {
    table: String,
    if_not_exists: bool,
    columns: Vec<ColumnDef>,
    foreign_keys: Vec<ForeignKeyDef>,
    checks: Vec<String>,
}

impl CreateTable {
    pub fn new(table: impl Into<String>) -> Self {
        CreateTable {
            table: table.into(),
            if_not_exists: false,
            columns: Vec::new(),
            foreign_keys: Vec::new(),
            checks: Vec::new(),
        }
    }

    /// `CREATE TABLE IF NOT EXISTS` instead of a plain `CREATE TABLE`.
    pub fn if_not_exists(mut self) -> Self {
        self.if_not_exists = true;
        self
    }

    /// Adds a column, nullable by default. `.not_null()`/`.primary_key()`/
    /// `.autoincrement()`/`.unique()`/`.default_raw(...)` each modify the
    /// most recently added column, so chain them right after — any of them
    /// panics if called before any `.column(...)`.
    pub fn column(mut self, name: impl Into<String>, ty: ColumnType) -> Self {
        self.columns.push(ColumnDef {
            name: name.into(),
            ty,
            nullable: true,
            primary_key: false,
            autoincrement: false,
            unique: false,
            default: None,
        });
        self
    }

    fn last_column(&mut self) -> &mut ColumnDef {
        self.columns
            .last_mut()
            .expect("CreateTable: column modifier called with no preceding .column(...)")
    }

    /// The most recently added column may not be `NULL`.
    pub fn not_null(mut self) -> Self {
        self.last_column().nullable = false;
        self
    }

    /// Marks the most recently added column as (part of) the table's
    /// primary key — implies `.not_null()`. Marking exactly one column
    /// both `.primary_key()` and `.autoincrement()` renders it inline via
    /// `Dialect::autoincrement_primary_key_sql`; any other combination —
    /// including more than one `.primary_key()` column — renders as a
    /// separate `PRIMARY KEY (...)` table constraint (a composite key).
    pub fn primary_key(mut self) -> Self {
        let col = self.last_column();
        col.primary_key = true;
        col.nullable = false;
        self
    }

    /// Auto-incrementing integer primary key. Only meaningful combined
    /// with `.primary_key()`, and only supported on a `ColumnType::I64`
    /// column — `to_sql` panics otherwise.
    pub fn autoincrement(mut self) -> Self {
        self.last_column().autoincrement = true;
        self
    }

    /// The most recently added column must hold distinct values.
    pub fn unique(mut self) -> Self {
        self.last_column().unique = true;
        self
    }

    /// A raw SQL fragment (e.g. `"CURRENT_TIMESTAMP"`, `"0"`,
    /// `"'pending'"`) as the most recently added column's `DEFAULT` — the
    /// same raw-SQL convention `#[table(default = "...")]`/
    /// `Insert::raw_value` use, and just as unvalidated: it's embedded
    /// verbatim.
    pub fn default_raw(mut self, raw_sql: impl Into<String>) -> Self {
        self.last_column().default = Some(raw_sql.into());
        self
    }

    /// A `FOREIGN KEY (columns...) REFERENCES referenced_table
    /// (referenced_columns...)` table constraint.
    pub fn foreign_key(
        mut self,
        columns: impl IntoIterator<Item = impl Into<String>>,
        referenced_table: impl Into<String>,
        referenced_columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.foreign_keys.push(ForeignKeyDef {
            columns: columns.into_iter().map(Into::into).collect(),
            referenced_table: referenced_table.into(),
            referenced_columns: referenced_columns.into_iter().map(Into::into).collect(),
        });
        self
    }

    /// A table-level `CHECK (expression)` constraint — `expression` is raw
    /// SQL, the same convention as `default_raw`.
    pub fn check(mut self, expression: impl Into<String>) -> Self {
        self.checks.push(expression.into());
        self
    }
}

impl ToSql for CreateTable {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut parts = Vec::new();

        for col in &self.columns {
            if col.primary_key && col.autoincrement {
                assert!(
                    matches!(col.ty, ColumnType::I64),
                    "CreateTable: .autoincrement() is only supported on a ColumnType::I64 \
                     column (column {:?})",
                    col.name
                );
                parts.push(format!(
                    "{} {}",
                    dialect.quote_ident(&col.name),
                    dialect.autoincrement_primary_key_sql()
                ));
                continue;
            }

            let mut line = format!(
                "{} {}",
                dialect.quote_ident(&col.name),
                dialect.column_type_sql(&col.ty)
            );
            if !col.nullable {
                line.push_str(" NOT NULL");
            }
            if col.unique {
                line.push_str(" UNIQUE");
            }
            if let Some(default) = &col.default {
                line.push_str(" DEFAULT ");
                line.push_str(default);
            }
            parts.push(line);
        }

        let pk_columns: Vec<&str> = self
            .columns
            .iter()
            .filter(|c| c.primary_key && !c.autoincrement)
            .map(|c| c.name.as_str())
            .collect();
        if !pk_columns.is_empty() {
            let cols = pk_columns
                .iter()
                .map(|c| dialect.quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!("PRIMARY KEY ({cols})"));
        }

        for fk in &self.foreign_keys {
            let cols = fk
                .columns
                .iter()
                .map(|c| dialect.quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            let ref_cols = fk
                .referenced_columns
                .iter()
                .map(|c| dialect.quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            parts.push(format!(
                "FOREIGN KEY ({cols}) REFERENCES {} ({ref_cols})",
                dialect.quote_ident(&fk.referenced_table)
            ));
        }

        for check in &self.checks {
            parts.push(format!("CHECK ({check})"));
        }

        let if_not_exists = if self.if_not_exists {
            "IF NOT EXISTS "
        } else {
            ""
        };
        let sql = format!(
            "CREATE TABLE {if_not_exists}{} ({})",
            dialect.quote_ident(&self.table),
            parts.join(", ")
        );
        (sql, Vec::new())
    }
}

/// Builds a portable `DROP TABLE` statement.
#[derive(Debug, Clone)]
pub struct DropTable {
    table: String,
    if_exists: bool,
}

impl DropTable {
    pub fn new(table: impl Into<String>) -> Self {
        DropTable {
            table: table.into(),
            if_exists: false,
        }
    }

    /// `DROP TABLE IF EXISTS` instead of a plain `DROP TABLE`.
    pub fn if_exists(mut self) -> Self {
        self.if_exists = true;
        self
    }
}

impl ToSql for DropTable {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let if_exists = if self.if_exists { "IF EXISTS " } else { "" };
        (
            format!("DROP TABLE {if_exists}{}", dialect.quote_ident(&self.table)),
            Vec::new(),
        )
    }
}

/// Builds a portable `CREATE INDEX` statement.
#[derive(Debug, Clone)]
pub struct CreateIndex {
    name: String,
    table: String,
    columns: Vec<String>,
    unique: bool,
    if_not_exists: bool,
}

impl CreateIndex {
    pub fn new(
        name: impl Into<String>,
        table: impl Into<String>,
        columns: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        CreateIndex {
            name: name.into(),
            table: table.into(),
            columns: columns.into_iter().map(Into::into).collect(),
            unique: false,
            if_not_exists: false,
        }
    }

    /// `CREATE UNIQUE INDEX` instead of a plain `CREATE INDEX`.
    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    /// `CREATE INDEX IF NOT EXISTS` instead of a plain `CREATE INDEX`.
    pub fn if_not_exists(mut self) -> Self {
        self.if_not_exists = true;
        self
    }
}

impl ToSql for CreateIndex {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let unique = if self.unique { "UNIQUE " } else { "" };
        let if_not_exists = if self.if_not_exists {
            "IF NOT EXISTS "
        } else {
            ""
        };
        let cols = self
            .columns
            .iter()
            .map(|c| dialect.quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "CREATE {unique}INDEX {if_not_exists}{} ON {} ({cols})",
            dialect.quote_ident(&self.name),
            dialect.quote_ident(&self.table),
        );
        (sql, Vec::new())
    }
}

/// Builds a portable `DROP INDEX` statement. MySQL/MariaDB needs `ON
/// <table>` to identify the index (an index name is only unique within
/// its table there); Postgres/SQLite don't — see
/// `Dialect::drop_index_needs_table_name`.
#[derive(Debug, Clone)]
pub struct DropIndex {
    name: String,
    table: String,
    if_exists: bool,
}

impl DropIndex {
    pub fn new(name: impl Into<String>, table: impl Into<String>) -> Self {
        DropIndex {
            name: name.into(),
            table: table.into(),
            if_exists: false,
        }
    }

    /// `DROP INDEX IF EXISTS` instead of a plain `DROP INDEX`.
    pub fn if_exists(mut self) -> Self {
        self.if_exists = true;
        self
    }
}

impl ToSql for DropIndex {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let if_exists = if self.if_exists { "IF EXISTS " } else { "" };
        let sql = if dialect.drop_index_needs_table_name() {
            format!(
                "DROP INDEX {if_exists}{} ON {}",
                dialect.quote_ident(&self.name),
                dialect.quote_ident(&self.table)
            )
        } else {
            format!("DROP INDEX {if_exists}{}", dialect.quote_ident(&self.name))
        };
        (sql, Vec::new())
    }
}
