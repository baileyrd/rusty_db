use crate::error::Result;
use crate::row::Row;

/// Describes the table a struct maps to. Implemented by `#[derive(Mapped)]`
/// (from `rusty-db-derive`), not meant to be implemented by hand.
pub trait Mapped {
    /// The table name.
    const TABLE_NAME: &'static str;

    /// Column names, in field declaration order.
    const COLUMNS: &'static [&'static str];

    /// The column marked `#[table(primary_key)]`, if any.
    const PRIMARY_KEY: Option<&'static str> = None;
}

/// Decodes a `Row` into a concrete type. Implemented by `#[derive(Mapped)]`.
pub trait FromRow: Sized {
    fn from_row(row: &Row) -> Result<Self>;
}
