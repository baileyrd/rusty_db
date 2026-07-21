# rusty_db

A Rust take on [SQLAlchemy Core](https://docs.sqlalchemy.org/en/20/core/): a single, database-agnostic query builder and connection API that lets you swap the underlying database without touching application code.

## Architecture

```
crates/rusty-db-core      database-agnostic layer: query builder, Row/Value, Driver/Connection traits, Engine
crates/rusty-db-derive    #[derive(Mapped)] proc macro: maps a struct onto a table
crates/rusty-db-sqlite    SQLite Driver impl (wraps sqlx::SqlitePool)
crates/rusty-db-postgres  PostgreSQL Driver impl (wraps sqlx::PgPool)
crates/rusty-db-mysql     MySQL/MariaDB Driver impl (wraps sqlx::MySqlPool)
rusty_db/                 facade crate: re-exports core + feature-gated drivers ("sqlite", "postgres", "mysql", "derive")
```

Application code depends only on `rusty-db-core` (via the `rusty_db` facade): `Engine`, `Table`/`Column`, `Select`/`Insert`/`Update`/`Delete`, `Expr`. Which database actually runs underneath is decided once, at startup, by which `Driver` you construct the `Engine` with — everything built on top is portable across backends.

```rust
use rusty_db::prelude::*;

let engine = SqliteDriver::engine("sqlite::memory:").await?;
// let engine = PostgresDriver::engine("postgres://...").await?; // <- only line that changes

let users = Table::new("users");
let query = Select::from(&users)
    .filter(users.col("active").eq(true))
    .order_by(users.col("id").asc())
    .limit(10);

let rows = engine.fetch_all(&query).await?;
for row in rows {
    let name: String = row.get_by_name("name")?;
    println!("{name}");
}
```

### How a new backend gets added

A driver crate implements two traits from `rusty-db-core`:

- `Driver`: hands out connections (`connect(&self) -> Result<Box<dyn Connection>>`) and exposes a `Dialect` (identifier quoting, placeholder style, e.g. `?` vs `$1`).
- `Connection` (via `Executor`): runs SQL text + positional `Value` params, returns `Row`s.

The query builder never talks to a database directly — it renders `(String, Vec<Value>)` via `ToSql::to_sql(&dialect)`, and `Engine` hands that off to whichever `Connection` the configured `Driver` produced. `rusty-db-sqlite`, `rusty-db-postgres`, and `rusty-db-mysql` all implement this by wrapping `sqlx`, decoding sqlx rows into `Value` based on each column's runtime type — `?`-placeholder, double-quote-identifier dialects for SQLite, `$1`-placeholder dialects (with `RETURNING` support) for Postgres, and `?`-placeholder, backtick-identifier dialects for MySQL/MariaDB (`rusty_db::mysql::MySqlDriver`, `mysql` feature).

Both Postgres and MySQL send several column types over their own binary wire formats rather than as text — `NUMERIC`, `DATE`/`TIME`/`TIMESTAMP(TZ)`, `UUID`, and `JSON`/`JSONB` for Postgres; `DATE`/`TIME`/`DATETIME`/`TIMESTAMP` for MySQL — so those get decoded through the matching typed `sqlx` decoder (`BigDecimal`, `chrono`, `Uuid`, `serde_json::Value`) and formatted to `Value::Text`, rather than assumed to already be UTF-8 text like the generic fallback does for everything else (`TEXT`/`VARCHAR`/`CHAR`/etc.).

### Transactions

```rust
let mut txn = engine.begin().await?;
txn.execute("UPDATE accounts SET balance = balance - ? WHERE id = ?", &[100.into(), from_id.into()]).await?;
txn.execute("UPDATE accounts SET balance = balance + ? WHERE id = ?", &[100.into(), to_id.into()]).await?;
txn.commit().await?; // or txn.rollback().await?
```

### Connection pooling

Every driver's `connect`/`engine` use that driver's default pool settings. To tune them explicitly — a smaller `max_connections` for a resource-constrained deployment, or an `acquire_timeout` so a starved pool errors out instead of hanging a request forever — use `connect_with`/`engine_with` with a `PoolConfig`:

```rust
use std::time::Duration;

let config = PoolConfig::new(10).with_acquire_timeout(Duration::from_secs(5));
let engine = SqliteDriver::engine_with("sqlite://app.db?mode=rwc", config).await?;
// same shape on PostgresDriver/MySqlDriver
```

Once `max_connections` connections are checked out, a further `engine.connect()` (or anything that needs a connection, like `Session::flush`) just waits for one to free up — or, with `acquire_timeout` set, gives up and returns an error after that long instead of waiting indefinitely.

### Read replicas and failover

`ReplicaSet` routes reads round-robin across a set of replica `Engine`s and sends writes to a single primary — the common "one writer, many readers" topology most databases scale reads with. It doesn't (and can't) replicate any data itself — that's the database server's own feature (Postgres streaming replication, MySQL/MariaDB replication, etc); `ReplicaSet` just pairs an already-replicated set of `Engine`s, one per server, into a router:

```rust
let primary = SqliteDriver::engine("...").await?; // or PostgresDriver/MySqlDriver
let replica_a = SqliteDriver::engine("...").await?;
let replica_b = SqliteDriver::engine("...").await?;

let replicas = ReplicaSet::with_replicas(primary, vec![replica_a, replica_b]);

let rows = replicas.fetch_all(&Select::from(&users)).await?; // routed to a replica
replicas.execute(&some_insert).await?;                       // always the primary
```

### Query timeouts and cancellation

`with_timeout` wraps any `Engine`/`Transaction`/`Session`/`ReplicaSet` call (anything returning a `Result<T>`) with a client-side timeout: if it hasn't finished within the given duration, it's cancelled and `Error::Timeout` comes back instead:

```rust
use std::time::Duration;

let rows = with_timeout(Duration::from_secs(5), engine.fetch_all(&Select::from(&users))).await?;
```

"Cancelled" here means exactly what it means for any Rust future: the operation is dropped without being polled again — the same thing that happens if you `tokio::spawn` a query and call `JoinHandle::abort()` on it instead. Whatever connection it was using is returned to (or, if it was mid-query, discarded from) the pool by the driver's own `Drop` handling, so a cancelled operation never leaves the pool stuck — the next call just gets a fresh connection if the old one couldn't be reused. Note that the database server itself may not learn the query was abandoned until the connection actually closes; this only stops the client from waiting on it.

### Schema introspection (reflection)

`Engine::list_tables`/`table_schema` ask a live database what tables and columns it actually has, straight from its own catalog, rather than relying only on what the application's `#[derive(Mapped)]` structs declare:

```rust
let tables = engine.list_tables().await?; // Vec<String>, in name order

if let Some(schema) = engine.table_schema("users").await? {
    for column in &schema.columns {
        println!("{}: {} (nullable={}, pk={})", column.name, column.type_name, column.nullable, column.primary_key);
    }
}
```

Column type names are reported verbatim from each database's own catalog (SQLite says `"INTEGER"`, Postgres says `"bigint"`, MySQL says `"bigint(20)"` — note MySQL includes the display width) — there's no attempt to unify them into one portable type system, the same scope decision this crate already makes for `Value`. `table_schema` returns `Ok(None)` for a table that doesn't exist. A driver that doesn't implement introspection (only SQLite/Postgres/MySQL do) returns `Err(Error::Unsupported)` rather than requiring every `Driver` implementor to support it.

If a replica's connection attempt fails, the read automatically retries the next replica in the rotation instead of failing outright, and falls back to the primary if every replica turns out to be unreachable — a `ReplicaSet` built with no replicas at all is simply a primary-only fallback, not a case callers need to special-case. `ReplicaSet::session()` deliberately hands back a `Session` backed by the primary, never a replica: a session's autoflush/identity-map guarantees depend on reading back its own not-yet-committed writes, which a lagging replica can't promise.

### Joins

`Select` supports `join`/`left_join`/`right_join`/`full_join`, and `Column::eq_col` builds a column-to-column condition (as opposed to `Column::eq`, which compares against a literal):

```rust
let orders = Table::new("orders");
let users = Table::new("users");

let query = Select::from(&orders)
    .columns([orders.col("amount"), users.col("name")])
    .join(&users, orders.col("user_id").eq_col(&users.col("id")))
    .filter(users.col("active").eq(true));
```

### Struct-to-table mapping (`#[derive(Mapped)]`)

Enable the `derive` feature to map a struct onto a table:

```rust
use rusty_db::prelude::*;

#[derive(Mapped)]
#[table(name = "users")] // optional; defaults to the snake_case struct name
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
    active: bool,
}

let user = User { id: 1, name: "ada".into(), active: true };
engine.execute(&user.insert()).await?;

let users: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;

let mut user = users.into_iter().next().unwrap();
user.active = false;
engine.execute(&user.update()).await?;   // only generated when a field is #[table(primary_key)]
engine.execute(&user.delete_query()).await?;
```

Field types are limited to whatever `Value` already converts from (`bool`, `i64`, `i32`, `f64`, `String`, `Vec<u8>`, and `Option<_>` of those) — there's no arbitrary custom-type support yet.

### Session / unit of work

`Engine::session()` (or `Session::new(engine)`) gives you a unit of work: queue writes with `add`/`update`/`delete` (available for any `#[derive(Mapped)]` type — `add` needs the `Entity` impl every mapped type gets, `update`/`delete` need `Identifiable`, which requires a `#[table(primary_key)]` field), then send them to the database with `commit()`:

```rust
let mut session = engine.session();

session.add(&User { id: 1, name: "ada".into(), active: true });
session.add(&User { id: 2, name: "grace".into(), active: true });
session.commit().await?; // both inserts run in one transaction

let mut ada = /* ...fetched via engine.fetch_one_as::<User>(...)... */;
ada.active = false;
session.update(&ada);
session.delete(&some_other_user);
session.commit().await?; // update + delete, atomically
```

A session's writes actually run inside one ongoing transaction that begins lazily (on the first flush or read) and stays open until `commit()` (`COMMIT`) or `rollback()` (`ROLLBACK` — undoing anything already flushed into it, not just what's still queued). `session.flush()` sends every currently-queued write into that transaction without committing it — `session.get`/`load_all` (see the identity map below) call this automatically first (autoflush), so they always see your own not-yet-committed writes. Nothing outside the session sees them until `commit()`: raw reads through `Engine` (`engine.fetch_all_as`, etc.) go through a different connection and only ever see already-committed data, exactly like a second client would.

If a flush fails partway through a batch, the whole transaction (including anything flushed earlier in its lifetime) rolls back and the queue is left untouched, so fixing the problem and calling `commit()`/flushing again starts over cleanly.

### Identity map

`Session::get`/`load_all` cache decoded rows by `(type, primary key)`: loading the same row twice through the same session returns the exact same `Rc<RefCell<T>>` handle rather than two independently-decoded copies, and mutations made through one handle are visible through any other handle to that row — including handles you fetch *after* the mutation, since the identity map wins over what's actually in the database:

```rust
let mut session = engine.session();

let ada = session.get::<User>(1_i64).await?.unwrap();
ada.borrow_mut().active = false; // in-memory only, not yet written anywhere

let ada_again = session.get::<User>(1_i64).await?.unwrap();
assert!(Rc::ptr_eq(&ada, &ada_again));       // same object
assert_eq!(ada_again.borrow().active, false); // sees the in-memory change

let users = session.load_all::<User>(&Select::from(&User::table())).await?; // Vec<Rc<RefCell<User>>>
// users[0] is `ada` again if its primary key was already cached, not a fresh decode.

session.update(&*ada.borrow()); // queued; autoflushes on the next get/load_all, or on commit()
session.commit().await?;
```

`get::<T>` requires a `#[table(primary_key)]` field (it looks up by that column); `load_all::<T>` requires `T: Identifiable` for the same reason, since it needs each row's own primary key to key the cache. This is why `Session` isn't `Send` — it hands out `Rc`s, matching how SQLAlchemy's own `Session` is documented as single-thread/task use only.

`session.delete(&entity)` evicts it from the identity map immediately (not deferred to flush), so `get`/`load_all` can never hand back a stale cached instance for a row you've already deleted — even before `commit()` runs:

```rust
let ada = session.get::<User>(1_i64).await?.unwrap();
session.delete(&*ada.borrow());
assert_eq!(session.get::<User>(1_i64).await?, None); // autoflushes the delete, then queries fresh
```

That eviction isn't undone by `rollback()`: if the delete itself gets rolled back, the row still exists in the database but is no longer cached, so the next `get`/`load_all` for it just re-fetches and re-caches it. Call `session.clear_identity_map()` (or start a new `Session`) for any other clean-slate need.

### Relationships and eager loading

`#[has_many(Child, foreign_key = "...")]` and `#[belongs_to(Parent, foreign_key = "...")]` declare a relationship between two `#[derive(Mapped)]` types and generate a batched ("select-in") loader — one extra query for the whole batch, not one per row:

```rust
#[derive(Mapped)]
#[table(name = "users")]
#[has_many(Order, foreign_key = "user_id")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[derive(Mapped)]
#[table(name = "orders")]
#[belongs_to(User, foreign_key = "user_id")]
struct Order {
    #[table(primary_key)]
    id: i64,
    user_id: i64,
    amount: i64,
}

let users: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
let orders_by_user: HashMap<i64, Vec<Order>> = User::load_orders(&engine, &users).await?;

let orders: Vec<Order> = engine.fetch_all_as(&Select::from(&Order::table())).await?;
let users_by_id: HashMap<i64, User> = Order::load_user(&engine, &orders).await?;
```

`has_many`'s loader name is the pluralized (naive `+s`) snake_case child type (`load_orders`); `belongs_to`'s is the singular snake_case parent type (`load_user`). Both are generated from, and callable directly as, `rusty_db::relations::load_many`/`load_one` if you'd rather not use the attributes (e.g. for a relationship keyed by something other than the primary key). There's no lazy-loading proxy or identity map here — relationships are always loaded explicitly, in a batch, by calling one of these functions.

### Migrations

`Engine::migrator()` (or `Migrator::new(&engine)`) runs versioned schema migrations, tracking which have applied in a bookkeeping table (`_rusty_db_migrations` by default). Migrations are plain SQL — the query builder covers DML, not DDL, and DDL syntax diverges more across databases than DML does, so there's no attempt to make a migration portable for you:

```rust
const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "create_users",
        up: &["CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"],
        down: &["DROP TABLE users"],
    },
    Migration {
        version: 2,
        name: "add_users_email",
        up: &["ALTER TABLE users ADD COLUMN email TEXT"],
        down: &[], // empty = irreversible
    },
];

let migrator = engine.migrator();
let applied = migrator.up(MIGRATIONS).await?; // runs every not-yet-applied migration, in order

migrator.down(MIGRATIONS).await?; // reverts the most recently applied one (errors if its `down` is empty)
migrator.status(MIGRATIONS).await?; // Vec<(Migration, bool)> — for diagnostics/tests
```

Each migration runs in its own transaction (its `up`/`down` statements plus the bookkeeping row update, atomically). If a migration fails partway through a batch, `up()`/`down()` return the error and everything before the failure in that call has already committed — fix the problem and call again to resume. Each entry in `up`/`down` is a separate SQL statement (executed one call at a time), since not every driver's query protocol supports multiple statements per call — split multi-statement changes into multiple slice entries rather than joining them with `;`.

To instead fold migrations into a `Session`'s own unit of work — sharing atomicity with regular reads/writes, so a schema change and the data migration that depends on it commit (or roll back) together — use `session.migrate(MIGRATIONS)` instead of a standalone `Migrator`:

```rust
let mut session = engine.session();

session.migrate(MIGRATIONS).await?; // runs inside the session's own transaction, not a separate one
session.add(&User { id: 1, name: "ada".into(), active: true });
session.commit().await?; // the schema change and the row both take effect together
```

Unlike `Migrator::up`, which commits each migration independently, `session.migrate` applies every pending migration inside the session's single ongoing transaction (autoflushing queued writes first) — so nothing it does is visible to another connection, and none of it takes effect, until that session's `commit()` runs. A failure rolls back the whole transaction, same as `flush()`.

## Status

This covers Core (query builder, connections), a thin mapping layer (`#[derive(Mapped)]`, joins, has-many/belongs-to eager loading), versioned migrations (standalone via `Migrator`, or folded into a session's transaction via `session.migrate`), and a unit-of-work `Session` with autoflush and an identity map (including eviction on delete). Three drivers exist — SQLite, PostgreSQL, and MySQL/MariaDB — all built the same way (wrapping `sqlx`) and all exercised by the test suite. The Postgres and MySQL tests run against real servers when reachable (`POSTGRES_TEST_URL`/`MYSQL_TEST_URL`, defaulting to local `rusty`/`rusty` test databases) and just skip themselves rather than fail if one isn't — so `cargo test` stays green without either installed, but this environment does have both, and both are actually exercised here.

`tests/concurrent_sessions.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover multiple `Session`s sharing one `Engine`/connection pool at the same time — `Session` is intentionally `!Send` (it hands out `Rc`s for the identity map), so these run via `tokio::task::LocalSet`/`spawn_local` rather than `tokio::spawn`, which is the standard way to get genuinely concurrent, interleaved execution of `!Send` futures on one thread. They cover independent commits landing correctly under a burst of concurrent sessions, one session's flushed-but-uncommitted write staying invisible to a concurrent reader on a separate connection (deterministically ordered via a `oneshot` channel, not timing), and two sessions never sharing identity-map state for the same row. Same skip-if-unreachable behavior as the Postgres/MySQL smoke tests; each test uses its own table to avoid colliding with other tests running concurrently against the same live server.

`tests/pool_exhaustion.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `PoolConfig` itself: a `connect()` beyond a pool's `max_connections` blocks rather than erroring or handing out a duplicate connection, and succeeds once an outstanding connection is dropped back to the pool (proven with `tokio::time::timeout` rather than assumptions about scheduling order); a higher `max_connections` is honored before the pool starts blocking; a configured `acquire_timeout` errors out promptly instead of blocking forever; and a burst of `Session`s serializes correctly (no lost or corrupted writes) when they're all forced to take turns on a single-connection pool.

`tests/replica_set.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `ReplicaSet`: reads round-robin across healthy replicas (verified by seeding each stand-in replica with its own marker row and checking which one a read actually returned, rather than peeking at any internal routing state); a down replica fails over to the next healthy one instead of surfacing an error; every replica being down falls back to the primary; a `ReplicaSet` with zero replicas configured always uses the primary; and writes (`execute`, `Session`) always land on the primary regardless of replica health. Since real database replication is a server-side feature this crate can't spin up in a sandbox, a "down" replica/primary is simulated with a minimal fake `Driver` whose `connect()` always returns `Error::Connection` — the same failure shape a genuinely unreachable server produces — rather than by actually taking a live server down mid-suite; the Postgres/MySQL versions otherwise use real, separate tables on the live servers as replica stand-ins.

`tests/query_timeout.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `with_timeout`: an operation finishing inside its timeout succeeds normally; a genuinely slow/blocked operation is cancelled and returns `Error::Timeout` instead of hanging; a cancelled operation leaves the pool usable afterward rather than stuck; a timeout on one call has no lingering effect on later calls; and aborting a task running a slow operation (`JoinHandle::abort`, the other standard way to cancel a Rust future) cancels it the same way a timeout would. The SQLite version gets a genuinely blocked (not simulated) query via real lock contention — holding SQLite's write lock on one connection while a second connection attempts a conflicting write, which sqlx's SQLite driver retries against its own 5-second `busy_timeout` rather than erroring immediately. The Postgres/MySQL versions use their real, built-in `pg_sleep()`/`SLEEP()` functions for a genuinely slow query instead.

`tests/schema_introspection.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `list_tables`/`table_schema`: created tables are reported (and, for SQLite, internal `sqlite_*` tables never are); a table's columns come back with the right type names, nullability, and primary-key flags; and a table that doesn't exist reports `Ok(None)` rather than an error. Chasing down why the SQLite version's PRAGMA-based columns were coming back entirely `Null` uncovered a real bug in the SQLite driver's row decoder: it was keying off each column's *declared* type (`"NULL"` when SQLite can't infer one statically — true for `PRAGMA` output and other non-plain-table results, not just when the value itself is null) instead of that value's actual *runtime* type, so it silently decoded every column as `Value::Null` whenever SQLite left the static type undeclared. Fixed by falling back to the per-value runtime type (via `try_get_raw`) exactly when the declared type is `"NULL"`, leaving the existing behavior for ordinary table queries (which do have a meaningful declared type) unchanged — confirmed by the rest of the suite still passing unmodified.

## Running tests

```
cargo test --workspace --all-features
```
