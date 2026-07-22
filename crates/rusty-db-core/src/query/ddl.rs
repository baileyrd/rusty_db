use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

/// A portable column type for `CreateTable`, translated to each dialect's
/// own `CREATE TABLE` spelling by `Dialect::column_type_sql` — the same
/// native-vs-fallback split already documented on `Value`'s own variants
/// (e.g. `Uuid` is a native column type on Postgres, `TEXT`/`CHAR(36)`
/// elsewhere).
#[derive(Debug, Clone, Copy, PartialEq)]
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

#[derive(Debug, Clone)]
struct AddColumnDef {
    name: String,
    ty: ColumnType,
    nullable: bool,
    default: Option<String>,
}

// The shared `*Column` postfix is the point, not an accident worth
// renaming away: every variant genuinely is a column-level operation.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone)]
enum AlterOperation {
    AddColumn(AddColumnDef),
    DropColumn(String),
    RenameColumn { old_name: String, new_name: String },
}

/// Builds a portable `ALTER TABLE` statement — always exactly *one*
/// operation (`ADD COLUMN`, `DROP COLUMN`, or `RENAME COLUMN`), never
/// several combined into one statement: Postgres and MySQL/MariaDB both
/// allow comma-separating multiple actions in a single `ALTER TABLE`, but
/// SQLite only ever allows one action per statement, so combining several
/// would render differently — or fail outright — per dialect. Modifying
/// several columns means calling `engine.execute(...)` once per
/// `AlterTable`, which works identically everywhere.
///
/// `RENAME COLUMN <old> TO <new>` happens to be spelled identically on
/// all three dialects (SQLite since 3.25.0, MySQL/MariaDB since 8.0/10.5.2,
/// both well below anything this crate otherwise assumes), so
/// `rename_column` needs no per-dialect branching the way `add_column`'s
/// type spelling does.
///
/// Adding a `NOT NULL` column with no default to a table that already has
/// rows is rejected by the database itself (there'd be no value for
/// existing rows) — this isn't checked here, the same "let the database
/// enforce its own rules" approach `CreateTable`'s `CHECK`/`DEFAULT`
/// already take.
///
/// **A real caveat, not a hypothetical one:** on SQLite, a connection that
/// already had a table's *pre-`ALTER`* shape "in view" can panic (inside
/// the underlying `sqlx-sqlite` driver, not this crate) if it's used to
/// query that table again right after altering it — a long-standing
/// upstream limitation in how SQLite's own statement caching interacts
/// with schema changes (see [`launchbadge/sqlx#296`](https://github.com/launchbadge/sqlx/issues/296)).
/// Postgres has a related but much gentler version of the same thing: a
/// `SELECT *`-shaped statement already prepared against the old shape can
/// fail with a clean, catchable `"cached plan must not change result
/// type"` error if reused on the same connection afterward — a normal
/// consequence of Postgres's own server-side prepared statements, not a
/// bug. MySQL/MariaDB has neither issue. On any dialect, the safe pattern
/// is the same: get a fresh `Engine` (a new connection pool) before
/// reading from a table you just ran `AlterTable` against, rather than
/// continuing to use the one that issued the `ALTER TABLE` itself.
#[derive(Debug, Clone)]
pub struct AlterTable {
    table: String,
    operation: AlterOperation,
}

impl AlterTable {
    /// `ALTER TABLE <table> ADD COLUMN <name> <ty>`, nullable by default —
    /// chain `.not_null()`/`.default_raw(...)` right after to modify it.
    pub fn add_column(table: impl Into<String>, name: impl Into<String>, ty: ColumnType) -> Self {
        AlterTable {
            table: table.into(),
            operation: AlterOperation::AddColumn(AddColumnDef {
                name: name.into(),
                ty,
                nullable: true,
                default: None,
            }),
        }
    }

    /// `ALTER TABLE <table> DROP COLUMN <name>`.
    pub fn drop_column(table: impl Into<String>, name: impl Into<String>) -> Self {
        AlterTable {
            table: table.into(),
            operation: AlterOperation::DropColumn(name.into()),
        }
    }

    /// `ALTER TABLE <table> RENAME COLUMN <old_name> TO <new_name>` —
    /// preserves the column's data, unlike issuing a `drop_column` plus an
    /// `add_column` for what was actually just a rename.
    pub fn rename_column(
        table: impl Into<String>,
        old_name: impl Into<String>,
        new_name: impl Into<String>,
    ) -> Self {
        AlterTable {
            table: table.into(),
            operation: AlterOperation::RenameColumn {
                old_name: old_name.into(),
                new_name: new_name.into(),
            },
        }
    }

    fn added_column(&mut self) -> &mut AddColumnDef {
        match &mut self.operation {
            AlterOperation::AddColumn(def) => def,
            AlterOperation::DropColumn(_) | AlterOperation::RenameColumn { .. } => {
                panic!("AlterTable: .not_null()/.default_raw() only apply to .add_column(...)")
            }
        }
    }

    /// The column being added may not be `NULL`. Only meaningful after
    /// `add_column`; panics after `drop_column`.
    pub fn not_null(mut self) -> Self {
        self.added_column().nullable = false;
        self
    }

    /// A raw SQL fragment as the added column's `DEFAULT` — the same
    /// raw-SQL convention `CreateTable::default_raw`/`Insert::raw_value`
    /// use. Only meaningful after `add_column`; panics after `drop_column`.
    pub fn default_raw(mut self, raw_sql: impl Into<String>) -> Self {
        self.added_column().default = Some(raw_sql.into());
        self
    }
}

impl ToSql for AlterTable {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let sql = match &self.operation {
            AlterOperation::AddColumn(def) => {
                let mut line = format!(
                    "ALTER TABLE {} ADD COLUMN {} {}",
                    dialect.quote_ident(&self.table),
                    dialect.quote_ident(&def.name),
                    dialect.column_type_sql(&def.ty)
                );
                if !def.nullable {
                    line.push_str(" NOT NULL");
                }
                if let Some(default) = &def.default {
                    line.push_str(" DEFAULT ");
                    line.push_str(default);
                }
                line
            }
            AlterOperation::DropColumn(name) => format!(
                "ALTER TABLE {} DROP COLUMN {}",
                dialect.quote_ident(&self.table),
                dialect.quote_ident(name)
            ),
            AlterOperation::RenameColumn { old_name, new_name } => format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                dialect.quote_ident(&self.table),
                dialect.quote_ident(old_name),
                dialect.quote_ident(new_name)
            ),
        };
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
