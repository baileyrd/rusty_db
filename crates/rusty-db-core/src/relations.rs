//! Batched ("select-in") eager loading for relationships between
//! `#[derive(Mapped)]` types.
//!
//! These are plain generic functions, usable directly, and are also what
//! the `#[has_many(...)]`/`#[belongs_to(...)]` derive attributes generate
//! convenience methods around. Both take a batch of already-fetched rows
//! and issue exactly one extra query for the whole batch, rather than one
//! query per row (the N+1 problem).

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mapping::{FromRow, Mapped};
use crate::query::{Select, Table};
use crate::value::{FromValue, Value};

/// Has-many eager load: given the primary keys of a batch of
/// already-fetched parents, fetch every `Child` row whose `fk_column`
/// matches one of them in a single query, grouped by that foreign key.
pub async fn load_many<Child, PK>(
    engine: &Engine,
    parent_keys: impl IntoIterator<Item = PK>,
    fk_column: &str,
) -> Result<HashMap<PK, Vec<Child>>>
where
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let keys: Vec<PK> = parent_keys.into_iter().collect();
    let mut grouped: HashMap<PK, Vec<Child>> = HashMap::new();
    if keys.is_empty() {
        return Ok(grouped);
    }

    let table = Table::new(Child::TABLE_NAME);
    let query =
        Select::from(&table).filter(table.col(fk_column).is_in(keys.into_iter().map(Into::into)));

    for row in engine.fetch_all(&query).await? {
        let key: PK = row.get_by_name(fk_column)?;
        let child = Child::from_row(&row)?;
        grouped.entry(key).or_default().push(child);
    }

    Ok(grouped)
}

/// Has-one eager load: like `load_many`, but for a relationship expected to
/// have at most one matching `Child` row per parent key. If a second
/// `Child` row turns up for the same key, returns `Error::Conflict` instead
/// of silently keeping (or silently dropping) one of them — the validation
/// that the relationship really is 1:1 that expressing it as a plain
/// `has_many` (or a `belongs_to` pointed the wrong way) can't give you.
pub async fn load_has_one<Child, PK>(
    engine: &Engine,
    parent_keys: impl IntoIterator<Item = PK>,
    fk_column: &str,
) -> Result<HashMap<PK, Child>>
where
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let keys: Vec<PK> = parent_keys.into_iter().collect();
    let mut by_key: HashMap<PK, Child> = HashMap::new();
    if keys.is_empty() {
        return Ok(by_key);
    }

    let table = Table::new(Child::TABLE_NAME);
    let query =
        Select::from(&table).filter(table.col(fk_column).is_in(keys.into_iter().map(Into::into)));

    for row in engine.fetch_all(&query).await? {
        let key: PK = row.get_by_name(fk_column)?;
        let child = Child::from_row(&row)?;
        if by_key.insert(key, child).is_some() {
            return Err(Error::Conflict(format!(
                "has_one: more than one {:?} row shares the same {fk_column:?} value, \
                 so this isn't actually a one-to-one relationship",
                Child::TABLE_NAME
            )));
        }
    }

    Ok(by_key)
}

/// Belongs-to (many-to-one) eager load: given the foreign key values of a
/// batch of already-fetched children, fetch every distinct `Parent` row
/// they reference in a single query, keyed by `parent_key_column`.
pub async fn load_one<Parent, PK>(
    engine: &Engine,
    foreign_keys: impl IntoIterator<Item = PK>,
    parent_key_column: &str,
) -> Result<HashMap<PK, Parent>>
where
    Parent: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let keys: HashSet<PK> = foreign_keys.into_iter().collect();
    let mut by_key: HashMap<PK, Parent> = HashMap::new();
    if keys.is_empty() {
        return Ok(by_key);
    }

    let table = Table::new(Parent::TABLE_NAME);
    let query = Select::from(&table).filter(
        table
            .col(parent_key_column)
            .is_in(keys.into_iter().map(Into::into)),
    );

    for row in engine.fetch_all(&query).await? {
        let key: PK = row.get_by_name(parent_key_column)?;
        let parent = Parent::from_row(&row)?;
        by_key.insert(key, parent);
    }

    Ok(by_key)
}
