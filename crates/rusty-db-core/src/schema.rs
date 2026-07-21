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
}

/// A reflected table: its name and columns, in the database's own column
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableSchema {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
}
