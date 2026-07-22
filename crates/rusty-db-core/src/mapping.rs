use crate::error::Result;
use crate::query::{Delete, Expr, Insert, Table, Update};
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

    /// The (boolean) column marked `#[table(soft_delete)]`, if any. When
    /// present, `Session::delete` marks the row (`SET <column> = true`)
    /// instead of actually removing it, and `Session::get` treats an
    /// already-marked row as not found. See `not_deleted_filter` for
    /// building the same "still active" condition into your own queries.
    const SOFT_DELETE_COLUMN: Option<&'static str> = None;

    /// A `<column> = false` filter excluding soft-deleted rows, or `None`
    /// for a type with no `#[table(soft_delete)]` column. Needs no
    /// per-type code generation — built entirely from `TABLE_NAME` and
    /// `SOFT_DELETE_COLUMN` — so every `Mapped` type gets it for free.
    fn not_deleted_filter() -> Option<Expr>
    where
        Self: Sized,
    {
        Self::SOFT_DELETE_COLUMN.map(|column| Table::new(Self::TABLE_NAME).col(column).eq(false))
    }
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

/// Optional application code run around a write, for a type that
/// implements this by hand alongside `#[derive(Mapped)]` — hook bodies are
/// arbitrary application logic, so this isn't (and can't be) derived,
/// the same reasoning behind `Into<Value>`/`FromValue` being available as
/// a hand-written escape hatch alongside `MappedEnum`/`MappedNewtype`.
/// Every method has a no-op (or `Ok(())`) default, so implementing only
/// the ones a given type needs is fine.
///
/// Nothing calls these automatically: `Session::add`/`update`/`delete`
/// stay exactly as they were before this trait existed (unhooked,
/// infallible, `&T`) for every existing caller. Opt in per-call instead,
/// with `Session::add_mut`/`update_mut`/`delete_mut`.
pub trait Lifecycle: Sized {
    /// Runs before the row is queued for insertion — the only hook able to
    /// mutate `self` before its data is captured into the `INSERT`.
    fn before_insert(&mut self) {}
    /// Runs once the insert has actually succeeded (never for a write that
    /// failed and rolled back), on a snapshot of `self` taken right after
    /// `before_insert`/`validate` ran.
    fn after_insert(&self) {}
    /// Runs before the row is queued for update — the only hook able to
    /// mutate `self` before its data is captured into the `UPDATE`.
    fn before_update(&mut self) {}
    /// Runs once the update has actually succeeded (never for a write that
    /// failed and rolled back), on a snapshot of `self` taken right after
    /// `before_update`/`validate` ran.
    fn after_update(&self) {}
    /// Runs before the row is queued for deletion.
    fn before_delete(&mut self) {}
    /// Runs once the delete has actually succeeded (never for a write that
    /// failed and rolled back), on a snapshot of `self` taken right after
    /// `before_delete` ran.
    fn after_delete(&self) {}
    /// Runs after `before_insert`/`before_update` (never for a delete),
    /// before anything is queued — returning `Err` rejects the write
    /// outright, so it's never sent to the database at all, rather than
    /// failing later at flush/commit time.
    fn validate(&self) -> Result<()> {
        Ok(())
    }
}
