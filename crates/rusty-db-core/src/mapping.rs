use crate::error::Result;
use crate::query::{Delete, Insert, Update};
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

/// Produces the `Insert` for `self`. Implemented by every
/// `#[derive(Mapped)]` type; lets `Session` queue writes for heterogeneous
/// entity types behind one trait object.
pub trait Entity: Mapped {
    fn insert(&self) -> Insert;
}

/// Produces `Update`/`Delete` statements identified by `self`'s primary key.
/// Implemented by `#[derive(Mapped)]` types that have a
/// `#[table(primary_key)]` field.
pub trait Identifiable: Entity {
    fn update(&self) -> Update;
    fn delete_query(&self) -> Delete;
}
