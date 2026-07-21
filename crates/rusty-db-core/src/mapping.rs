use crate::error::Result;
use crate::query::{Delete, Insert, Update};
use crate::row::Row;
use crate::value::Value;

/// Describes the table a struct maps to. Implemented by `#[derive(Mapped)]`
/// (from `rusty-db-derive`), not meant to be implemented by hand.
pub trait Mapped {
    /// The table name.
    const TABLE_NAME: &'static str;

    /// Column names, in field declaration order.
    const COLUMNS: &'static [&'static str];

    /// The column marked `#[table(primary_key)]`, if any.
    const PRIMARY_KEY: Option<&'static str> = None;

    /// The column marked `#[table(version)]` for optimistic locking, if
    /// any. When present, `Identifiable::update`/`delete_query` include
    /// it in their `WHERE` clause (matching the value the struct was
    /// last loaded with) — see `Session::update`/`delete`, which turn a
    /// zero-rows-affected result into `Error::Conflict` when this is set.
    const VERSION_COLUMN: Option<&'static str> = None;
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

    /// The value of `self`'s `#[table(primary_key)]` field. Used by
    /// `Session`'s identity map to key cached instances.
    fn primary_key_value(&self) -> Value;
}
