//! Database-agnostic schema introspection ("reflection"): asking a live
//! database what tables and columns it actually has, rather than relying
//! only on what the application's own `#[derive(Mapped)]` structs declare.
//!
//! Column type names are reported verbatim from each database's own
//! catalog (e.g. SQLite says `"INTEGER"`, Postgres says `"integer"`,
//! MySQL says `"bigint"`) — there's no attempt to unify them into one
//! portable type system, the same scope decision this crate already
//! makes for `Value`.

/// One column of a reflected table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnInfo {
    pub name: String,
    /// The type name exactly as the database's own catalog reports it.
    pub type_name: String,
    pub nullable: bool,
    pub primary_key: bool,
    /// This column's default, exactly as the database's own catalog
    /// reports it (e.g. a literal like `"0"`, or an expression like
    /// `"nextval('users_id_seq'::regclass)"`) — `None` if it has none.
    /// Verbatim text, like `type_name`: no attempt to parse or unify it
    /// into a portable value.
    pub default: Option<String>,
}

/// A named `UNIQUE` constraint and the column(s) it covers, in the order
/// the database's own catalog reports them. Doesn't include the primary
/// key (already its own, separate `ColumnInfo::primary_key` flag).
///
/// SQLite implements `UNIQUE` as an index rather than a true named
/// constraint, so it doesn't actually keep a name given via `CONSTRAINT
/// <name> UNIQUE (...)` — `name` there is the backing index's own name
/// instead (an explicit one, or SQLite's auto-generated
/// `sqlite_autoindex_<table>_<n>` for an inline `UNIQUE` with no index of
/// its own).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UniqueConstraint {
    pub name: String,
    pub columns: Vec<String>,
}

/// A `CHECK` constraint's expression, exactly as the database's own
/// catalog (Postgres/MySQL) or `CREATE TABLE` text (SQLite, which has no
/// catalog for these) reports it — verbatim, like `ColumnInfo::type_name`:
/// no attempt to parse or evaluate it. SQLite constraints declared without
/// an explicit `CONSTRAINT <name>` get a synthetic, positional name
/// (`"check_1"`, `"check_2"`, ...) since SQLite doesn't require one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckConstraint {
    pub name: String,
    pub expression: String,
}

/// A foreign key: the column(s) in this table, in order, that reference
/// another table's column(s) in the same order (so `columns[i]`
/// references `referenced_columns[i]`).
///
/// SQLite doesn't name foreign keys at all, so its `name` there is
/// synthetic (`"fk_1"`, `"fk_2"`, ...), not something recoverable from the
/// database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

/// A reflected index: its name, the column(s) it covers in index order,
/// and whether it enforces uniqueness. Includes indexes backing a
/// `UNIQUE` constraint (see `UniqueConstraint`) as well as plain,
/// non-unique indexes; excludes the index automatically backing the
/// primary key itself (already `ColumnInfo::primary_key`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

/// A reflected table: its name and columns, in the database's own column
/// order, plus its `UNIQUE`/`CHECK`/foreign key constraints and indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    pub unique_constraints: Vec<UniqueConstraint>,
    pub check_constraints: Vec<CheckConstraint>,
    pub foreign_keys: Vec<ForeignKey>,
    pub indexes: Vec<IndexInfo>,
}
