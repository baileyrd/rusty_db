//! Batched ("select-in") eager loading for relationships between
//! `#[derive(Mapped)]` types.
//!
//! These are plain generic functions, usable directly, and are also what
//! the `#[has_many(...)]`/`#[belongs_to(...)]` derive attributes generate
//! convenience methods around. Both take a batch of already-fetched rows
//! and issue exactly one extra query for the whole batch, rather than one
//! query per row (the N+1 problem).
//!
//! The `*_via_subquery` functions further down are a second strategy for
//! the same four relationship shapes — SQLAlchemy calls this
//! `subqueryload`. Rather than collecting a batch of already-fetched
//! parent keys into Rust and shipping them back as a literal `IN (...)`
//! list (what every function above does), the caller instead hands over a
//! `Select` — however it's filtered/joined — that picks out the *parent*
//! side's key column, and the matching row is found by joining against
//! that `Select` directly, wrapped as a `WITH` CTE (see `Cte`/
//! `Select::with`), letting the database compute the matching set
//! server-side in one round trip instead of two. This is the same trick
//! that gives this crate `IN (subquery)` support and a genuine `FROM`
//! subquery for free, without a new "derived table" primitive: a CTE is
//! already just a named, referenceable result set. `#[has_many(...)]`/etc.
//! also generate a `_via_subquery`-suffixed convenience method around
//! each of these.
//!
//! `load_many_joined`/`load_has_one_joined`/`load_one_joined` are a third
//! strategy — SQLAlchemy calls this `joinedload` — for the
//! `has_many`/`has_one`/`belongs_to` shapes so far. Unlike the two above,
//! it doesn't add a second query at all: both sides come back from a
//! single `LEFT JOIN` query, one row per match (or one row with every
//! column on the "many" side's counterpart `NULL`, for an unmatched row).
//! That's a materially different shape than `load_many`/
//! `load_many_via_subquery` return — those always start from an
//! already-fetched (or independently queried) batch on one side — so
//! these fetch *and* return both sides themselves, and have to
//! deduplicate whichever side the join naturally repeats: the "one" side
//! for `has_many`/`has_one` (a parent row repeats once per matching
//! child), or the referenced side for `belongs_to` (a parent row repeats
//! once per child that references it — see `load_one_joined`'s own doc
//! for why the deduplication direction flips there). They also have to
//! solve a problem the other two strategies never hit: the two mapped
//! types' own column names can collide (both mapping an `id` primary key,
//! say), so every selected column gets an internal, uniquely prefixed
//! alias, invisible to the caller, that `FromRow` decodes back through.
//! `load_many_to_many_joined` extends the same technique across a
//! two-hop join (`Parent LEFT JOIN through_table LEFT JOIN Target`). All
//! four are wired into `#[has_many(...)]`/`#[has_one(...)]`/
//! `#[belongs_to(...)]`/`#[many_to_many(...)]` as `_joined`-suffixed
//! convenience methods, the same way `_via_subquery` already was.
//!
//! Each `*_joined` function above only accepts a plain, optional `filter`
//! on one side's own table — the `_joined_from_query` sibling next to
//! each one instead takes an arbitrary caller-built `Select` (its own
//! filters, joins, even other CTEs), the same widened contract
//! `*_via_subquery` already has, but with one added requirement: it must
//! select every column of the "from" side's own `Mapped::COLUMNS`, each
//! under its own (unaliased) column name, rather than just a single key
//! column — reconstructing a full mapped row (not just a key) is what the
//! `LEFT JOIN` needs to build. It's wrapped as a `WITH` CTE (same trick as
//! `_via_subquery`) and joined against directly instead of a plain
//! `Table::new(Parent::TABLE_NAME)`.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;

use crate::engine::Engine;
use crate::error::{Error, Result};
use crate::mapping::{FromRow, Mapped};
use crate::query::{Cte, Expr, Select, SelectExpr, Table};
use crate::row::Row;
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

/// Many-to-many eager load: given the primary keys of a batch of
/// already-fetched parents, fetch every `Target` row joined to one of them
/// through `through_table` (a join table with `local_key_column`
/// referencing the parent and `foreign_key_column` referencing
/// `target_key_column` on `Target`) in a single query, grouped by the
/// parent key.
///
/// Only `local_key_column` and `Target`'s own columns are ever selected —
/// nothing else from `through_table` — so the two can't collide unless
/// `Target` happens to have its own column named the same as
/// `local_key_column`.
pub async fn load_many_to_many<Target, PK>(
    engine: &Engine,
    parent_keys: impl IntoIterator<Item = PK>,
    through_table: &str,
    local_key_column: &str,
    foreign_key_column: &str,
    target_key_column: &str,
) -> Result<HashMap<PK, Vec<Target>>>
where
    Target: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let keys: Vec<PK> = parent_keys.into_iter().collect();
    let mut grouped: HashMap<PK, Vec<Target>> = HashMap::new();
    if keys.is_empty() {
        return Ok(grouped);
    }

    let target = Table::new(Target::TABLE_NAME);
    let through = Table::new(through_table);

    let mut select_columns = vec![through.col(local_key_column)];
    select_columns.extend(Target::COLUMNS.iter().map(|c| target.col(*c)));

    let query = Select::from(&target)
        .join(
            &through,
            target
                .col(target_key_column)
                .eq_col(&through.col(foreign_key_column)),
        )
        .columns(select_columns)
        .filter(
            through
                .col(local_key_column)
                .is_in(keys.into_iter().map(Into::into)),
        );

    for row in engine.fetch_all(&query).await? {
        let key: PK = row.get_by_name(local_key_column)?;
        let target_row = Target::from_row(&row)?;
        grouped.entry(key).or_default().push(target_row);
    }

    Ok(grouped)
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

const SUBQUERY_CTE_NAME: &str = "_rusty_db_eager_load_ids";

/// Shared machinery for every `*_via_subquery` function below: wraps
/// `ids` as a `WITH _rusty_db_eager_load_ids AS (...)` CTE, hands
/// `build_query` a `Table` reference to that CTE (referenceable exactly
/// like any other table — `cte.col(...)` — since that's all a CTE
/// reference ever is), and runs whatever `Select` it builds.
async fn query_via_subquery(
    engine: &Engine,
    ids: Select,
    build_query: impl FnOnce(&Table) -> Select,
) -> Result<Vec<Row>> {
    let cte_table = Table::new(SUBQUERY_CTE_NAME);
    let cte = Cte::new(SUBQUERY_CTE_NAME, ids);
    let query = build_query(&cte_table).with([cte]);
    engine.fetch_all(&query).await
}

/// Has-many eager load via the "subqueryload" strategy: like `load_many`,
/// but instead of a batch of already-fetched parent keys, takes
/// `parent_ids` — a `Select` selecting just one column, named
/// `parent_pk_column`, filtered/joined however the caller wants the
/// parent batch chosen (e.g. `Select::from(&users).columns([users.col("id")]).filter(...)`).
/// `Child` rows are found by joining directly against `parent_ids`
/// (wrapped as a CTE) rather than shipping a literal key list back and
/// forth — better suited when the parent batch is large or itself the
/// result of a nontrivial query; for an already-in-hand list of keys,
/// `load_many` avoids the extra CTE/join machinery.
pub async fn load_many_via_subquery<Child, PK>(
    engine: &Engine,
    parent_ids: Select,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<HashMap<PK, Vec<Child>>>
where
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let child_table = Table::new(Child::TABLE_NAME);

    let rows = query_via_subquery(engine, parent_ids, |cte| {
        Select::from(&child_table)
            .join(
                cte,
                child_table
                    .col(fk_column)
                    .eq_col(&cte.col(parent_pk_column)),
            )
            .columns(Child::COLUMNS.iter().map(|c| child_table.col(*c)))
    })
    .await?;

    let mut grouped: HashMap<PK, Vec<Child>> = HashMap::new();
    for row in rows {
        let key: PK = row.get_by_name(fk_column)?;
        let child = Child::from_row(&row)?;
        grouped.entry(key).or_default().push(child);
    }

    Ok(grouped)
}

/// Has-one eager load via the "subqueryload" strategy — see
/// `load_many_via_subquery` for what `parent_ids`/`parent_pk_column` mean,
/// and `load_has_one` for the one-to-one conflict check this shares.
pub async fn load_has_one_via_subquery<Child, PK>(
    engine: &Engine,
    parent_ids: Select,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<HashMap<PK, Child>>
where
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let child_table = Table::new(Child::TABLE_NAME);

    let rows = query_via_subquery(engine, parent_ids, |cte| {
        Select::from(&child_table)
            .join(
                cte,
                child_table
                    .col(fk_column)
                    .eq_col(&cte.col(parent_pk_column)),
            )
            .columns(Child::COLUMNS.iter().map(|c| child_table.col(*c)))
    })
    .await?;

    let mut by_key: HashMap<PK, Child> = HashMap::new();
    for row in rows {
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

/// Belongs-to (many-to-one) eager load via the "subqueryload" strategy:
/// like `load_one`, but instead of a batch of already-fetched children's
/// foreign key values, takes `foreign_key_ids` — a `Select` selecting
/// just one column, named `fk_column`, scoped to whatever child batch the
/// caller cares about. `Parent` rows are found by joining directly
/// against `foreign_key_ids` (wrapped as a CTE) rather than shipping a
/// literal key list back and forth.
pub async fn load_one_via_subquery<Parent, PK>(
    engine: &Engine,
    foreign_key_ids: Select,
    fk_column: &str,
    parent_key_column: &str,
) -> Result<HashMap<PK, Parent>>
where
    Parent: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let parent_table = Table::new(Parent::TABLE_NAME);

    let rows = query_via_subquery(engine, foreign_key_ids, |cte| {
        Select::from(&parent_table)
            .join(
                cte,
                parent_table
                    .col(parent_key_column)
                    .eq_col(&cte.col(fk_column)),
            )
            .columns(Parent::COLUMNS.iter().map(|c| parent_table.col(*c)))
    })
    .await?;

    let mut by_key: HashMap<PK, Parent> = HashMap::new();
    for row in rows {
        let key: PK = row.get_by_name(parent_key_column)?;
        let parent = Parent::from_row(&row)?;
        by_key.insert(key, parent);
    }

    Ok(by_key)
}

/// Many-to-many eager load via the "subqueryload" strategy — see
/// `load_many_to_many` for what `through_table`/`local_key_column`/
/// `foreign_key_column`/`target_key_column` mean; `parent_ids`/
/// `parent_pk_column` are the same as `load_many_via_subquery`'s.
pub async fn load_many_to_many_via_subquery<Target, PK>(
    engine: &Engine,
    parent_ids: Select,
    parent_pk_column: &str,
    through_table: &str,
    local_key_column: &str,
    foreign_key_column: &str,
    target_key_column: &str,
) -> Result<HashMap<PK, Vec<Target>>>
where
    Target: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let target = Table::new(Target::TABLE_NAME);
    let through = Table::new(through_table);

    let rows = query_via_subquery(engine, parent_ids, |cte| {
        let mut select_columns = vec![through.col(local_key_column)];
        select_columns.extend(Target::COLUMNS.iter().map(|c| target.col(*c)));
        Select::from(&target)
            .join(
                &through,
                target
                    .col(target_key_column)
                    .eq_col(&through.col(foreign_key_column)),
            )
            .join(
                cte,
                through
                    .col(local_key_column)
                    .eq_col(&cte.col(parent_pk_column)),
            )
            .columns(select_columns)
    })
    .await?;

    let mut grouped: HashMap<PK, Vec<Target>> = HashMap::new();
    for row in rows {
        let key: PK = row.get_by_name(local_key_column)?;
        let target_row = Target::from_row(&row)?;
        grouped.entry(key).or_default().push(target_row);
    }

    Ok(grouped)
}

const JOINED_PARENT_PREFIX: &str = "__rusty_db_joined_parent__";
const JOINED_CHILD_PREFIX: &str = "__rusty_db_joined_child__";
const JOINED_FROM_QUERY_CTE_NAME: &str = "_rusty_db_eager_load_joined_from";

/// A synthetic `Row` built from every column in `row` whose name starts
/// with `prefix`, with the prefix stripped back off — used to decode one
/// side of `load_many_joined`'s aliased column projection without
/// `Parent`/`Child` ever needing to know a join happened at all.
fn prefixed_sub_row(row: &Row, prefix: &str) -> Row {
    let mut columns = Vec::new();
    let mut values = Vec::new();
    for (i, name) in row.columns().iter().enumerate() {
        if let Some(stripped) = name.strip_prefix(prefix) {
            columns.push(stripped.to_string());
            values.push(row.value(i).cloned().unwrap_or(Value::Null));
        }
    }
    Row::new(columns.into(), values)
}

/// Has-many eager load via the "joined" strategy (SQLAlchemy's
/// `joinedload`): fetches every `Parent` row matching `filter` (`None`
/// for no filter) together with its matching `Child` rows in a single
/// `LEFT JOIN` query — one round trip total, not `load_many`'s two.
/// Returns the parents in the order they first appear in the joined
/// result set, deduplicated (a join naturally repeats a parent row once
/// per matching child), alongside the same `HashMap<PK, Vec<Child>>`
/// shape `load_many` returns — a parent with no matching children has no
/// entry in it, same as there.
///
/// `Parent` and `Child` may map columns with the very same name (both
/// having their own `id` primary key, say) without colliding: every
/// selected column is given an internal, uniquely prefixed alias, and
/// decoded back through it — invisible to the caller, and to `FromRow`.
///
/// Simpler than `load_many_via_subquery`'s `parent_ids: Select` — this
/// only accepts a plain `filter` on `Parent`'s own table (build it with
/// `Parent::table().col(...)`, not a fresh `Table::new(...)`, so its
/// column references resolve against the same table name this function
/// builds internally), rather than an arbitrary caller-built query, since
/// the join and column-aliasing this needs require an actual `Table`
/// handle this function controls throughout, not just a key column name.
pub async fn load_many_joined<Parent, Child, PK>(
    engine: &Engine,
    filter: Option<Expr>,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<(Vec<Parent>, HashMap<PK, Vec<Child>>)>
where
    Parent: Mapped + FromRow,
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let (parents, matches) =
        fetch_joined::<Parent, Child, PK>(engine, filter, parent_pk_column, fk_column).await?;
    Ok((parents, group_matches(matches)))
}

/// Like `load_many_joined`, but instead of a plain `filter` on `Parent`'s
/// own table, takes `parents` — an arbitrary `Select` the caller built
/// however they like (its own filters, joins, even other CTEs), as long as
/// it selects every one of `Parent::COLUMNS`, each under its own
/// (unaliased) column name — e.g.
/// `Select::from(&Parent::table()).columns(Parent::COLUMNS.iter().map(|c| Parent::table().col(c))).filter(...)`.
/// `parents` is wrapped as a `WITH` CTE (the same trick
/// `query_via_subquery` uses) and the `Child` `LEFT JOIN` runs against
/// that CTE instead of `Parent`'s table directly — this is what lets the
/// caller bring their own joins/CTEs, something `load_many_joined`'s plain
/// `filter` can't do.
pub async fn load_many_joined_from_query<Parent, Child, PK>(
    engine: &Engine,
    parents: Select,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<(Vec<Parent>, HashMap<PK, Vec<Child>>)>
where
    Parent: Mapped + FromRow,
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let (parents, matches) =
        fetch_joined_from_query::<Parent, Child, PK>(engine, parents, parent_pk_column, fk_column)
            .await?;
    Ok((parents, group_matches(matches)))
}

/// Groups a flat `(PK, Child)` match list (what `fetch_joined`/
/// `fetch_joined_from_query` return) into the `HashMap<PK, Vec<Child>>`
/// shape `load_many_joined`/`load_many_joined_from_query` return.
fn group_matches<PK, Child>(matches: Vec<(PK, Child)>) -> HashMap<PK, Vec<Child>>
where
    PK: Eq + Hash,
{
    let mut grouped: HashMap<PK, Vec<Child>> = HashMap::new();
    for (key, child) in matches {
        grouped.entry(key).or_default().push(child);
    }
    grouped
}

/// Has-one eager load via the "joined" strategy — see `load_many_joined`
/// for what `filter`/the column-aliasing/no-match detection mean, and
/// `load_has_one` for the one-to-one conflict check this shares:
/// `Err(Error::Conflict)` if more than one `Child` row ends up matching
/// the same parent, instead of silently keeping (or dropping) one of them.
pub async fn load_has_one_joined<Parent, Child, PK>(
    engine: &Engine,
    filter: Option<Expr>,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<(Vec<Parent>, HashMap<PK, Child>)>
where
    Parent: Mapped + FromRow,
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let (parents, matches) =
        fetch_joined::<Parent, Child, PK>(engine, filter, parent_pk_column, fk_column).await?;
    Ok((
        parents,
        dedup_matches::<Child, _>(matches, fk_column, Child::TABLE_NAME)?,
    ))
}

/// Like `load_has_one_joined`, but instead of a plain `filter`, takes
/// `parents` — an arbitrary `Select` on `Parent`'s own table, the same
/// contract `load_many_joined_from_query`'s `parents` has.
pub async fn load_has_one_joined_from_query<Parent, Child, PK>(
    engine: &Engine,
    parents: Select,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<(Vec<Parent>, HashMap<PK, Child>)>
where
    Parent: Mapped + FromRow,
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let (parents, matches) =
        fetch_joined_from_query::<Parent, Child, PK>(engine, parents, parent_pk_column, fk_column)
            .await?;
    Ok((
        parents,
        dedup_matches::<Child, _>(matches, fk_column, Child::TABLE_NAME)?,
    ))
}

/// Collapses a flat `(PK, Child)` match list into a `HashMap<PK, Child>`,
/// erroring with the same `Error::Conflict` message every `has_one`-shaped
/// loader in this module uses if the same key turns up twice — the
/// relationship isn't actually one-to-one.
fn dedup_matches<Child, PK>(
    matches: Vec<(PK, Child)>,
    fk_column: &str,
    child_table_name: &str,
) -> Result<HashMap<PK, Child>>
where
    PK: Eq + Hash,
{
    let mut by_key: HashMap<PK, Child> = HashMap::new();
    for (key, child) in matches {
        if by_key.insert(key, child).is_some() {
            return Err(Error::Conflict(format!(
                "has_one: more than one {child_table_name:?} row shares the same {fk_column:?} \
                 value, so this isn't actually a one-to-one relationship"
            )));
        }
    }
    Ok(by_key)
}

/// Shared machinery for `load_many_joined`/`load_has_one_joined`: builds
/// the single `LEFT JOIN` query against `Parent`'s own table (filtered by
/// `filter`, if any) and hands it to `decode_joined_rows`.
async fn fetch_joined<Parent, Child, PK>(
    engine: &Engine,
    filter: Option<Expr>,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<(Vec<Parent>, Vec<(PK, Child)>)>
where
    Parent: Mapped + FromRow,
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let parent_table = Table::new(Parent::TABLE_NAME);
    let child_table = Table::new(Child::TABLE_NAME);

    let mut select_columns: Vec<SelectExpr> = Parent::COLUMNS
        .iter()
        .map(|c| SelectExpr::from(parent_table.col(*c)).alias(format!("{JOINED_PARENT_PREFIX}{c}")))
        .collect();
    select_columns.extend(
        Child::COLUMNS.iter().map(|c| {
            SelectExpr::from(child_table.col(*c)).alias(format!("{JOINED_CHILD_PREFIX}{c}"))
        }),
    );

    let mut query = Select::from(&parent_table)
        .left_join(
            &child_table,
            parent_table
                .col(parent_pk_column)
                .eq_col(&child_table.col(fk_column)),
        )
        .columns(select_columns);
    if let Some(filter) = filter {
        query = query.filter(filter);
    }

    decode_joined_rows(engine, query, parent_pk_column, fk_column).await
}

/// Shared machinery for `load_many_joined_from_query`/
/// `load_has_one_joined_from_query`: wraps `parents` as a `WITH` CTE (see
/// `query_via_subquery`'s own doc for the same trick) and builds the
/// `LEFT JOIN` query against that CTE instead of `Parent`'s table
/// directly, then hands it to `decode_joined_rows` exactly like
/// `fetch_joined` does. `parents` must select every one of
/// `Parent::COLUMNS`, each under its own (unaliased) column name, so the
/// CTE's own column list lines up with what `Parent::from_row` expects.
async fn fetch_joined_from_query<Parent, Child, PK>(
    engine: &Engine,
    parents: Select,
    parent_pk_column: &str,
    fk_column: &str,
) -> Result<(Vec<Parent>, Vec<(PK, Child)>)>
where
    Parent: Mapped + FromRow,
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let cte_table = Table::new(JOINED_FROM_QUERY_CTE_NAME);
    let cte = Cte::new(JOINED_FROM_QUERY_CTE_NAME, parents);
    let child_table = Table::new(Child::TABLE_NAME);

    let mut select_columns: Vec<SelectExpr> = Parent::COLUMNS
        .iter()
        .map(|c| SelectExpr::from(cte_table.col(*c)).alias(format!("{JOINED_PARENT_PREFIX}{c}")))
        .collect();
    select_columns.extend(
        Child::COLUMNS.iter().map(|c| {
            SelectExpr::from(child_table.col(*c)).alias(format!("{JOINED_CHILD_PREFIX}{c}"))
        }),
    );

    let query = Select::from(&cte_table)
        .left_join(
            &child_table,
            cte_table
                .col(parent_pk_column)
                .eq_col(&child_table.col(fk_column)),
        )
        .columns(select_columns)
        .with([cte]);

    decode_joined_rows(engine, query, parent_pk_column, fk_column).await
}

/// Decodes an already fully-built `query` — selecting both sides through
/// the `JOINED_PARENT_PREFIX`/`JOINED_CHILD_PREFIX` aliasing scheme — into
/// the deduplicated parent list plus a flat `(PK, Child)` list of every
/// actual match (a parent with no matching child contributes nothing to
/// it), left for each caller to group/conflict-check however its own
/// return shape needs. Shared by every has_many/has_one/many_to_many
/// `*_joined`/`*_joined_from_query` function. `child_null_check_column` is
/// whichever column on the child/target side is guaranteed non-null for a
/// real match and null for an unmatched `LEFT JOIN` row (`fk_column` for
/// has_many/has_one, `target_key_column` for many_to_many) — the join
/// condition guarantees it's never null for a real match, so it's an
/// unambiguous "no match here" signal regardless of whether any of
/// `Child`'s own fields happen to be nullable.
async fn decode_joined_rows<Parent, Child, PK>(
    engine: &Engine,
    query: Select,
    parent_pk_column: &str,
    child_null_check_column: &str,
) -> Result<(Vec<Parent>, Vec<(PK, Child)>)>
where
    Parent: Mapped + FromRow,
    Child: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let mut parents = Vec::new();
    let mut seen_parent_keys: HashSet<PK> = HashSet::new();
    let mut matches = Vec::new();

    for row in engine.fetch_all(&query).await? {
        let parent_row = prefixed_sub_row(&row, JOINED_PARENT_PREFIX);
        let parent_key: PK = parent_row.get_by_name(parent_pk_column)?;
        if seen_parent_keys.insert(parent_key.clone()) {
            parents.push(Parent::from_row(&parent_row)?);
        }

        let child_row = prefixed_sub_row(&row, JOINED_CHILD_PREFIX);
        let null_check: Value = child_row.get_by_name(child_null_check_column)?;
        if !matches!(null_check, Value::Null) {
            let child = Child::from_row(&child_row)?;
            matches.push((parent_key, child));
        }
    }

    Ok((parents, matches))
}

/// Many-to-many eager load via the "joined" strategy: fetches every
/// `Parent` row matching `filter` (`None` for no filter) together with
/// every `Target` row it's joined to through `through_table`, in a single
/// two-hop `LEFT JOIN` query (`Parent LEFT JOIN through_table LEFT JOIN
/// Target`) — one round trip total, not `load_many_to_many`'s two.
///
/// Returns the parents in the order they first appear in the joined
/// result set, deduplicated (a join naturally repeats a parent row once
/// per matching target — same direction as `load_many_joined`, since a
/// `Parent` can match many `Target`s here too), alongside the same
/// `HashMap<PK, Vec<Target>>` shape `load_many_to_many` returns — a
/// parent with no matching targets has no entry in it, same as there.
///
/// See `load_many_joined` for what `filter`/the column-aliasing/no-match
/// detection mean (checked here via `target_key_column`, the same role
/// `fk_column` plays there), and `load_many_to_many` for what
/// `through_table`/`local_key_column`/`foreign_key_column`/
/// `target_key_column` mean. `through_table` itself is never selected
/// from beyond its two join columns, so it can't collide with either
/// mapped type's own columns.
pub async fn load_many_to_many_joined<Parent, Target, PK>(
    engine: &Engine,
    filter: Option<Expr>,
    parent_pk_column: &str,
    through_table: &str,
    local_key_column: &str,
    foreign_key_column: &str,
    target_key_column: &str,
) -> Result<(Vec<Parent>, HashMap<PK, Vec<Target>>)>
where
    Parent: Mapped + FromRow,
    Target: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let parent_table = Table::new(Parent::TABLE_NAME);
    let through = Table::new(through_table);
    let target_table = Table::new(Target::TABLE_NAME);

    let mut select_columns: Vec<SelectExpr> = Parent::COLUMNS
        .iter()
        .map(|c| SelectExpr::from(parent_table.col(*c)).alias(format!("{JOINED_PARENT_PREFIX}{c}")))
        .collect();
    select_columns.extend(Target::COLUMNS.iter().map(|c| {
        SelectExpr::from(target_table.col(*c)).alias(format!("{JOINED_CHILD_PREFIX}{c}"))
    }));

    let mut query = Select::from(&parent_table)
        .left_join(
            &through,
            parent_table
                .col(parent_pk_column)
                .eq_col(&through.col(local_key_column)),
        )
        .left_join(
            &target_table,
            target_table
                .col(target_key_column)
                .eq_col(&through.col(foreign_key_column)),
        )
        .columns(select_columns);
    if let Some(filter) = filter {
        query = query.filter(filter);
    }

    let (parents, matches) =
        decode_joined_rows(engine, query, parent_pk_column, target_key_column).await?;
    Ok((parents, group_matches(matches)))
}

/// Like `load_many_to_many_joined`, but instead of a plain `filter`, takes
/// `parents` — an arbitrary `Select` on `Parent`'s own table, the same
/// contract `load_many_joined_from_query`'s `parents` has. `through_table`
/// itself is still never selected from beyond its two join columns.
pub async fn load_many_to_many_joined_from_query<Parent, Target, PK>(
    engine: &Engine,
    parents: Select,
    parent_pk_column: &str,
    through_table: &str,
    local_key_column: &str,
    foreign_key_column: &str,
    target_key_column: &str,
) -> Result<(Vec<Parent>, HashMap<PK, Vec<Target>>)>
where
    Parent: Mapped + FromRow,
    Target: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash + Clone,
{
    let cte_table = Table::new(JOINED_FROM_QUERY_CTE_NAME);
    let cte = Cte::new(JOINED_FROM_QUERY_CTE_NAME, parents);
    let through = Table::new(through_table);
    let target_table = Table::new(Target::TABLE_NAME);

    let mut select_columns: Vec<SelectExpr> = Parent::COLUMNS
        .iter()
        .map(|c| SelectExpr::from(cte_table.col(*c)).alias(format!("{JOINED_PARENT_PREFIX}{c}")))
        .collect();
    select_columns.extend(Target::COLUMNS.iter().map(|c| {
        SelectExpr::from(target_table.col(*c)).alias(format!("{JOINED_CHILD_PREFIX}{c}"))
    }));

    let query = Select::from(&cte_table)
        .left_join(
            &through,
            cte_table
                .col(parent_pk_column)
                .eq_col(&through.col(local_key_column)),
        )
        .left_join(
            &target_table,
            target_table
                .col(target_key_column)
                .eq_col(&through.col(foreign_key_column)),
        )
        .columns(select_columns)
        .with([cte]);

    let (parents, matches) =
        decode_joined_rows(engine, query, parent_pk_column, target_key_column).await?;
    Ok((parents, group_matches(matches)))
}

/// Belongs-to (many-to-one) eager load via the "joined" strategy: fetches
/// every `Child` row matching `filter` (`None` for no filter) together
/// with the single `Parent` row it references, in one `LEFT JOIN` query —
/// one round trip total, not `load_one`'s two.
///
/// The deduplication direction is the opposite of `load_many_joined`/
/// `load_has_one_joined`: there, one parent row could repeat across
/// several matching children, so the *parent* list needed deduplicating.
/// Here it's the reverse — each `Child` row matches at most one `Parent`,
/// so `Child` never repeats and needs no dedup, but more than one child
/// can reference the *same* parent, so the returned
/// `HashMap<PK, Parent>` (the same shape `load_one` returns, keyed by
/// `parent_key_column`) is what dedupes instead.
///
/// `filter` is on `Child`'s own table this time (build it with
/// `Child::table().col(...)`, not a fresh `Table::new(...)`, so it
/// resolves against the same table name this function builds
/// internally) — see `load_many_joined` for why only a plain `filter` is
/// accepted, and for how `Parent`/`Child` column-name collisions are
/// resolved.
pub async fn load_one_joined<Child, Parent, PK>(
    engine: &Engine,
    filter: Option<Expr>,
    fk_column: &str,
    parent_key_column: &str,
) -> Result<(Vec<Child>, HashMap<PK, Parent>)>
where
    Child: Mapped + FromRow,
    Parent: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let child_table = Table::new(Child::TABLE_NAME);
    let parent_table = Table::new(Parent::TABLE_NAME);

    let mut select_columns: Vec<SelectExpr> = Child::COLUMNS
        .iter()
        .map(|c| SelectExpr::from(child_table.col(*c)).alias(format!("{JOINED_CHILD_PREFIX}{c}")))
        .collect();
    select_columns.extend(Parent::COLUMNS.iter().map(|c| {
        SelectExpr::from(parent_table.col(*c)).alias(format!("{JOINED_PARENT_PREFIX}{c}"))
    }));

    let mut query = Select::from(&child_table)
        .left_join(
            &parent_table,
            child_table
                .col(fk_column)
                .eq_col(&parent_table.col(parent_key_column)),
        )
        .columns(select_columns);
    if let Some(filter) = filter {
        query = query.filter(filter);
    }

    decode_one_joined_rows(engine, query, parent_key_column).await
}

/// Like `load_one_joined`, but instead of a plain `filter` on `Child`'s
/// own table, takes `children` — an arbitrary `Select` the caller built
/// however they like, as long as it selects every one of
/// `Child::COLUMNS`, each under its own (unaliased) column name — the
/// same contract `load_many_joined_from_query`'s `parents` has, just on
/// the child/"many" side instead of the parent side (matching this
/// relationship's own direction: `Child` is the `FROM` table here, same
/// as in `load_one_joined`). Wrapped as a `WITH` CTE and joined against
/// directly, the same trick every other `*_joined_from_query` function
/// uses.
pub async fn load_one_joined_from_query<Child, Parent, PK>(
    engine: &Engine,
    children: Select,
    fk_column: &str,
    parent_key_column: &str,
) -> Result<(Vec<Child>, HashMap<PK, Parent>)>
where
    Child: Mapped + FromRow,
    Parent: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let cte_table = Table::new(JOINED_FROM_QUERY_CTE_NAME);
    let cte = Cte::new(JOINED_FROM_QUERY_CTE_NAME, children);
    let parent_table = Table::new(Parent::TABLE_NAME);

    let mut select_columns: Vec<SelectExpr> = Child::COLUMNS
        .iter()
        .map(|c| SelectExpr::from(cte_table.col(*c)).alias(format!("{JOINED_CHILD_PREFIX}{c}")))
        .collect();
    select_columns.extend(Parent::COLUMNS.iter().map(|c| {
        SelectExpr::from(parent_table.col(*c)).alias(format!("{JOINED_PARENT_PREFIX}{c}"))
    }));

    let query = Select::from(&cte_table)
        .left_join(
            &parent_table,
            cte_table
                .col(fk_column)
                .eq_col(&parent_table.col(parent_key_column)),
        )
        .columns(select_columns)
        .with([cte]);

    decode_one_joined_rows(engine, query, parent_key_column).await
}

/// Shared decode step for `load_one_joined`/`load_one_joined_from_query`:
/// every row's `Child` side is decoded directly (it never repeats, so
/// there's nothing to deduplicate there), while the `Parent` side
/// deduplicates into the returned `HashMap` — the same "every column
/// NULL, including the join key" no-match signal `decode_joined_rows`
/// uses, just checked on the parent side here since the dedup direction
/// is reversed for this relationship shape.
async fn decode_one_joined_rows<Child, Parent, PK>(
    engine: &Engine,
    query: Select,
    parent_key_column: &str,
) -> Result<(Vec<Child>, HashMap<PK, Parent>)>
where
    Child: Mapped + FromRow,
    Parent: Mapped + FromRow,
    PK: Into<Value> + FromValue + Eq + Hash,
{
    let mut children = Vec::new();
    let mut by_key: HashMap<PK, Parent> = HashMap::new();

    for row in engine.fetch_all(&query).await? {
        let child_row = prefixed_sub_row(&row, JOINED_CHILD_PREFIX);
        children.push(Child::from_row(&child_row)?);

        let parent_row = prefixed_sub_row(&row, JOINED_PARENT_PREFIX);
        let key_value: Value = parent_row.get_by_name(parent_key_column)?;
        if !matches!(key_value, Value::Null) {
            let key: PK = parent_row.get_by_name(parent_key_column)?;
            let parent = Parent::from_row(&parent_row)?;
            by_key.insert(key, parent);
        }
    }

    Ok((children, by_key))
}
