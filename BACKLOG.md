# Backlog: capability gaps vs. SQLAlchemy

`rusty_db` is a Rust take on SQLAlchemy — a portable query builder plus a
thin ORM-lite layer over SQLite/Postgres/MySQL. This document tracks what
SQLAlchemy (Core + ORM) can do that `rusty_db` can't yet, as raw material
for deciding what to build next. It's a snapshot, not a roadmap — nothing
here is committed to, ordered by, or scheduled; items get picked off (or
re-scoped, or dropped) one at a time as the project's "what's next" moves.

Each item has a rough effort guess (S/M/L/XL) — a gut-feel for how much
surface area it touches (query builder vs. derive macro vs. `Session` vs.
per-driver code), not a real estimate.

Every item below is also tracked as a GitHub issue, grouped under one
tracking epic per section: [Schema/DDL/reflection (#54)](https://github.com/baileyrd/rusty_db/issues/54),
[Mapping/derive macro (#60)](https://github.com/baileyrd/rusty_db/issues/60),
[Relationships/eager loading (#66)](https://github.com/baileyrd/rusty_db/issues/66),
[Topology/deployment (#80)](https://github.com/baileyrd/rusty_db/issues/80),
[Tooling (#83)](https://github.com/baileyrd/rusty_db/issues/83).

Four tracking epics are fully done and no longer listed above: [Value/type
system (#46)](https://github.com/baileyrd/rusty_db/issues/46),
[Session/unit-of-work (#73)](https://github.com/baileyrd/rusty_db/issues/73),
[Query builder (#38)](https://github.com/baileyrd/rusty_db/issues/38), and
[Async & performance (#77)](https://github.com/baileyrd/rusty_db/issues/77)
— see "current state" below for what they added.

## How to read "current state"

As of the most recently merged work: a query builder (`Select`/`Insert`/
`Update`/`Delete`, `INNER`/`LEFT`/`RIGHT`/`FULL JOIN`, `=`/`<>`/`<`/`<=`/
`>`/`>=`/`LIKE`/`ILIKE`/`BETWEEN`/`IN`/`IS [NOT] NULL`/`AND`/`OR`/`NOT`,
`DISTINCT`, `ORDER BY`, `LIMIT`/`OFFSET`, `RETURNING` on `INSERT`/`UPDATE`/
`DELETE`, table aliasing/self-joins, a `text()` escape hatch for dropping
raw SQL into an otherwise builder-constructed query, `COUNT`/`SUM`/`AVG`/
`MIN`/`MAX`/arbitrary expression `SELECT` columns via `SelectExpr`,
`GROUP BY`/`HAVING`, `UNION`/`UNION ALL`/`INTERSECT`/`EXCEPT` via
`SetOperation`, `LOWER`/`UPPER`/concatenation/arithmetic/`CASE`/
`COALESCE`/`CURRENT_TIMESTAMP`, subqueries — `IN (subquery)`,
correlated `EXISTS`, and scalar subqueries, though not yet a subquery in a
`FROM` clause — CTEs via `Cte`, including `WITH RECURSIVE`, and window
functions (`ROW_NUMBER`/`RANK`/`DENSE_RANK`, and aggregates as window
functions, via `Window`/`.over(...)`); a portable DDL builder
(`CreateTable`/`DropTable`/`CreateIndex`/`DropIndex`/`AlterTable`, with a
portable `ColumnType` translated to each dialect's own `CREATE TABLE`
spelling) — `AlterTable` only adds/drops a column (no renaming or
altering an existing column's type/constraints), and on SQLite specifically
carries a real caveat: a connection that already had a table's pre-`ALTER`
shape in view can panic if reused to query that table right afterward, a
long-standing upstream `sqlx`/SQLite limitation (a fresh `Engine` avoids
it); first-class `Value` variants for `Uuid`, `BigDecimal`, `serde_json::Value` (as `Json`),
`chrono`'s `NaiveDate`/`NaiveTime`/`NaiveDateTime`/`DateTime<Utc>`, and
`Vec<T>` arrays (native on Postgres, JSON-flattened on MySQL/MariaDB and
SQLite); `#[derive(Mapped)]` with one primary key, one version column, one
soft-delete column, custom column/table names, plus `#[derive(MappedEnum)]`
and `#[derive(MappedNewtype)]` for mapping a Rust enum or newtype onto a
column; a `Session` unit-of-work with an identity map, autoflush, bulk
insert, `bulk_update`/`bulk_delete`, audit logging, optimistic locking,
soft deletes, mapping-level column defaults (`#[table(default = "...")]`,
distinct from the database-side column defaults schema introspection
reflects below), computed/hybrid properties (`#[hybrid(name = "...", expr
= "...")]`, an arithmetic-over-fields subset — see "Richer hybrid-property
expressions" below), session-level lifecycle hooks (`on_before_flush`/etc.)
plus a hand-implemented `Lifecycle` trait for entity-level
`before_insert`/`after_update`/`validate`-style hooks (`Session::add_mut`/
`update_mut`/`delete_mut`), `expire_on_commit` semantics, savepoints/
nested transactions, two-phase commit, and a fluent `session.query::<T>()`
API; `has_many`/`belongs_to`/`has_one`/`many_to_many` select-in eager
loading with cascade delete/orphan rules, plus a `subqueryload`-style
alternative for all four (`rusty_db::relations::load_many_via_subquery`/
`load_has_one_via_subquery`/`load_one_via_subquery`/
`load_many_to_many_via_subquery`, joining directly against a
caller-supplied `Select` wrapped as a CTE instead of shipping a parent
key list back and forth — not yet wired into the derive attributes as an
opt-in strategy, see "Wire subqueryload into the derive attributes"
below); hand-written versioned
migrations; schema introspection (columns/types/nullability/PK/foreign
keys/indexes/unique constraints/check constraints/column defaults);
logical backup/restore; read replicas; TLS; query timeouts; connection-pool
observability; connection-level event hooks (`PoolConfig::with_on_connect`/
`.with_before_acquire`/`.with_after_release`); a tunable per-connection
statement-cache capacity (`PoolConfig::with_statement_cache_capacity`);
streaming query results (`Engine::fetch_stream`/`fetch_stream_as`);
automap-style `#[derive(Mapped)]` struct generation from live schema
reflection (`Engine::automap_table`/`automap_all`); Alembic-style
autogenerate diffing a set of `#[derive(Mapped)]` types' expected shape
against a live database, generating `CreateTable`/`AlterTable` DDL for
review (`Engine::autogenerate_migration`, `TableSpec`), with
`AutogenerateOptions` opting into a caller-hinted column rename
(`AlterTable::rename_column`, spelled identically on all three dialects)
or an explicitly allow-listed whole-table drop, beyond the conservative
add/drop-column default — still never a type change either way (see
"Autogenerate type-change detection" below); and `ShardRouter`, routing to one
of several `Engine`s by hashing a caller-supplied key, via either naive
modulo hashing (`ShardRouter::new`) or a consistent-hash ring
(`ShardRouter::new_consistent`, remapping only a minority of keys when
the shard count grows — see "Live resharding" below for what's still
missing). See `README.md` for the full tour with examples.

---

## Schema / DDL / reflection

- **Autogenerate type-change detection** — `Engine::autogenerate_migration`
  now closes the rename and whole-table-drop gaps via explicit, opt-in
  `AutogenerateOptions` (a caller-supplied rename hint, an allow-listed
  drop), but still never detects a column's *type* changing: a live
  column's `type_name` is dialect-native, verbatim text with no portable
  representation to compare `ColumnType` against without reimplementing
  `automap::rust_type_for`'s heuristic in reverse, per dialect — changing
  a field's type without renaming it produces no suggested statement at
  all. Closing this needs either that reverse heuristic (per dialect) or
  its own opt-in surface (e.g. a caller-supplied "this column's new type
  is X" hint, mirroring how renames are already handled) — plus, once a
  type change is detected at all, a real `AlterTable` operation to render
  it as (`ALTER COLUMN ... TYPE ...`/`MODIFY COLUMN`/SQLite's
  table-rebuild dance), which doesn't exist yet either. **M**

## Mapping / derive macro (`#[derive(Mapped)]`)

- **Composite primary keys** — the macro currently enforces at most one
  `#[table(primary_key)]` field; every PK-dependent feature (`get`,
  `update`, `delete`, optimistic locking, soft deletes, relationships) is
  built assuming a single scalar key. This is a foundational, cross-cutting
  change, not a bolt-on. **XL**
- **Inheritance/polymorphism** (single-table, joined-table, or concrete) —
  entirely absent; every `Mapped` type maps to exactly one table with no
  discriminator concept. **XL**
- **Richer hybrid-property expressions** — `#[hybrid(...)]`'s v1 only
  parses `+`/`-`/`*`/`/` over this struct's own fields, literals, and
  parentheses; it has no string functions, `CASE`/`COALESCE`, comparisons,
  or references to a joined table's columns, and the Rust-side/SQL-side
  halves are only guaranteed to agree for that arithmetic subset (anything
  richer needs a hand-written Rust method sitting beside a hand-written
  `_expr()`, with nothing checking the two still agree). **M**

## Relationships / eager loading

- **Lazy loading** (an attribute that fetches on first access instead of
  always being eagerly select-in-loaded) — today every relationship is
  eager, which is safe but can over-fetch. **L**
- **Wire subqueryload into the derive attributes** — `load_many_via_subquery`/
  `load_has_one_via_subquery`/`load_one_via_subquery`/
  `load_many_to_many_via_subquery` exist and work (a `subqueryload`-style
  alternative to the default select-in strategy, joining directly against
  a caller-supplied `Select` instead of a literal key list), but only as
  plain functions called directly — `#[has_many(...)]`/etc. don't yet
  accept a `strategy = "subquery"` (or similar) to generate a convenience
  method around them the way the default strategy already gets one. **S**
- **`joined` eager-loading strategy** — SQLAlchemy's other alternative to
  select-in: fetch parent and child in one query via `LEFT JOIN` instead
  of a second round trip. Doesn't exist in any form yet; unlike
  `subqueryload` above, this one changes the *shape* of the parent fetch
  itself (one row per matched child, needing de-duplication into the
  eventual `Vec<Parent>`), not just how the second query is built, so it
  isn't just "another `relations.rs` function" the way subqueryload was. **M**

## Topology / deployment

- **Live resharding** for `ShardRouter` — `ShardRouter::new_consistent`
  now bounds how many keys a *new* router with a different shard count
  remaps, but there's still no `add_shard`/`remove_shard` on an existing
  router, no online rebalancing, and — the actually hard part — nothing
  moves a remapped key's existing rows from its old shard to its new one;
  that's still entirely a migration step of your own. Also no
  multi-primary-per-shard topology (each shard is a single `Engine`, so a
  shard with its own read replicas needs its own `ReplicaSet` composed in
  separately, which `ShardRouter` doesn't do for you). **L**
- **Additional backends** (Oracle, MSSQL, or a generic ODBC-style driver) —
  only SQLite/Postgres/MySQL exist; each new backend is roughly "another
  driver crate," bounded but not small. **XL** (per backend)

## Tooling

- **A migration CLI** (an Alembic-equivalent command-line tool — `rusty-db
  migrate up`/`down`/`status` as a standalone binary, vs. today's
  library-only `Migrator`/`session.migrate()` API) — there's no binary
  crate anywhere in the workspace yet. Note: migrations are always defined
  as compile-time Rust `const` arrays (`&'static [Migration]`), not loaded
  from files on disk, so a genuinely standalone binary needs a file-based
  migration format invented first — a bigger prerequisite than the effort
  guess suggests. **M**

---

_This is a living snapshot generated from the codebase's state at the time
of writing — re-derive it (or prune completed items) whenever the "what's
next" conversation would benefit from a refresh, rather than trying to
keep it perfectly in sync by hand._
