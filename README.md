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

Both Postgres and MySQL send several column types over their own binary wire formats rather than as text — `NUMERIC`, `DATE`/`TIME`/`TIMESTAMP(TZ)`, and `JSON`/`JSONB` for Postgres; `DATE`/`TIME`/`DATETIME`/`TIMESTAMP` for MySQL — so those get decoded through the matching typed `sqlx` decoder (`BigDecimal`, `chrono`, `serde_json::Value`) and formatted to `Value::Text`, rather than assumed to already be UTF-8 text like the generic fallback does for everything else (`TEXT`/`VARCHAR`/`CHAR`/etc.). `UUID` gets its own dedicated treatment — see below.

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

`TableSchema` also reflects each column's default (`ColumnInfo::default`, verbatim text like a literal `"0"` or an expression like `"nextval('users_id_seq'::regclass)"`, `None` if it has none), plus the table's `UNIQUE` and `CHECK` constraints:

```rust
if let Some(schema) = engine.table_schema("accounts").await? {
    for unique in &schema.unique_constraints {
        println!("UNIQUE {}: {:?}", unique.name, unique.columns);
    }
    for check in &schema.check_constraints {
        println!("CHECK {}: {}", check.name, check.expression);
    }
}
```

`CheckConstraint::expression` is verbatim catalog text too, same as `type_name`/`default` — no attempt to parse or evaluate it. SQLite is a special case for both of these: it implements `UNIQUE` as an index rather than a true named constraint, so `UniqueConstraint::name` there is the backing index's name (an explicit one, or SQLite's auto-generated `sqlite_autoindex_<table>_<n>`), not necessarily whatever name a `CONSTRAINT <name> UNIQUE (...)` clause gave it; and SQLite has no catalog for `CHECK` constraints at all, so its `check_constraints` are recovered with a best-effort text scan of the table's own `CREATE TABLE` statement (a small tokenizer that understands quoted literals and balanced parens, not a full SQL parser) — an inline `CHECK (...)` with no explicit `CONSTRAINT <name>` gets a synthetic, positional name (`"check_1"`, `"check_2"`, ...).

MySQL/MariaDB has its own `default` quirk: `information_schema.columns.column_default` reports the literal, unquoted text `"NULL"` for a nullable column with no meaningful default — both when no `DEFAULT` clause was given at all, and for an explicit `DEFAULT NULL` (indistinguishable in the catalog, and behaving identically anyway). Reported verbatim, that would surface as `Some("NULL")` where Postgres and SQLite both report `None` for the same case — so `rusty-db-mysql` normalizes a nullable column's bare `"NULL"` catalog text to `None`, matching the other two backends. A genuine string-literal default of the text `NULL` is reported quoted (`"'NULL'"`) and is left untouched, since that's a real default rather than this ambiguity.

`schema.foreign_keys` reflects each foreign key: the column(s) in this table (in order) and the table/column(s) they reference (in the same order, so `columns[i]` references `referenced_columns[i]`) — correct even for a composite (multi-column) foreign key:

```rust
if let Some(schema) = engine.table_schema("orders").await? {
    for fk in &schema.foreign_keys {
        println!("{}: {:?} -> {}.{:?}", fk.name, fk.columns, fk.referenced_table, fk.referenced_columns);
    }
}
```

SQLite doesn't name foreign keys at all (there's no `CONSTRAINT <name>` for them), so `ForeignKey::name` there is synthetic (`"fk_1"`, `"fk_2"`, ...) rather than anything recoverable from the database.

`schema.indexes` reflects every index on the table — its name, the column(s) it covers in index order, and whether it enforces uniqueness — including ones that also back a `UNIQUE` constraint (already covered separately by `unique_constraints`), but excluding the index automatically backing the primary key itself (already `ColumnInfo::primary_key`):

```rust
if let Some(schema) = engine.table_schema("people").await? {
    for index in &schema.indexes {
        println!("{}: {:?} (unique={})", index.name, index.columns, index.unique);
    }
}
```

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

`Table::alias(...)` gives a second, independent reference to the same underlying table — for a self-join, or anywhere the same table needs to appear more than once in one query. `.col(...)` on the alias qualifies with the alias, not the original table name, and `Select`/`join` render `<table> AS <alias>` for it:

```rust
let employees = Table::new("employees");
let managers = employees.alias("managers");

let query = Select::from(&employees)
    .columns([employees.col("name"), managers.col("name")])
    .join(&managers, employees.col("manager_id").eq_col(&managers.col("id")));
// SELECT "employees"."name", "managers"."name" FROM "employees"
// INNER JOIN "employees" AS "managers" ON "employees"."manager_id" = "managers"."id"
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

### Raw SQL fragments composed into the builder (`Expr::text`)

`Expr::text(sql, params)` drops a raw SQL fragment into an otherwise builder-constructed query — the escape hatch for a database-specific function or anything else the builder doesn't model yet, without losing composability with `Select`'s other clauses (unlike `Engine::connect()`/`Transaction::execute`, which run a whole statement standalone). It composes with ordinary `Expr`s via `.and`/`.or` exactly like any other filter:

```rust
let orders = Table::new("orders");

let query = Select::from(&orders).filter(
    Expr::text("lower(customer) = ?", [Value::Text("ada".to_string())])
        .and(orders.col("amount").gt(20_i64)),
);
```

Write `sql` in whichever dialect you're actually targeting (it's inserted verbatim, so it isn't portable across backends unless you keep it portable yourself); bind parameters use `?` placeholders regardless of the target dialect, rewritten to that dialect's real placeholder syntax (`$1`, `?`, ...) in the order they appear. This is a plain character scan for `?`, not a SQL parser, so avoid a literal `?` elsewhere in the fragment (e.g. inside a quoted string).

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

Field types are limited to whatever `Value` already converts from (`bool`, `i64`, `i32`, `f64`, `String`, `Vec<u8>`, `Uuid`, `BigDecimal`, `Json`, and `Option<_>` of those) — there's no arbitrary custom-type support yet. A field additionally marked `#[table(version)]` (requires `#[table(primary_key)]` too) turns on optimistic locking — see below.

### UUID values

`Value::Uuid` (re-exported as `rusty_db::Uuid` — the exact `uuid` crate type `Value::Uuid` wraps, so a mapped field's version can never mismatch) is a first-class value, not flattened to text like most of the "wide" column types mentioned above:

```rust
#[derive(Mapped)]
#[table(name = "widgets")]
struct Widget {
    #[table(primary_key)]
    id: Uuid,
    name: String,
    owner: Option<Uuid>,
}

let widget = Widget { id: Uuid::new_v4(), name: "gizmo".into(), owner: None };
engine.execute(&widget.insert()).await?;
```

Postgres has a native `UUID` column type and round-trips this variant directly over its binary wire format — a `UUID` column decodes as `Value::Uuid`, not `Value::Text`. MySQL/MariaDB and SQLite have no such type (a UUID column there is really just `CHAR(36)`/`TEXT`), so those two bind it as its hyphenated string form and a UUID column decodes back as plain `Value::Text` — but `FromValue for Uuid` parses that text form too, so a mapped struct's `Uuid` field round-trips correctly on every backend, just without Postgres's native wire format on the other two.

### Decimal values

`Value::Decimal` (re-exported as `rusty_db::BigDecimal`, the exact `bigdecimal` crate type it wraps) is the arbitrary-precision counterpart to `Uuid` above — an `f64` field would silently lose precision on a `NUMERIC`/`DECIMAL` column, so this gets its own dedicated variant instead:

```rust
#[derive(Mapped)]
#[table(name = "invoices")]
struct Invoice {
    #[table(primary_key)]
    id: i64,
    total: BigDecimal,
    discount: Option<BigDecimal>,
}

let invoice = Invoice {
    id: 1,
    total: "129.99".parse().unwrap(),
    discount: None,
};
engine.execute(&invoice.insert()).await?;
```

Same split as `Uuid`: Postgres has a native `NUMERIC` type and round-trips this variant directly over its binary wire format (a `NUMERIC` column decodes as `Value::Decimal`, not `Value::Text`). MySQL/MariaDB sends `DECIMAL` as text on its own wire protocol, and SQLite has no such type at all (a `NUMERIC`-affinity column there decodes as whatever runtime type the stored value actually has) — `FromValue for BigDecimal` accepts `Value::Text`/`Value::I64`/`Value::F64` too (parsing or converting as needed), so a mapped struct's `BigDecimal` field round-trips correctly on every backend, just without Postgres's native wire format (and, via the `f64` fallback specifically, without arbitrary precision beyond what an `f64` itself preserves).

### JSON values

`Value::Json` (re-exported as `rusty_db::Json` — `serde_json`'s own `Value` type, renamed on the way out since `rusty_db` already has its own `Value`) rounds out the same treatment for `JSON`/`JSONB` columns:

```rust
#[derive(Mapped)]
#[table(name = "events")]
struct Event {
    #[table(primary_key)]
    id: i64,
    name: String,
    payload: Json,
    metadata: Option<Json>,
}

let event = Event {
    id: 1,
    name: "signup".into(),
    payload: serde_json::json!({"user_id": 42, "plan": "pro"}),
    metadata: None,
};
engine.execute(&event.insert()).await?;
```

Postgres has native `JSON`/`JSONB` types and round-trips this variant directly over its binary wire format. SQLite has no JSON type at all (a JSON column there is really just `TEXT`), so it decodes a JSON column back as plain `Value::Text`. MySQL/MariaDB's own `JSON` type is a stranger case: it reports as one of MySQL's `BLOB`-family types at the wire-protocol level even though the bytes themselves are plain UTF-8 JSON text, so it decodes back as `Value::Bytes` instead of `Value::Text`. `FromValue for Json` parses both of those forms, so a mapped struct's `Json` field round-trips correctly on every backend, just without Postgres's native wire format on the other two.

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

### Bulk update / delete

`session.bulk_update`/`bulk_delete` queue an arbitrary, filter-scoped `UPDATE`/`DELETE` against a type's table — not bound to any single entity — for changing or removing every row matching a filter in one statement, instead of loading each one and writing it back individually:

```rust
let table = User::table();
session.bulk_update::<User>(
    Update::table(&table)
        .set("active", false)
        .filter(table.col("last_login").lt(cutoff)),
);
session.bulk_delete::<User>(Delete::from(&table).filter(table.col("active").eq(false)));
session.commit().await?; // both run as part of the same flush/transaction
```

Both are queued the same way `add`/`update`/`delete` are (not sent until the next flush) and audit-logged the same way when enabled. Two things are deliberately bypassed, since neither makes sense for a filter-scoped write that isn't tied to one loaded instance: the identity map (any already-cached instances of the type stay exactly as they were in memory — evict them yourself with `clear_identity_map()` if that matters) and, for `bulk_delete`, a `#[table(soft_delete)]` column (it's always a real, hard `DELETE`; use `Session::delete` for the soft-delete-aware, single-entity path).

### Savepoints (nested transactions)

`session.savepoint()` marks a point inside the session's ongoing transaction that `rollback_to_savepoint` can later undo back to — for a sub-unit of work that might fail and need undoing on its own — without aborting the whole transaction the way a full `Session::rollback()` would:

```rust
session.add(&User { id: 1, name: "ada".into(), active: true });
session.flush().await?;

let sp = session.savepoint().await?;
session.add(&User { id: 2, name: "risky-write".into(), active: true });
if something_went_wrong {
    session.rollback_to_savepoint(&sp).await?; // undoes just the risky write
} else {
    session.release_savepoint(sp).await?; // keeps it, done with this savepoint
}

session.add(&User { id: 3, name: "grace".into(), active: true });
session.commit().await?; // ada and (if kept) the risky write and grace all land
```

`rollback_to_savepoint` undoes everything since the savepoint — whether already flushed into the transaction or still only queued (those are simply discarded, since they never reached the database) — and the session keeps going afterward in the same, still-open transaction. `release_savepoint` flushes anything queued since the savepoint and keeps its effects, just without being able to roll back to it anymore; it's never required before `commit()`/`rollback()`, since an unreleased savepoint is released or undone right along with the whole transaction either way. Savepoints nest: creating one while another is still open just works, using generated, unique names under the hood.

### Two-phase (prepared) commit

`Engine::begin_two_phase`/`Transaction::prepare`/`Engine::commit_prepared`/`Engine::rollback_prepared` split an ordinary transaction's commit into two steps — durably *prepare* it, then separately *finalize* it — for coordinating a single logical transaction across more than one participant (e.g. two databases, or a transaction whose outcome depends on something outside the database entirely), where every participant needs to confirm it *can* commit before any of them actually do:

```rust
let mut txn = engine.begin_two_phase("order-42").await?; // "order-42": a caller-chosen id
txn.execute("INSERT INTO orders (id) VALUES (42)", &[]).await?;
txn.prepare(engine.dialect()).await?; // durably recorded, not yet visible to anyone

// ... once every other participant a coordinator is waiting on has also
// prepared successfully, likely from a different connection or process ...
engine.commit_prepared("order-42").await?; // finalizes it — now visible
// or: engine.rollback_prepared("order-42").await?; // discards it instead
```

This is Postgres's `PREPARE TRANSACTION`/`COMMIT PREPARED`/`ROLLBACK PREPARED` and MySQL/MariaDB's `XA START`/`XA END`/`XA PREPARE`/`XA COMMIT`/`XA ROLLBACK` under a single portable API; `Err(Error::Unsupported)` on SQLite, which has no such concept. Postgres requires `max_prepared_transactions > 0` on the server (`0`, i.e. disabled, is the shipped default) — `PREPARE TRANSACTION` itself fails otherwise. `commit_prepared`/`rollback_prepared` take just the id, not a `Transaction` handle, since the whole point of the first phase is that a prepared transaction survives independently of the connection (or even the process) that prepared it — a real coordinator commonly resolves it from somewhere else entirely, possibly well after `prepare()` returns.

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

### Fluent queries

`session.query::<T>()` is a fluent, type-bound alternative to building a `Select::from(&T::table())` yourself and passing it to `load_all` — `.filter`/`.order_by`/`.limit`/`.offset` mirror `Select`'s own builder methods, and `.active_only()` mirrors `load_active`'s soft-delete filtering (a no-op for a type with no `#[table(soft_delete)]` column):

```rust
let table = User::table();
let recent_active = session
    .query::<User>()
    .filter(table.col("active").eq(true))
    .order_by(table.col("id").desc())
    .limit(10)
    .all()
    .await?; // Vec<Rc<RefCell<User>>>

let first_match = session
    .query::<User>()
    .filter(table.col("name").eq("ada"))
    .first()
    .await?; // Option<Rc<RefCell<User>>>
```

`.all()`/`.first()` (which just adds `LIMIT 1`) run the query (autoflushing first, same as `load_all`/`get`) and decode results through the session's identity map exactly the same way — a row already cached comes back as the same handle, in-memory changes and all.

### Session hooks

`session.on_before_flush`/`on_after_flush`/`on_before_commit`/`on_after_commit`/`on_after_rollback` register plain callbacks that fire at specific points in a session's lifecycle — for logging, metrics, cache invalidation, or anything else that should happen alongside a flush/commit/rollback without threading extra code through every call site:

```rust
session.on_before_flush(|| println!("about to send queued writes"));
session.on_after_commit(|| println!("transaction committed"));

session.add(&User { id: 1, name: "ada".into(), active: true });
session.commit().await?; // fires on_before_flush, then on_after_commit
```

Hooks are plain `FnMut()` closures (no `async` support — they fire at a specific synchronous point inside an already-`async fn`, not as their own awaited step) and run in registration order. `on_before_flush`/`on_after_flush` only fire when a flush actually has queued writes to send (a flush with nothing pending — including the implicit ones inside `get`/`load_all`/`query`/`commit`/`savepoint` — is a no-op and doesn't trigger them), and `on_after_flush` never fires for a flush that failed. `on_before_commit` fires on every `commit()` call, pending writes or not; `on_after_commit`/`on_after_rollback` only fire when a transaction actually existed to commit/roll back — calling `commit()`/`rollback()` when nothing was ever flushed or read (so no transaction was ever opened) doesn't trigger them.

### Expiring on commit

`engine.session().with_expire_on_commit()` clears the whole identity map right after every successful `commit()`, so the next `get`/`load_all`/`query` for a row re-fetches it fresh from the database instead of handing back a cached, possibly now-stale in-memory handle:

```rust
let mut session = engine.session().with_expire_on_commit();

let ada = session.get::<User>(1_i64).await?.unwrap();
ada.borrow_mut().name = "not yet saved".into(); // in-memory only

session.add(&User { id: 2, name: "grace".into() });
session.commit().await?; // clears the identity map, since a real commit happened

let ada_again = session.get::<User>(1_i64).await?.unwrap(); // fresh fetch, not the stale handle above
```

This is coarser than SQLAlchemy's own `expire_on_commit`, which lazily re-fetches each *attribute* the next time it's accessed rather than the whole object — rusty_db's mapped types are plain structs with no attribute-access proxy to hook into, so there's no per-field equivalent to offer, only clearing the map outright. It only affects what a *future* `get`/`load_all`/`query` call returns — handles you already hold keep working exactly as `clear_identity_map()` describes, and a `commit()` with nothing to commit (no transaction was ever opened) doesn't clear anything. Off by default, unlike SQLAlchemy's `Session` (where `expire_on_commit=True` is the default) — every behavior change in this crate is opt-in, and clearing the identity map on every commit is a real change from the caching `Session` otherwise does.

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

`#[has_one(Child, foreign_key = "...")]` is the same direction as `has_many` (keyed by `Self`'s own primary key, referenced by the child's `foreign_key` column), but for a relationship expected to have at most one matching row per parent — today's only alternative, a `belongs_to` pointed the wrong way, gives you neither the right ergonomics (you'd still get a `Vec`-shaped or backwards-keyed result) nor any check that the relationship is really 1:1:

```rust
#[derive(Mapped)]
#[table(name = "users")]
#[has_one(Profile, foreign_key = "user_id")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[derive(Mapped)]
#[table(name = "profiles")]
struct Profile {
    #[table(primary_key)]
    id: i64,
    user_id: i64,
    bio: String,
}

let users: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
let profiles_by_user: HashMap<i64, Profile> = User::load_profile(&engine, &users).await?;
// a user with no profile just has no entry in the map
```

The loader is named after the singular snake_case child type (`load_profile`, not pluralized like `has_many`'s), and returns `Child` directly rather than `Vec<Child>`. If a second `Profile` row ever turns up referencing the same user, the loader returns `Err(Error::Conflict)` instead of silently keeping (or silently dropping) one of them — exactly the validation SQLAlchemy's own `uselist=False` gives you and a `has_many`/reversed-`belongs_to` workaround can't. Generated from, and callable directly as, `rusty_db::relations::load_has_one`.

`#[many_to_many(Target, through = "...", local_key = "...", foreign_key = "...")]` covers the one relationship shape `has_many`/`has_one`/`belongs_to` can't express at all: two tables related through a join table, with no FK on either side directly pointing at the other. `through` is the join table's name; `local_key` is the join table's column referencing `Self`; `foreign_key` is the join table's column referencing `Target` (naming it consistently with `has_many`/`belongs_to`'s own `foreign_key`, even though here it's on the *join* table rather than either side):

```rust
#[derive(Mapped)]
#[table(name = "posts")]
#[many_to_many(Tag, through = "post_tags", local_key = "post_id", foreign_key = "tag_id")]
struct Post {
    #[table(primary_key)]
    id: i64,
    title: String,
}

#[derive(Mapped)]
#[table(name = "tags")]
struct Tag {
    #[table(primary_key)]
    id: i64,
    name: String,
}
// post_tags(post_id, tag_id) — a plain join table, no #[derive(Mapped)] needed for it at all

let posts: Vec<Post> = engine.fetch_all_as(&Select::from(&Post::table())).await?;
let tags_by_post: HashMap<i64, Vec<Tag>> = Post::load_tags(&engine, &posts).await?;
// a post with no tags at all just has no entry in the map
```

Unlike `has_many`/`has_one`/`belongs_to`, which only ever need one extra query beyond the parents you already have, this one's a real SQL `JOIN` against the join table rather than a plain `WHERE ... IN (...)` — still exactly one extra query for the whole batch, just joined instead of filtered. Generated from, and callable directly as, `rusty_db::relations::load_many_to_many`. The three named parameters can appear in any order after `Target`.

### Cascade rules

`has_many`/`has_one`/`many_to_many` accept an optional `cascade = "delete"` or `cascade = "orphan"` parameter (`many_to_many` only supports `"delete"`). Any relation carrying one generates `Self::delete_cascading(&self, engine)`, which runs every cascading relationship's cleanup query, then deletes `self`, all inside one transaction:

```rust
#[derive(Mapped)]
#[table(name = "users")]
#[has_many(Order, foreign_key = "user_id", cascade = "delete")]
#[has_one(Profile, foreign_key = "user_id", cascade = "delete")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

let ada: User = /* ... */;
ada.delete_cascading(&engine).await?; // deletes ada's orders and profile, then ada herself
```

`cascade = "delete"` on `has_many`/`has_one` issues a `DELETE` against the child table filtered by the foreign key; `cascade = "orphan"` issues an `UPDATE ... SET <foreign_key> = NULL` instead, so the children survive with a null foreign key rather than being removed. `cascade = "delete"` on `many_to_many` deletes only the join-table rows for this parent — never the target's own rows, which may still be referenced by other parents (there's no `"orphan"` mode for `many_to_many`, since nulling a join table's row doesn't mean anything; the row itself *is* the association). Without at least one `cascade = "..."` parameter somewhere on a type, `delete_cascading` simply isn't generated for it at all.

This is a plain `Engine`-based alternative to `Session::delete`, not integrated with it — no identity-map eviction, no audit logging, no soft-delete support (cascading always issues a real `DELETE`/`UPDATE`, the same way `delete_query()` does) — call it directly when you want cascading, consistent with every other relationship helper's "explicitly loaded/called, no hidden magic" philosophy.

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

This covers Core (query builder, connections, first-class `Uuid`/`BigDecimal`/`Json` value types), a thin mapping layer (`#[derive(Mapped)]`, joins, has-many/has-one/belongs-to/many-to-many eager loading, cascade delete/orphan rules), versioned migrations (standalone via `Migrator`, or folded into a session's transaction via `session.migrate`), and a unit-of-work `Session` with autoflush and an identity map (including eviction on delete). Three drivers exist — SQLite, PostgreSQL, and MySQL/MariaDB — all built the same way (wrapping `sqlx`) and all exercised by the test suite. The Postgres and MySQL tests run against real servers when reachable (`POSTGRES_TEST_URL`/`MYSQL_TEST_URL`, defaulting to local `rusty`/`rusty` test databases) and just skip themselves rather than fail if one isn't — so `cargo test` stays green without either installed, but this environment does have both, and both are actually exercised here.

`tests/concurrent_sessions.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover multiple `Session`s sharing one `Engine`/connection pool at the same time — `Session` is intentionally `!Send` (it hands out `Rc`s for the identity map), so these run via `tokio::task::LocalSet`/`spawn_local` rather than `tokio::spawn`, which is the standard way to get genuinely concurrent, interleaved execution of `!Send` futures on one thread. They cover independent commits landing correctly under a burst of concurrent sessions, one session's flushed-but-uncommitted write staying invisible to a concurrent reader on a separate connection (deterministically ordered via a `oneshot` channel, not timing), and two sessions never sharing identity-map state for the same row. Same skip-if-unreachable behavior as the Postgres/MySQL smoke tests; each test uses its own table to avoid colliding with other tests running concurrently against the same live server.

`tests/session_expire_on_commit.rs` (SQLite) covers `Session::with_expire_on_commit`: the identity map is cleared after a real commit but left untouched by a no-op `commit()` (nothing was ever flushed or read, so no transaction was ever opened), untouched entirely without the option set, and a row fetched after expiration reflects the database's actual post-commit state — including a change made directly through the underlying `Engine`, bypassing the session — rather than a stale in-memory edit or the old cached handle.

`tests/pool_exhaustion.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `PoolConfig` itself: a `connect()` beyond a pool's `max_connections` blocks rather than erroring or handing out a duplicate connection, and succeeds once an outstanding connection is dropped back to the pool (proven with `tokio::time::timeout` rather than assumptions about scheduling order); a higher `max_connections` is honored before the pool starts blocking; a configured `acquire_timeout` errors out promptly instead of blocking forever; and a burst of `Session`s serializes correctly (no lost or corrupted writes) when they're all forced to take turns on a single-connection pool.

`tests/two_phase_commit.rs` (SQLite) confirms `begin_two_phase`/`commit_prepared`/`rollback_prepared` report `Error::Unsupported` there, since SQLite has no concept of a transaction prepared independently of its connection; `_postgres`/`_mysql` exercise the real thing against a live server — a prepared-but-not-yet-committed write stays invisible, `commit_prepared` makes it visible, `rollback_prepared` discards it instead — with `commit_prepared`/`rollback_prepared` always issued from a connection distinct from the one that prepared, since a real coordinator resolving a distributed transaction typically isn't the same connection (or even the same process) that prepared it. Getting MySQL/MariaDB's `XA` transactions working correctly through a pooled connection took two fixes beyond the query builder itself: `XA COMMIT`/`XA ROLLBACK` don't reliably resolve a transaction prepared on a different connection when sent through `sqlx`'s prepared-statement protocol (works fine as plain text SQL, the same wire format the `mysql` CLI uses — an `execute_unprepared` `Connection` method, MySQL-only override, sends these as raw text instead), and a connection that just ran `XA PREPARE` is left unable to run anything else at all until it resolves its own prepared transaction, so `Transaction::prepare` closes that connection outright afterward instead of returning it to the pool for reuse (which would otherwise break whoever got it next).

`tests/replica_set.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `ReplicaSet`: reads round-robin across healthy replicas (verified by seeding each stand-in replica with its own marker row and checking which one a read actually returned, rather than peeking at any internal routing state); a down replica fails over to the next healthy one instead of surfacing an error; every replica being down falls back to the primary; a `ReplicaSet` with zero replicas configured always uses the primary; and writes (`execute`, `Session`) always land on the primary regardless of replica health. Since real database replication is a server-side feature this crate can't spin up in a sandbox, a "down" replica/primary is simulated with a minimal fake `Driver` whose `connect()` always returns `Error::Connection` — the same failure shape a genuinely unreachable server produces — rather than by actually taking a live server down mid-suite; the Postgres/MySQL versions otherwise use real, separate tables on the live servers as replica stand-ins.

`tests/relations.rs` (SQLite) now also covers `#[has_one(...)]`: a batch of parents where only some have a matching child comes back with entries only for those that do (same "no entry at all" shape `has_many` already had for a childless parent); and a parent with *two* matching child rows — a relationship that isn't actually one-to-one — returns `Error::Conflict` rather than silently keeping or dropping one of them.

It also covers `#[many_to_many(...)]`: a batch of parents joined through a join table to a shared and a distinct set of targets comes back grouped correctly per parent (a post tagged `rust`+`systems` and another tagged `rust`+`databases` both get the right two tags, with `rust` correctly appearing under both), a parent with no join-table rows at all has no entry in the map, and empty input returns an empty map, same as the other three relationship kinds.

It also covers cascade rules (`delete_cascading`): deleting a user with `cascade = "delete"` `has_many`/`has_one` relations removes both its orders and its profile along with the user itself, while a different user's own orders are left completely untouched; deleting a team with a `cascade = "orphan"` `has_many` leaves its players in place but with their foreign key nulled out rather than deleting them; and deleting a post with a `cascade = "delete"` `many_to_many` removes only that post's own join-table rows — a tag shared with another post survives, and so does the other post's own join row and its own view of that tag through `load_tags`.

`tests/uuid_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Uuid`/`Uuid`: a mapped struct's `Uuid` field (including an `Option<Uuid>` one) round-trips correctly on every backend, and — Postgres-only — a native `UUID` column decodes as `Value::Uuid` directly rather than `Value::Text`. Getting a `NULL` into a nullable `Uuid` column on Postgres surfaced a real, pre-existing bug unrelated to UUIDs specifically: binding a `NULL` parameter always declared it as an `int8` (`query.bind(None::<i64>)`), which Postgres's strict per-parameter type checking then rejected for a target column of any type without an implicit/assignment cast from `int8` (`UUID`, `BOOLEAN`, and `JSON` all reproduce it) — a bug that had simply never been hit yet, since no earlier test happened to insert an explicit `NULL` into one of those column types. Fixed at the query-builder level, not the driver: `Insert`/`Update`/`BulkInsert` now render a `Value::Null` assignment as the bare SQL literal `NULL` instead of a bound placeholder, sidestepping the type-declaration conflict entirely (and doing so for every dialect, not just Postgres, since a literal `NULL` has no type to conflict with anywhere).

`tests/decimal_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Decimal`/`BigDecimal`: a mapped struct's `BigDecimal` field (including an `Option<BigDecimal>` one) round-trips correctly on every backend, and — Postgres-only — a native `NUMERIC` column decodes as `Value::Decimal` directly rather than `Value::Text`.

`tests/json_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Json`/`Json`: a mapped struct's `Json` field (including an `Option<Json>` one) round-trips correctly on every backend, and — Postgres-only — a native `JSONB` column decodes as `Value::Json` directly rather than `Value::Text`. Getting this working on MySQL/MariaDB surfaced a real quirk: its `JSON` columns report as one of MySQL's own `BLOB`-family types at the wire-protocol level, so they decode as `Value::Bytes`, not `Value::Text` the way `DECIMAL`/`UUID`-as-text columns do elsewhere on that backend — `FromValue for Json` accepts that form too (via UTF-8 first, then JSON parsing), so a `Json` field round-trips there without the caller ever needing to know about the difference.

`tests/query_timeout.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `with_timeout`: an operation finishing inside its timeout succeeds normally; a genuinely slow/blocked operation is cancelled and returns `Error::Timeout` instead of hanging; a cancelled operation leaves the pool usable afterward rather than stuck; a timeout on one call has no lingering effect on later calls; and aborting a task running a slow operation (`JoinHandle::abort`, the other standard way to cancel a Rust future) cancels it the same way a timeout would. The SQLite version gets a genuinely blocked (not simulated) query via real lock contention — holding SQLite's write lock on one connection while a second connection attempts a conflicting write, which sqlx's SQLite driver retries against its own 5-second `busy_timeout` rather than erroring immediately. The Postgres/MySQL versions use their real, built-in `pg_sleep()`/`SLEEP()` functions for a genuinely slow query instead.

`tests/schema_introspection.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `list_tables`/`table_schema`: created tables are reported (and, for SQLite, internal `sqlite_*` tables never are); a table's columns come back with the right type names, nullability, and primary-key flags; a table that doesn't exist reports `Ok(None)` rather than an error; a column's `DEFAULT` reflects as `Some(...)` verbatim text (`None` for a column with none); a `UNIQUE` constraint's name and covered column(s) come back correctly; and a `CHECK` constraint's expression comes back correctly. Chasing down why the SQLite version's PRAGMA-based columns were coming back entirely `Null` uncovered a real bug in the SQLite driver's row decoder: it was keying off each column's *declared* type (`"NULL"` when SQLite can't infer one statically — true for `PRAGMA` output and other non-plain-table results, not just when the value itself is null) instead of that value's actual *runtime* type, so it silently decoded every column as `Value::Null` whenever SQLite left the static type undeclared. Fixed by falling back to the per-value runtime type (via `try_get_raw`) exactly when the declared type is `"NULL"`, leaving the existing behavior for ordinary table queries (which do have a meaningful declared type) unchanged — confirmed by the rest of the suite still passing unmodified. The MySQL version's `table_schema_reports_defaults_unique_and_check_constraints` test also covers a MariaDB-specific `default` quirk found the same way (against a real server, not just the query builder): a nullable column with no `DEFAULT` clause at all, one with an explicit `DEFAULT NULL`, and one with a genuine string-literal default of the text `NULL` (quoted `'NULL'` in the catalog) — the first two both normalize to `None`, and the third reflects `Some("'NULL'")`, not the ambiguous ones.

Reflecting `CHECK` constraints turned up two more real per-backend quirks, each confirmed against a live server rather than assumed: Postgres's `information_schema.check_constraints` also reports a synthetic entry for every `NOT NULL` column (its catalog-level way of representing `NOT NULL`, already covered by `ColumnInfo::nullable`) — fixed by querying `pg_catalog.pg_constraint` directly and filtering to `contype = 'c'` (genuine `CHECK`), which also gives a cleaner expression via `pg_get_expr` than `information_schema`'s doubly-parenthesized text. And SQLite has no catalog for `CHECK` constraints at all, so `crates/rusty-db-sqlite/src/lib.rs`'s `check_constraints` module recovers them with its own small tokenizer over the table's `CREATE TABLE` text (unit-tested directly, independent of a live database, with 6 tests covering a named constraint, synthetic positional names for anonymous ones, an inline column-level `CHECK`, nested parens and string literals inside the expression, a `CHECK`-like word inside a string literal not being mistaken for a real one, and a table with no `CHECK` constraints at all).

Foreign key reflection (`schema.foreign_keys`) is covered by the same `tests/schema_introspection*.rs` files: both a single-column and a genuinely composite (two-column) foreign key come back with their columns correctly paired against the referenced table's columns, in order. Getting that pairing right for a composite key surfaced a real `information_schema` pitfall on Postgres: joining `key_column_usage` to `constraint_column_usage` by constraint name alone (the usual approach) has no shared ordinal between the two, so it cross-joins every local column with every referenced column of a multi-column key instead of pairing them up correctly. Fixed by querying `pg_catalog.pg_constraint`'s own `conkey`/`confkey` arrays instead and pairing them positionally via `unnest(...) WITH ORDINALITY`, which is exact regardless of how many columns are involved. MySQL/MariaDB's `information_schema.key_column_usage` already includes `referenced_table_name`/`referenced_column_name` directly on each row (a MySQL-specific extension beyond the SQL standard), so no equivalent care was needed there. SQLite doesn't name foreign keys at all, so `ForeignKey::name` there is synthetic (`"fk_1"`, `"fk_2"`, ...), grouped from `PRAGMA foreign_key_list`'s rows sharing the same `id`.

Index reflection (`schema.indexes`) rounds out the same test files: a `UNIQUE`-backed index and a plain, non-unique multi-column index both come back with the right name, columns (in index order), and `unique` flag, and the primary key's own automatically-created index is never included among them (already `ColumnInfo::primary_key`) — verified on Postgres via a `NOT EXISTS` against `pg_constraint` (the reliable way to identify it, since its name isn't predictable), on MySQL/MariaDB by excluding `index_name = 'PRIMARY'`, and on SQLite by excluding `PRAGMA index_list`'s `origin = "pk"` row.

`tests/backup_restore.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `backup`/`restore`: a backup captures every row of every table; restoring returns the database to exactly its backed-up state after rows are deleted, updated, and added; a dump backed up from one `Engine` restores correctly into a completely different one; a failing restore (a corrupted dump with a duplicate primary key partway through) rolls back the *entire* transaction rather than leaving the table partially wiped; and backing up an empty table round-trips correctly. The Postgres/MySQL versions scope every backup/restore to their own table via `backup_tables`, since a whole-database `restore()` would otherwise risk wiping tables that other tests running concurrently against the same shared live server still need.

`tests/tls_postgres.rs`/`tests/tls_mysql.rs` cover encrypted connections (no SQLite equivalent — it has no network/TLS concept at all): the default `sslmode`/`ssl-mode` (`prefer`/`PREFERRED`) already opportunistically encrypts against a server that supports it, verified against the server's own bookkeeping (`pg_stat_ssl`, `SHOW STATUS LIKE 'Ssl_cipher'`) rather than just assuming the connection attempt succeeding means it's encrypted; `disable`/`DISABLED` produces a genuinely plain connection; `require`/`REQUIRED` encrypts without verifying the certificate; and `verify-full`/`verify-ca`/`VERIFY_CA` succeed with the server's actual CA (and, for `verify-full`, matching hostname) but fail closed against a CA that doesn't match — proving certificate verification actually verifies something rather than silently accepting any well-formed root. This environment's Postgres already has TLS enabled by default (a self-signed cert the Debian/Ubuntu package generates automatically); MariaDB needed a one-time setup of a self-signed CA/server certificate (`ssl-ca`/`ssl-cert`/`ssl-key` in `/etc/mysql/mariadb.conf.d/50-server.cnf`) to have any TLS support to test against at all.

`tests/audit_log.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly exercise change tracking, since the identity-map/uncommitted-write-visibility behavior is already covered by the SQLite version and doesn't need re-proving per driver) cover `Session`'s opt-in audit logging: a plain session never creates an audit table at all; insert/update/delete each get recorded with the right table, operation, and rendered SQL/params; a failed write's audit entry rolls back right along with the write itself (proving the audit trail shares the write's own transaction rather than being an independent, potentially-inconsistent side effect); the session's own `audit_log()` sees its not-yet-committed entries (autoflush + read-through-transaction, same as `get`/`load_all`); and a custom audit table name is honored. The Postgres/MySQL versions use their own audit table name per test (`with_audit_log_table`) to avoid colliding with other tests running concurrently against the same shared live server, and are careful to `commit()` (closing the transaction `audit_log()` itself opens) before any cleanup `DROP TABLE` from a separate connection — otherwise the cleanup deadlocks waiting on a lock the still-open session transaction is holding.

`tests/optimistic_locking.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — the two conflict-detection tests, since the identity/no-op-without-`#[table(version)]` cases don't need re-proving per driver) cover `#[table(version)]`: updating with the current version succeeds and increments the stored version; updating with a stale version (superseded by someone else's edit) fails with `Error::Conflict` and leaves the other edit intact; updating a row someone else already deleted also conflicts; the same two cases for `delete`; and a type with no `#[table(version)]` field keeps its pre-existing behavior of a silent no-op on a stale/missing-row write — proving the feature is fully opt-in.

`tests/bulk_insert.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — the round-trip and rollback tests, since `BulkInsert::combine`'s own rendering/validation is pure query-builder logic that doesn't need a live server to re-prove per driver) cover `BulkInsert`/`Session::add_all`: combining several `Insert`s renders one statement with one `VALUES` group per row; combining zero inserts yields `None` rather than an invalid empty statement; combining `Insert`s from different tables is rejected; `add_all` queues exactly one pending write for the whole slice (not one per entity) and an empty slice is a no-op; a failing bulk insert (a duplicate primary key partway through) rolls back the entire batch, including rows that would've inserted fine on their own; and a `BulkInsert` works standalone through `engine.execute()`, without a `Session` at all.

`tests/soft_delete.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly exercise the feature end to end, since the query-builder-level `not_deleted_filter`/`load_active` coverage and the plain-type-is-unaffected case don't need re-proving per driver) cover `#[table(soft_delete)]`: `Session::delete` marks the row (`SET <column> = true`) instead of removing it; `Session::get` treats an already-marked row the same as one that was never there, including from a fresh `Session` with no identity-map cache from before the delete; `Mapped::not_deleted_filter()` and `Session::load_active` both exclude marked rows from query results; calling an entity's own `delete_query()` directly still issues a real, unmarked `DELETE`; and a type with no `#[table(soft_delete)]` field keeps `Session::delete`'s pre-existing behavior of a genuine hard delete, proving the feature is fully opt-in.

`tests/pool_stats.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests proving `pool_stats()` against a real network-backed pool, since the pure counting behavior doesn't need re-proving per driver) cover `Engine::pool_stats()`: checking out a connection is reflected immediately (`in_use` up, `total_acquires` up), and releasing it moves it back to `idle` without touching `total_acquires`; the counter keeps accumulating across repeated checkouts rather than just tracking the current one; and `waiters` goes to `1` while a second acquire is genuinely blocked behind a pool of size one, then back to `0` once it unblocks. Discovering the exact numbers to assert here surfaced two things worth knowing about sqlx's own pool: it eagerly opens (and keeps, idle) one connection up front to validate the URL when the pool is first constructed, so a "fresh" pool already reports that one as active/idle even though `total_acquires` correctly stays `0` (that startup connection never went through `Driver::connect`); and releasing a connection back to the pool isn't synchronous inside `drop` — the tests give it a brief moment (`tokio::time::sleep`) before reading the snapshot back, the same pattern already used elsewhere in this suite for other not-quite-synchronous async cleanup. `waiters`/`total_acquires` are the two numbers sqlx doesn't expose on its own, so each driver keeps a small `PoolMetrics` (a couple of atomics behind an `Arc`) alongside its pool for just those two; every other `PoolStats` field is a zero-cost read of the pool itself.

`tests/query_builder_extras.rs` (SQLite) and its `_postgres` counterpart (a reduced version there — just the two things that are actually Postgres-specific, `ILIKE` and `RETURNING` on `UPDATE`/`DELETE`; `DISTINCT`/`BETWEEN`/`Table::alias`/`Expr::text` have no dialect-specific behavior and don't need re-proving against a live server) cover the newer query-builder additions against a real SQL engine rather than just checking rendered SQL strings: `Select::distinct()` actually dedupes matching rows; `Column::between` includes both boundaries inclusively; `Column::ilike` matches case-insensitively via its portable `LIKE` fallback on SQLite and via Postgres's native `ILIKE` keyword; `.returning(...)` on `Update`/`Delete` actually returns the requested columns from a real Postgres server (and is silently ignored on SQLite, whose dialect doesn't support `RETURNING` at all); `Table::alias` supports a genuine self-join (an `employees` table joined to itself to pair each employee with their manager's name); and `Expr::text` composes a raw SQL fragment (with its own `?` placeholder) together with an ordinary builder-constructed filter via `.and(...)`. The pure rendering side of all of these — including that `RETURNING` is dialect-gated, `ilike_operator()` picks the right keyword per dialect, an alias renders `<table> AS <alias>` while an unaliased `Table` is unchanged, and `text()`'s `?` placeholders get rewritten to each dialect's real placeholder syntax in order — is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`, alongside the rest of the query builder's SQL generation.

`tests/bulk_update_delete.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two round-trip tests, since the identity-map-bypass/audit-log/rollback behavior is `Session`-level logic that doesn't depend on which driver is underneath) cover `Session::bulk_update`/`bulk_delete`: a filter-scoped update/delete changes/removes every matching row in one statement (`pending_len()` stays `1` regardless of how many rows match); an already-cached identity-mapped instance stays exactly as it was in memory after a `bulk_update` touches its row (the database itself is genuinely updated, confirmed by re-fetching through a fresh `Session`); `bulk_delete` is always a real, hard `DELETE` even against a `#[table(soft_delete)]` type, bypassing the soft-delete column entirely; both are recorded the same way ordinary `add`/`update`/`delete` writes are when audit logging is enabled; and a failing write queued in the same batch (a duplicate primary key) rolls the bulk write back too, since they share one all-or-nothing transaction.

`tests/savepoints.rs` (SQLite, 6 tests) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly prove `SAVEPOINT`/`ROLLBACK TO SAVEPOINT`/`RELEASE SAVEPOINT` actually work against a real server, since the rest is `Session`-level logic independent of the driver underneath) cover `Session::savepoint`/`rollback_to_savepoint`/`release_savepoint`: rolling back to a savepoint undoes a sub-unit of work (whether already flushed or still only queued) without aborting the rest of the transaction, which keeps committing normally afterward; releasing a savepoint keeps its effects and lets the transaction continue; savepoints nest, rolling back an inner one independently of an outer one still open around it; and both an unreleased savepoint and a full `Session::rollback()` behave correctly regardless of whether a savepoint is still open — standard SQL behavior this crate doesn't need to do anything special to get right, beyond generating unique, safe (unquoted) savepoint names so nested savepoints within one session never collide.

`tests/session_query.rs` (SQLite) covers `Session::query`/`SessionQuery`: `.filter`/`.order_by` narrow and order the result set; `.limit`/`.offset` page through it; `.first()` returns just the first matching row (`None` when nothing matches); results come back through the identity map exactly like `load_all`'s do, including reflecting an in-memory change made through a handle fetched a different way; and `.active_only()` excludes soft-deleted rows for a `#[table(soft_delete)]` type, the same as `load_active`. No `_postgres`/`_mysql` counterparts — this is `Session`-level logic built entirely on `load_all` and `Select`'s own filter/order/limit/offset, both already exercised per-driver elsewhere.

`tests/session_hooks.rs` (SQLite) covers `Session::on_before_flush`/`on_after_flush`/`on_before_commit`/`on_after_commit`/`on_after_rollback`: `on_before_flush` only fires when a flush actually has something queued to send, not for a no-op flush; `on_after_flush` fires after a successful flush and in the right order relative to `on_before_flush`, but never fires at all for a flush that failed; `on_before_commit` fires on every `commit()` call regardless of whether anything was pending; `on_after_commit`/`on_after_rollback` only fire when a transaction actually existed to commit/roll back (not when `commit()`/`rollback()` was a no-op because nothing was ever flushed or read); and multiple hooks registered on the same event run in registration order. No `_postgres`/`_mysql` counterparts — hooks are plain `Session`-level callbacks with no interaction with which driver is underneath.

## Running tests

```
cargo test --workspace --all-features
```
