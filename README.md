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

`engine.pool_stats()` gives a live snapshot instead of inferring saturation from `acquire_timeout` errors after the fact — how many connections are open against `max_connections`, how many are idle vs. actually in use, how many callers are blocked waiting for one right now, and how many acquires have ever succeeded:

```rust
let stats = engine.pool_stats();
println!(
    "{}/{} connections in use, {} waiting",
    stats.in_use, stats.max_connections, stats.waiters
);
```

This is a read-only view of the underlying pool — `active`/`idle`/`in_use` and `max_connections` cost nothing extra to compute (they're already tracked by the pool itself); `waiters` and `total_acquires` are the two things it doesn't expose on its own, so each driver keeps a small `PoolMetrics` counter alongside its pool for those.

### Encrypted connections (TLS)

Postgres and MySQL/MariaDB connections can be encrypted; there's no `rusty_db`-specific API for this — `connect`/`engine`/`connect_with` all just hand the connection URL straight through to `sqlx`, and TLS is controlled entirely by standard URL query parameters `sqlx`'s own Postgres/MySQL drivers already understand (backed by `tls-rustls`):

```rust
// Postgres: sslmode = disable | prefer (default) | require | verify-ca | verify-full
let engine = PostgresDriver::engine("postgres://user:pass@host/db?sslmode=verify-full&sslrootcert=/path/to/ca.pem").await?;

// MySQL/MariaDB: ssl-mode = DISABLED | PREFERRED (default) | REQUIRED | VERIFY_CA | VERIFY_IDENTITY
let engine = MySqlDriver::engine("mysql://user:pass@host/db?ssl-mode=VERIFY_CA&ssl-ca=/path/to/ca.pem").await?;
```

Both drivers default to opportunistically using TLS already (`prefer`/`PREFERRED`) if the server supports it, without requiring anything from the caller. `require`/`REQUIRED` encrypts without verifying the server's certificate at all (vulnerable to a MITM that presents any certificate); `verify-ca`/`VERIFY_CA` and `verify-full`/`VERIFY_IDENTITY` additionally check the certificate against a trusted root (`verify-full`/`VERIFY_IDENTITY` also checks the hostname matches). SQLite has no network/TLS concept at all — it's a local file, not a network protocol.

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

If a replica's connection attempt fails, the read automatically retries the next replica in the rotation instead of failing outright, and falls back to the primary if every replica turns out to be unreachable — a `ReplicaSet` built with no replicas at all is simply a primary-only fallback, not a case callers need to special-case. `ReplicaSet::session()` deliberately hands back a `Session` backed by the primary, never a replica: a session's autoflush/identity-map guarantees depend on reading back its own not-yet-committed writes, which a lagging replica can't promise.

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

### Backup and restore

`Engine::backup`/`restore` do a *logical* (row-data) backup — built entirely on `list_tables`/`table_schema` and the query builder, not any database-specific backup mechanism (`pg_dump`, SQLite's own backup API, etc) — so a `DatabaseDump` is the same shape regardless of which backend produced it, and can even be restored into a different `Engine` than the one it came from:

```rust
let dump = engine.backup().await?; // every row of every table

// ... time passes, data changes ...

engine.restore(&dump).await?; // every dumped table: delete all rows, re-insert the dump, atomically
```

`restore` runs as one transaction — every table is cleared and refilled, and a failure partway through rolls the *entire* restore back rather than leaving the database half-restored. `backup_tables(&["users", "orders"])` scopes a backup (and therefore a subsequent `restore`) to specific tables instead of every table in the database, e.g. for a test that shares a live server with other work and can't safely risk `restore` touching tables it doesn't own. Neither method knows about foreign keys — tables are deleted/re-inserted independently in the dump's own order, so schemas with cross-table constraints may need the caller to think about ordering (or deferred constraints) themselves.

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

### DISTINCT, BETWEEN, ILIKE, and RETURNING on UPDATE/DELETE

`Select::distinct()` adds `SELECT DISTINCT`; `Column::between(low, high)` builds an inclusive `BETWEEN ... AND ...`; `Column::ilike(pattern)` is a case-insensitive `LIKE` (Postgres's native `ILIKE` keyword — everywhere else it falls back to plain `LIKE`, which is already case-insensitive under SQLite's and MySQL/MariaDB's typical default collations, though not guaranteed if a case-sensitive one is configured); and `.returning(...)` — previously `Insert`-only — is now also available on `Update`/`Delete`, gated the same way (only honored where `Dialect::supports_returning()` is true, currently just Postgres):

```rust
let orders = Table::new("orders");

let query = Select::from(&orders)
    .columns([orders.col("customer")])
    .distinct()
    .filter(orders.col("amount").between(10_i64, 100_i64))
    .filter(orders.col("customer").ilike("%ada%"));

// Postgres only (RETURNING is ignored elsewhere, per Dialect::supports_returning()):
let updated = engine
    .fetch_one(
        &Update::table(&orders)
            .set("status", "shipped")
            .filter(orders.col("id").eq(1_i64))
            .returning(["id", "status"]),
    )
    .await?;
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

Field types are limited to whatever `Value` already converts from (`bool`, `i64`, `i32`, `f64`, `String`, `Vec<u8>`, and `Option<_>` of those) — there's no arbitrary custom-type support yet. A field additionally marked `#[table(version)]` (requires `#[table(primary_key)]` too) turns on optimistic locking — see below.

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

### Bulk insert

`session.add_all(&entities)` queues every entity in a slice as one multi-row `INSERT` (`BulkInsert`) — one statement, and one round trip at flush time, instead of one per row the way repeated `add` calls would:

```rust
let users = vec![
    User { id: 1, name: "ada".into(), active: true },
    User { id: 2, name: "grace".into(), active: true },
    User { id: 3, name: "linus".into(), active: true },
];
session.add_all(&users);
session.commit().await?; // one INSERT with three rows of VALUES
```

`BulkInsert::combine` builds this directly from ordinary `Insert`s — the same ones `Entity::insert()` (and so any `#[derive(Mapped)]` type) already produces — rather than a separate row-building API, so nothing about describing a single row changes: `BulkInsert::combine(entities.iter().map(Entity::insert))` works standalone too (e.g. to run with `engine.execute(...)` directly, outside a `Session`). It's a single statement, so a failure partway through (e.g. one row's primary key collides) rolls back *all* of it, same as any other `Session` write — there's no partial success to reason about. An empty slice is a no-op.

### Audit logging / change tracking

`session.with_audit_log()` records every write the session flushes into an append-only audit table (`_rusty_db_audit_log` by default — `with_audit_log_table` picks another), inside the *same transaction* as the write itself: an audit entry only ever exists for a change that actually took effect, and if the transaction rolls back, any audit entries flushed into it roll back right along with it. Opt-in — a plain `session()` never creates or touches an audit table at all.

```rust
let mut session = engine.session().with_audit_log();

session.add(&User { id: 1, name: "ada".into(), active: true });
session.commit().await?;

for entry in session.audit_log().await? {
    println!("{:?} on {} — {} [{}]", entry.operation, entry.table, entry.sql, entry.params_text);
}
```

This records the rendered SQL statement and its bound parameters (formatted to text) for each write — a lightweight write-ahead trail of *which statement ran, on which table, when*, not a structured before/after diff of column values. `audit_log()` autoflushes and reads through the session's own transaction first (the same "see your own writes" behavior `get`/`load_all` have), so it reflects writes this session has flushed but not yet committed too.

### Optimistic locking

A `#[table(version)]` field (alongside `#[table(primary_key)]`) makes `Session::update`/`delete` detect conflicting concurrent writes instead of silently overwriting (or deleting on top of) a change somebody else already made:

```rust
#[derive(Mapped)]
#[table(name = "documents")]
struct Document {
    #[table(primary_key)]
    id: i64,
    #[table(version)]
    version: i64,
    title: String,
}

session.update(&Document { id: 1, version: 1, title: "final".into() });
session.commit().await?; // succeeds; the stored row's version becomes 2

// ...using that same stale version: 1 copy again later...
session.update(&Document { id: 1, version: 1, title: "clobbering edit".into() });
session.commit().await?; // Err(Error::Conflict(_)) — the stored version is already 2
```

`update`'s generated `WHERE` clause requires the version column to still match what this struct was loaded with (and its `SET` clause increments it); `delete_query`'s `WHERE` clause requires the same match, unchanged. When `Session::update`/`delete`'s statement ends up matching zero rows — because the version moved on (or the row's gone entirely) — `flush`/`commit` return `Error::Conflict` instead of treating it as a silent no-op. A type with no `#[table(version)]` field is completely unaffected: a stale/missing-row update or delete on it stays a silent no-op, exactly as before this feature existed.

### Soft deletes

A `#[table(soft_delete)]` field (a `bool` column, alongside `#[table(primary_key)]`) turns `Session::delete` from a real `DELETE` into a marker `UPDATE`, and makes reads transparently skip marked rows:

```rust
#[derive(Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    #[table(soft_delete)]
    deleted: bool,
    name: String,
}

session.delete(&user); // issues `UPDATE users SET deleted = true WHERE id = 1`, not a DELETE
session.commit().await?;

assert_eq!(session.get::<User>(1_i64).await?, None); // treated the same as a row that was never there
let active = session.load_active::<User>().await?; // every row except ones marked deleted
```

`Mapped::not_deleted_filter()` gives you the same `<column> = false` condition for building into your own queries (`Select::from(&table).filter(User::not_deleted_filter().unwrap())`) — it needs no per-type code generation, since it's built entirely from `TABLE_NAME` and the `SOFT_DELETE_COLUMN` const at the trait-default level. `delete_query()` itself is untouched — always a real `DELETE` — so calling it directly still gives you a genuine hard delete on a soft-deletable type if you ever need one. A type with no `#[table(soft_delete)]` field is completely unaffected: `delete` stays a real delete, and `get`/`load_active` (equivalent to `load_all` here) see every row.

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

`tests/backup_restore.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `backup`/`restore`: a backup captures every row of every table; restoring returns the database to exactly its backed-up state after rows are deleted, updated, and added; a dump backed up from one `Engine` restores correctly into a completely different one; a failing restore (a corrupted dump with a duplicate primary key partway through) rolls back the *entire* transaction rather than leaving the table partially wiped; and backing up an empty table round-trips correctly. The Postgres/MySQL versions scope every backup/restore to their own table via `backup_tables`, since a whole-database `restore()` would otherwise risk wiping tables that other tests running concurrently against the same shared live server still need.

`tests/tls_postgres.rs`/`tests/tls_mysql.rs` cover encrypted connections (no SQLite equivalent — it has no network/TLS concept at all): the default `sslmode`/`ssl-mode` (`prefer`/`PREFERRED`) already opportunistically encrypts against a server that supports it, verified against the server's own bookkeeping (`pg_stat_ssl`, `SHOW STATUS LIKE 'Ssl_cipher'`) rather than just assuming the connection attempt succeeding means it's encrypted; `disable`/`DISABLED` produces a genuinely plain connection; `require`/`REQUIRED` encrypts without verifying the certificate; and `verify-full`/`verify-ca`/`VERIFY_CA` succeed with the server's actual CA (and, for `verify-full`, matching hostname) but fail closed against a CA that doesn't match — proving certificate verification actually verifies something rather than silently accepting any well-formed root. This environment's Postgres already has TLS enabled by default (a self-signed cert the Debian/Ubuntu package generates automatically); MariaDB needed a one-time setup of a self-signed CA/server certificate (`ssl-ca`/`ssl-cert`/`ssl-key` in `/etc/mysql/mariadb.conf.d/50-server.cnf`) to have any TLS support to test against at all.

`tests/audit_log.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly exercise change tracking, since the identity-map/uncommitted-write-visibility behavior is already covered by the SQLite version and doesn't need re-proving per driver) cover `Session`'s opt-in audit logging: a plain session never creates an audit table at all; insert/update/delete each get recorded with the right table, operation, and rendered SQL/params; a failed write's audit entry rolls back right along with the write itself (proving the audit trail shares the write's own transaction rather than being an independent, potentially-inconsistent side effect); the session's own `audit_log()` sees its not-yet-committed entries (autoflush + read-through-transaction, same as `get`/`load_all`); and a custom audit table name is honored. The Postgres/MySQL versions use their own audit table name per test (`with_audit_log_table`) to avoid colliding with other tests running concurrently against the same shared live server, and are careful to `commit()` (closing the transaction `audit_log()` itself opens) before any cleanup `DROP TABLE` from a separate connection — otherwise the cleanup deadlocks waiting on a lock the still-open session transaction is holding.

`tests/optimistic_locking.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — the two conflict-detection tests, since the identity/no-op-without-`#[table(version)]` cases don't need re-proving per driver) cover `#[table(version)]`: updating with the current version succeeds and increments the stored version; updating with a stale version (superseded by someone else's edit) fails with `Error::Conflict` and leaves the other edit intact; updating a row someone else already deleted also conflicts; the same two cases for `delete`; and a type with no `#[table(version)]` field keeps its pre-existing behavior of a silent no-op on a stale/missing-row write — proving the feature is fully opt-in.

`tests/bulk_insert.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — the round-trip and rollback tests, since `BulkInsert::combine`'s own rendering/validation is pure query-builder logic that doesn't need a live server to re-prove per driver) cover `BulkInsert`/`Session::add_all`: combining several `Insert`s renders one statement with one `VALUES` group per row; combining zero inserts yields `None` rather than an invalid empty statement; combining `Insert`s from different tables is rejected; `add_all` queues exactly one pending write for the whole slice (not one per entity) and an empty slice is a no-op; a failing bulk insert (a duplicate primary key partway through) rolls back the entire batch, including rows that would've inserted fine on their own; and a `BulkInsert` works standalone through `engine.execute()`, without a `Session` at all.

`tests/soft_delete.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly exercise the feature end to end, since the query-builder-level `not_deleted_filter`/`load_active` coverage and the plain-type-is-unaffected case don't need re-proving per driver) cover `#[table(soft_delete)]`: `Session::delete` marks the row (`SET <column> = true`) instead of removing it; `Session::get` treats an already-marked row the same as one that was never there, including from a fresh `Session` with no identity-map cache from before the delete; `Mapped::not_deleted_filter()` and `Session::load_active` both exclude marked rows from query results; calling an entity's own `delete_query()` directly still issues a real, unmarked `DELETE`; and a type with no `#[table(soft_delete)]` field keeps `Session::delete`'s pre-existing behavior of a genuine hard delete, proving the feature is fully opt-in.

`tests/pool_stats.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests proving `pool_stats()` against a real network-backed pool, since the pure counting behavior doesn't need re-proving per driver) cover `Engine::pool_stats()`: checking out a connection is reflected immediately (`in_use` up, `total_acquires` up), and releasing it moves it back to `idle` without touching `total_acquires`; the counter keeps accumulating across repeated checkouts rather than just tracking the current one; and `waiters` goes to `1` while a second acquire is genuinely blocked behind a pool of size one, then back to `0` once it unblocks. Discovering the exact numbers to assert here surfaced two things worth knowing about sqlx's own pool: it eagerly opens (and keeps, idle) one connection up front to validate the URL when the pool is first constructed, so a "fresh" pool already reports that one as active/idle even though `total_acquires` correctly stays `0` (that startup connection never went through `Driver::connect`); and releasing a connection back to the pool isn't synchronous inside `drop` — the tests give it a brief moment (`tokio::time::sleep`) before reading the snapshot back, the same pattern already used elsewhere in this suite for other not-quite-synchronous async cleanup. `waiters`/`total_acquires` are the two numbers sqlx doesn't expose on its own, so each driver keeps a small `PoolMetrics` (a couple of atomics behind an `Arc`) alongside its pool for just those two; every other `PoolStats` field is a zero-cost read of the pool itself.

`tests/query_builder_extras.rs` (SQLite) and its `_postgres` counterpart (a reduced version there — just the two things that are actually Postgres-specific, `ILIKE` and `RETURNING` on `UPDATE`/`DELETE`; `DISTINCT`/`BETWEEN` have no dialect-specific behavior and don't need re-proving against a live server) cover the newer query-builder additions against a real SQL engine rather than just checking rendered SQL strings: `Select::distinct()` actually dedupes matching rows; `Column::between` includes both boundaries inclusively; `Column::ilike` matches case-insensitively via its portable `LIKE` fallback on SQLite and via Postgres's native `ILIKE` keyword; and `.returning(...)` on `Update`/`Delete` actually returns the requested columns from a real Postgres server (and is silently ignored on SQLite, whose dialect doesn't support `RETURNING` at all). The pure rendering side of all four — including that `RETURNING` is dialect-gated and `ilike_operator()` picks the right keyword per dialect — is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`, alongside the rest of the query builder's SQL generation.

## Running tests

```
cargo test --workspace --all-features
```
