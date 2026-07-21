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
tracking epic per section: [Query builder (#38)](https://github.com/baileyrd/rusty_db/issues/38),
[Value/type system (#46)](https://github.com/baileyrd/rusty_db/issues/46),
[Schema/DDL/reflection (#54)](https://github.com/baileyrd/rusty_db/issues/54),
[Mapping/derive macro (#60)](https://github.com/baileyrd/rusty_db/issues/60),
[Relationships/eager loading (#66)](https://github.com/baileyrd/rusty_db/issues/66),
[Session/unit-of-work (#73)](https://github.com/baileyrd/rusty_db/issues/73),
[Async & performance (#77)](https://github.com/baileyrd/rusty_db/issues/77),
[Topology/deployment (#80)](https://github.com/baileyrd/rusty_db/issues/80),
[Tooling (#83)](https://github.com/baileyrd/rusty_db/issues/83).

## How to read "current state"

As of the most recently merged work: a query builder (`Select`/`Insert`/
`Update`/`Delete`, `INNER`/`LEFT`/`RIGHT`/`FULL JOIN`, `=`/`<>`/`<`/`<=`/
`>`/`>=`/`LIKE`/`IN`/`IS [NOT] NULL`/`AND`/`OR`/`NOT`, `ORDER BY`, `LIMIT`/
`OFFSET`, `RETURNING` on `INSERT`); `#[derive(Mapped)]` with one primary
key, one version column, one soft-delete column, custom column/table
names; a `Session` unit-of-work with an identity map, autoflush, bulk
insert, audit logging, optimistic locking, soft deletes; `has_many`/
`belongs_to` select-in eager loading; hand-written versioned migrations;
schema introspection (columns/types/nullability/PK only); logical backup/
restore; read replicas; TLS; query timeouts; connection-pool observability.
See `README.md` for the full tour with examples.

---

## Query builder (Core-equivalent)

- **Aggregate functions & expression columns** (`COUNT`/`SUM`/`AVG`/`MIN`/
  `MAX`, arbitrary `SELECT <expr> AS alias`) — `Select` only takes plain
  `Column`s today; there's no expression-column type at all. Blocks
  anything but the simplest reporting queries. **L**
- **`GROUP BY` / `HAVING`** — no aggregation grouping exists. **M**
- **`DISTINCT`** — no way to dedupe a result set at the SQL level. **S**
- **Subqueries** — no way to nest a `Select` inside another query's `FROM`,
  column list, or a filter (`IN (subquery)`, scalar subquery, correlated
  `EXISTS`). Currently the only composition is fetching once and filtering
  again in Rust. **L**
- **CTEs (`WITH`, `WITH RECURSIVE`)** — no support; recursive CTEs in
  particular have no workaround at all today. **L**
- **Set operations** (`UNION`/`UNION ALL`/`INTERSECT`/`EXCEPT`) — absent. **M**
- **Window functions** (`OVER (PARTITION BY ... ORDER BY ...)`, `ROW_NUMBER`,
  `RANK`, running totals) — absent. **L**
- **`CASE` expressions, `COALESCE`, arithmetic/string SQL functions**
  (`func.now()`-equivalent, `LOWER`/`UPPER`/`||`, date arithmetic) — `Expr`
  only has comparisons and boolean combinators, no function-call construct. **L**
- **`BETWEEN` and `ILIKE`/regex-style operators** — smaller gap in the same
  family as the aggregate-functions item; call out separately since it's a
  much cheaper first step toward a richer `Expr`. **S**
- **`RETURNING` on `UPDATE`/`DELETE`** — currently `Insert`-only (and only
  where `Dialect::supports_returning()` is true); the same plumbing would
  extend naturally. **S**
- **Table aliasing / self-joins** — no `Table::alias(...)`-equivalent, so a
  query can't join a table to itself or reference the same table twice.
  Also blocks writing correlated subqueries once those exist. **M**
- **A `text("...")` construct that composes with the builder** — raw SQL
  exists (`Engine::connect()`/`Transaction::execute`), but only as an
  all-or-nothing escape hatch; there's no way to drop a raw fragment into
  an otherwise builder-constructed `Select`/`Expr`. **M**

## Value / type system

- **First-class temporal types** (`Date`/`Time`/`DateTime`/`Timestamp`,
  ideally interoperating with `chrono`/`time`) — Postgres/MySQL currently
  decode these via sqlx's typed decoders but flatten them into
  `Value::Text`; round-tripping a `chrono::NaiveDateTime` through a mapped
  struct isn't possible today. **L**
- **UUID as a first-class `Value`** (currently also flattened to text). **M**
- **JSON/JSONB as a first-class `Value`** (ideally backed by `serde_json`,
  with query-side JSON path/containment operators eventually). **L**
- **Decimal/Numeric as a first-class `Value`** (currently flattened to
  text; `f64` would lose precision, so this needs its own variant, likely
  backed by a decimal crate). **M**
- **Array columns** (Postgres native arrays especially) — no representation
  at all. **L**
- **Per-field custom type conversion** (a `TypeDecorator`-equivalent hook
  so application code can map, say, a newtype or enum onto a `Value`
  without waiting on every type to become a built-in `Value` variant) —
  this alone would take a lot of pressure off the four items above by
  letting callers bridge the gap themselves in the meantime. **M**
- **Enum columns** — map a Rust `enum` onto a text/int column (and, for
  Postgres, potentially its native `ENUM` type) via the derive macro. **M**

## Schema / DDL / reflection

- **Foreign key reflection** — `list_tables`/`table_schema` report columns/
  types/nullability/PK only; no FK relationships are ever queried from the
  catalog. Blocks any future "generate relationships from an existing
  schema" tooling and makes `restore()` unable to reason about FK-safe
  insert ordering. **M**
- **Index reflection & creation** — absent both for introspection and for
  any DDL-builder. **M**
- **Unique constraint reflection & creation** — absent. **S**
- **Check constraint reflection & creation** — absent. **S**
- **Column default value reflection** — `ColumnInfo` has no `default`
  field. **S**
- **A DDL builder** (`CREATE TABLE`/`CREATE INDEX` construction from Rust,
  the way SQLAlchemy Core's `MetaData`/`Table`/`Column` double as DDL —
  today schema changes are entirely hand-written SQL strings inside a
  migration). **XL**
- **Alembic-style autogenerate** — diff a mapped model's shape against the
  live database and generate the migration for you, instead of every
  migration being fully hand-written. Depends on the DDL builder and much
  richer reflection above; this is the single largest gap in the
  migrations story. **XL**

## Mapping / derive macro (`#[derive(Mapped)]`)

- **Composite primary keys** — the macro currently enforces at most one
  `#[table(primary_key)]` field; every PK-dependent feature (`get`,
  `update`, `delete`, optimistic locking, soft deletes, relationships) is
  built assuming a single scalar key. This is a foundational, cross-cutting
  change, not a bolt-on. **XL**
- **Column defaults expressed at the mapping layer** (`#[table(default =
  ...)]`, distinct from a DB-side `DEFAULT`) so `Session::add` can omit a
  field and still insert a sensible value. **M**
- **Lifecycle hooks/validators** (`before_insert`/`after_update`/a
  `#[validates(...)]`-equivalent) — nothing in `Session` or the derive
  macro runs application code around a write today. **L**
- **Inheritance/polymorphism** (single-table, joined-table, or concrete) —
  entirely absent; every `Mapped` type maps to exactly one table with no
  discriminator concept. **XL**
- **Computed / hybrid properties** (a Rust-side derived attribute that can
  also translate into a SQL expression for filtering, à la
  `hybrid_property`) — depends on the expression-column work above to be
  useful for the "filter by it" half. **L**

## Relationships / eager loading

- **Many-to-many relationships** (join-table-aware, e.g. `#[many_to_many]`)
  — only `has_many`/`belongs_to` exist today; a join table has to be
  modeled and queried by hand. **L**
- **One-to-one as a first-class relationship kind** (currently only
  expressible as a `belongs_to` with an incidentally-unique FK — no
  dedicated ergonomics or validation that it's actually 1:1). **S**
- **Lazy loading** (an attribute that fetches on first access instead of
  always being eagerly select-in-loaded) — today every relationship is
  eager, which is safe but can over-fetch. **L**
- **Additional eager-loading strategies** (`joined`/`subquery`, alongside
  the existing select-in) — mostly matters once subqueries/joins-in-`Select`
  exist to make a real choice between strategies meaningful. **M**
- **Cascade rules** (deleting/soft-deleting a parent doesn't touch children
  at all right now — no cascade delete, no orphan cleanup). **L**

## Session / unit-of-work

- **A fluent, session-bound query API** (`session.query::<T>().filter(...)`)
  — today `load_all`/`load_active` take a raw `&dyn ToSql` you build
  yourself via `Select::from(&T::table())`; there's no type-bound builder
  that narrows to `T`'s own columns/filters for you. **M**
- **`bulk_update`/`bulk_delete` through `Session`** (a `WHERE`-scoped
  `UPDATE`/`DELETE` affecting many rows in one statement, the update/delete
  counterpart to the existing `add_all` bulk insert) — every `update`/
  `delete` today is still one `PendingWrite` per entity. **M**
- **Session-level events/hooks** (`before_flush`/`after_commit`-equivalent)
  — nothing observes a session's lifecycle today besides audit logging,
  which is purpose-built rather than a general hook point. **M**
- **`expire_on_commit`-style semantics** — identity-mapped objects never
  expire/refresh on commit; stale in-memory state persists until eviction
  or a fresh `Session`. **M**
- **Savepoints / nested transactions** — `Session`/`Engine::begin()` only
  offer one flat transaction; no way to roll back part of a unit of work
  without aborting all of it. **M**
- **Two-phase commit / distributed transactions** — absent; niche, but a
  real SQLAlchemy Core capability. **L**

## Async & performance

- **Streaming query results** (a cursor/`Stream`-based fetch instead of
  always collecting a full `Vec<Row>`) — every fetch path
  (`fetch_all`/`fetch_all_as`) materializes the entire result set today,
  which is a real ceiling for large exports/reports. **L**
- **Compiled-statement / query-result caching** (a `baked query`-equivalent
  — cache a rendered `(sql, params-shape)` for a repeatedly-run query
  shape) — nothing at the rusty_db layer controls or exposes this; sqlx
  may do some of its own prepared-statement caching underneath, but it
  isn't surfaced or tunable here. **M**
- **Connection-level event hooks** (on-connect/on-checkout/on-checkin
  callbacks — e.g. to set a session variable on every new connection) —
  absent; `pool_stats()` covers observability but not behavioral hooks. **M**

## Topology / deployment

- **Sharding / multi-tenant routing** beyond the existing single-primary +
  round-robin-read-replica `ReplicaSet` — no partitioning/shard-key
  routing, no multi-primary topology. **XL**
- **Additional backends** (Oracle, MSSQL, or a generic ODBC-style driver) —
  only SQLite/Postgres/MySQL exist; each new backend is roughly "another
  driver crate," bounded but not small. **XL** (per backend)

## Tooling

- **A migration CLI** (an Alembic-equivalent command-line tool — `rusty-db
  migrate up`/`down`/`status` as a standalone binary, vs. today's
  library-only `Migrator`/`session.migrate()` API) — there's no binary
  crate anywhere in the workspace yet. **M**
- **Automap-style reverse engineering** (generate `#[derive(Mapped)]`
  structs from an existing database's schema, using the reflection this
  crate already has) — a natural, self-contained follow-on to richer
  reflection (FKs especially) above. **L**

---

_This is a living snapshot generated from the codebase's state at the time
of writing — re-derive it (or prune completed items) whenever the "what's
next" conversation would benefit from a refresh, rather than trying to
keep it perfectly in sync by hand._
