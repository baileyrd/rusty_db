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

`PoolConfig` also carries connection-level event hooks — raw SQL run at three points in a connection's lifecycle, for behavioral setup `pool_stats()`'s pure observability doesn't cover:

```rust
let config = PoolConfig::new(10)
    .with_on_connect("SET application_name = 'my_app'")   // once per new physical connection
    .with_before_acquire("SET myapp.request_id = ''")      // every time a connection is checked out
    .with_after_release("DISCARD ALL");                    // every time a connection is checked back in
let engine = PostgresDriver::engine_with(&url, config).await?;
```

`.with_on_connect(...)` runs once on every newly-opened physical connection, before it's ever handed to a caller — the natural place for per-connection session setup that would otherwise need repeating on every checkout. `.with_before_acquire(...)` runs every time an idle connection is about to be handed out; `.with_after_release(...)` runs every time a connection is checked back in, about to go idle again — e.g. to reset session-local state a caller may have changed before the connection is reused by someone else. All three are plain SQL strings (not Rust closures), executed directly against the connection via the same driver-agnostic mechanism regardless of backend — under the hood, each maps onto sqlx's own `after_connect`/`before_acquire`/`after_release` pool hooks, which this crate didn't use at all before.

`.with_statement_cache_capacity(n)` tunes a `baked query`-equivalent that already exists underneath every connection but wasn't exposed here before: each connection caches up to `n` distinct prepared statements (LRU-evicted past that), so a query shape's rendered SQL only needs parsing/planning once per connection rather than on every execution, as long as it stays in cache:

```rust
let config = PoolConfig::new(10).with_statement_cache_capacity(500);
let engine = PostgresDriver::engine_with(&url, config).await?;
```

The underlying driver already caches up to 100 statements per connection with no configuration at all; raise this for a workload with many distinct, repeatedly-run query shapes (past 100, otherwise-cached statements would keep getting evicted and re-prepared), or lower it to bound per-connection memory when only a handful of shapes ever repeat. `Connection` also exposes `cached_statement_count()` — how many statements are cached right now, on that specific physical connection (not a pool-wide figure, unlike `pool_stats()`, since each connection keeps its own cache) — for confirming the setting actually took effect.

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

### Sharding

`ShardRouter` picks which of several `Engine`s a query should run against, based on hashing a caller-supplied key — the common "split rows across N databases by a tenant/customer id" topology:

```rust
let shard_0 = SqliteDriver::engine("...").await?; // or PostgresDriver/MySqlDriver
let shard_1 = SqliteDriver::engine("...").await?;
let shard_2 = SqliteDriver::engine("...").await?;

let shards = ShardRouter::new(vec![shard_0, shard_1, shard_2])?;

let customer_id = 42_i64;
shards.execute(customer_id, &some_insert).await?;                    // routed by hash(customer_id)
let rows = shards.fetch_all(customer_id, &Select::from(&orders)).await?; // same routing, same shard
let mut session = shards.session(customer_id);                       // a Session local to that one shard
```

`fetch_all`/`fetch_one`/`fetch_optional`/their `_as` counterparts, `execute`, `connect`, and `session` all mirror `Engine`'s own methods, just with a shard key as the first argument. `shard(index)`/`shards()` give direct index-based access to each underlying `Engine`, for fan-out maintenance (running the same `CreateTable`/`Migrator::up` against every shard) rather than per-key routing.

This is a router, not a distributed query planner: there's no cross-shard `JOIN`, no cross-shard aggregation, and no cross-shard transaction — every operation always talks to exactly one shard, chosen up front from the key you pass, and a `Session` obtained through `shards.session(key)` is a perfectly ordinary single-shard `Session` with no idea any other shard exists. `ShardRouter::new` and `ShardRouter::new_consistent` are the only two ways to build one — deliberately no `add_shard`, unlike `ReplicaSet::add_replica`, since neither routing strategy below actually moves a row's data for you when the shard count changes, only decides where a *new* `ShardRouter` would now look for it; adding a shard is a materially more dangerous operation than adding a read replica and shouldn't look equally casual.

`ShardRouter::new` routes with naive modulo hashing (`hash(key) % shard_count()`) — simple, but changing the shard count remaps nearly every key to a different shard (modulo a different number scrambles almost everything), so it only suits a shard count that's fixed for good. `ShardRouter::new_consistent(shards, virtual_nodes)` routes via a consistent-hash ring instead — `virtual_nodes` positions per shard (a few hundred is reasonable) — so that building a *new* router with one more shard appended (same existing shards, same `virtual_nodes`) only remaps the minority of keys that happen to land near the new shard's ring positions, leaving the rest routed exactly where they already were:

```rust
let shards = ShardRouter::new_consistent(vec![shard_0, shard_1, shard_2], 200)?;
// later, growing to four shards remaps only ~1/4 of keys, not nearly all of them:
let grown = ShardRouter::new_consistent(vec![shard_0, shard_1, shard_2, shard_3], 200)?;
```

Consistent hashing bounds how much data an actual resharding migration would need to move — it doesn't perform that migration itself, and neither strategy supports mutating an existing `ShardRouter`'s shard list in place.

### Query timeouts and cancellation

`with_timeout` wraps any `Engine`/`Transaction`/`Session`/`ReplicaSet` call (anything returning a `Result<T>`) with a client-side timeout: if it hasn't finished within the given duration, it's cancelled and `Error::Timeout` comes back instead:

```rust
use std::time::Duration;

let rows = with_timeout(Duration::from_secs(5), engine.fetch_all(&Select::from(&users))).await?;
```

"Cancelled" here means exactly what it means for any Rust future: the operation is dropped without being polled again — the same thing that happens if you `tokio::spawn` a query and call `JoinHandle::abort()` on it instead. Whatever connection it was using is returned to (or, if it was mid-query, discarded from) the pool by the driver's own `Drop` handling, so a cancelled operation never leaves the pool stuck — the next call just gets a fresh connection if the old one couldn't be reused. Note that the database server itself may not learn the query was abandoned until the connection actually closes; this only stops the client from waiting on it.

### Streaming query results

`Engine::fetch_all`/`fetch_all_as` collect the entire result set into a `Vec` before returning it — fine for ordinary queries, a real memory ceiling for a large export or report. `Engine::fetch_stream`/`fetch_stream_as` yield rows one at a time instead, as a `Stream`:

```rust
let orders = Table::new("orders");
let mut stream = engine.fetch_stream(&Select::from(&orders)).await?;
while let Some(row) = stream.next().await {
    let row = row?;
    // handle one row at a time, without ever holding the whole result set in memory
}
```

`fetch_stream_as::<T>` is the streaming counterpart of `fetch_all_as`, decoding each row into a `#[derive(Mapped)]` type as it arrives. The returned stream owns the connection it checked out for as long as it's alive — genuinely fetching row-by-row from the database rather than collecting everything up front and only then handing back a `Stream`-shaped wrapper — so dropping it early (e.g. stopping after the first few rows) releases the connection back to the pool right away instead of waiting for the rest of a result set nobody's going to read. `BoxStream`/`StreamExt` (for `.next()` and the rest of the `futures` combinators) are both re-exported from the prelude, so consuming a stream needs no extra dependency.

### DDL builder

`CreateTable`/`DropTable`/`CreateIndex`/`DropIndex` are the query builder's counterpart to `Select`/`Insert`/`Update`/`Delete`, but for schema definition instead of data access — a portable alternative to hand-writing `CREATE TABLE`/`DROP INDEX` text per dialect, the same way the rest of the query builder is a portable alternative to hand-written `SELECT`/`INSERT` text:

```rust
let create = CreateTable::new("users")
    .if_not_exists()
    .column("id", ColumnType::I64).primary_key().autoincrement()
    .column("email", ColumnType::VarChar(255)).not_null().unique()
    .column("bio", ColumnType::Text)
    .column("created_at", ColumnType::TimestampTz).default_raw("CURRENT_TIMESTAMP")
    .foreign_key(["tenant_id"], "tenants", ["id"])
    .check("email <> ''");
engine.execute(&create).await?;

engine.execute(&CreateIndex::new("idx_users_email", "users", ["email"]).unique()).await?;
engine.execute(&DropIndex::new("idx_users_email", "users")).await?;
engine.execute(&DropTable::new("users").if_exists()).await?;
```

Column types are given as a portable `ColumnType` (`Bool`, `I64`, `F64`, `Text`, `VarChar(n)`, `Bytes`, `Uuid`, `Decimal { precision, scale }`, `Json`, `Date`, `Time`, `DateTime` (naive), `TimestampTz` (a UTC instant)) — deliberately mirroring `Value`'s own variants — translated to each dialect's own `CREATE TABLE` spelling by a new `Dialect::column_type_sql` method, the same native-vs-fallback split already documented on `Value` itself (e.g. `Uuid` renders as Postgres's native `UUID`, but `TEXT` on SQLite and `CHAR(36)` on MySQL/MariaDB, which have no UUID type of their own).

`.column(name, ty)` adds a column (nullable by default); `.not_null()`/`.primary_key()`/`.unique()`/`.autoincrement()`/`.default_raw(...)` each modify the column *most recently added*, so chain them right after — calling one before any `.column(...)` panics. `.primary_key()` on exactly one column, combined with `.autoincrement()`, renders inline as each dialect's own auto-incrementing syntax (SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT`, Postgres's `BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY`, MySQL's `BIGINT AUTO_INCREMENT PRIMARY KEY` — `to_sql` panics if `.autoincrement()` is used on anything but a `ColumnType::I64` column); any other combination of `.primary_key()` columns — including more than one, for a composite key — renders as a separate table-level `PRIMARY KEY (...)` constraint instead. `.default_raw(...)`/`.check(...)` both take a raw SQL fragment, embedded verbatim — the same unvalidated-raw-text convention `#[table(default = "...")]`/`Insert::raw_value` already use — and are unrelated to that mapping-level feature: a `CreateTable` column `DEFAULT` is a real database-side default applied whenever an `INSERT` omits the column entirely, not a Rust-struct-level substitution.

`DropIndex` takes the table name up front (`DropIndex::new(name, table)`) even though Postgres/SQLite's own `DROP INDEX` doesn't need it — MySQL/MariaDB's does (an index name there is only unique within its table), handled via a new `Dialect::drop_index_needs_table_name` method so the same builder call works on all three.

`AlterTable::add_column`/`drop_column` cover altering an existing table's columns — each `AlterTable` is always exactly *one* operation, never several combined into one statement, since SQLite (unlike Postgres/MySQL) only ever allows one action per `ALTER TABLE`:

```rust
engine.execute(&AlterTable::add_column("users", "credits", ColumnType::I64).not_null().default_raw("0")).await?;
engine.execute(&AlterTable::drop_column("users", "credits")).await?;
```

**A real caveat surfaced while building this, not a hypothetical one:** on SQLite, a connection that already had a table's *pre-`ALTER`* shape "in view" can panic — inside the underlying `sqlx-sqlite` driver itself, not this crate's code — if it's used to query that table again right after altering it. This is a long-standing upstream limitation in how SQLite's own statement caching interacts with schema changes ([`launchbadge/sqlx#296`](https://github.com/launchbadge/sqlx/issues/296)), not something introduced by `AlterTable`; it would affect a hand-written `ALTER TABLE` run through this crate's raw `execute()` just as much. Postgres has a related but far gentler version of the same thing: a `SELECT *`-shaped statement already prepared against the old shape can fail with a clean, catchable `"cached plan must not change result type"` error if reused on the same connection afterward (a normal, well-documented consequence of Postgres's own server-side prepared statements). MySQL/MariaDB has neither issue. The safe pattern, on any dialect: get a fresh `Engine` (a new connection pool) before reading from a table you just ran `AlterTable` against, rather than continuing to use the one that issued the `ALTER TABLE` itself — `tests/ddl.rs`/`ddl_postgres.rs` both do exactly this.

What the DDL builder overall still doesn't cover: renaming a table, or altering an existing column's type/constraints — `AlterTable` only adds, drops, or renames a column, never a whole table.

### Autogenerate: diffing a mapped type's shape against a live database

`Engine::autogenerate_migration` diffs a set of `#[derive(Mapped)]` types' expected shape against a live database's reflected schema, generating the DDL statements (as SQL text, for review) needed to reconcile them — an Alembic-style starting point, built on the schema reflection and DDL builder above:

```rust
#[derive(Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
    nickname: Option<String>,
}

let statements = engine
    .autogenerate_migration(&[TableSpec::of::<User>()], &AutogenerateOptions::default())
    .await?;
for statement in &statements {
    println!("{statement}"); // review before running anything
}
```

`#[derive(Mapped)]` now also captures each field's portable `ColumnType` and nullability (a new `Mapped::COLUMN_SPECS` const, inferred from the field's own Rust type the same way `automap` infers a Rust type from a reflected column in the opposite direction — falling back to `ColumnType::Text` for anything unrecognized, e.g. a `MappedEnum`/`MappedNewtype` custom type or a `Vec<T>` array of anything but `u8`, since no portable array column type exists yet). `TableSpec::of::<T>()` bundles that with `T::TABLE_NAME`/`PRIMARY_KEY` into the "expected" side of a diff; `autogenerate_migration` reflects only the specific tables named (never the whole database) and compares.

Deliberately conservative, in the same "starting point, not a replacement" spirit as `automap` itself — and, for the two things below that genuinely can't be inferred from the expected/live shapes alone, the fix is an explicit, opt-in `AutogenerateOptions` you fill in by hand rather than a heuristic guessing at intent:
- **Never proposes dropping a whole table, unless explicitly allow-listed.** A live table this diff doesn't recognize might not be tracked by the `expected` list at all (a different part of the same app, this crate's own migration/audit-log bookkeeping tables, a schema shared with something else) — there's no way to tell "not currently mapped" from "meant to be deleted." `AutogenerateOptions::allow_drop_tables` is the missing say-so: name a table there and, if it's live but absent from `expected`, a `DropTable` is proposed for it; leave it off (the default) and an unrecognized live table is left completely alone.
- **Never detects a rename, unless explicitly hinted.** Renaming a field is otherwise reported as one unrelated `AlterTable::drop_column` plus one unrelated `AlterTable::add_column` — running both, in that order, loses the column's data rather than preserving it, the same rename-blindness Alembic's own autogenerate is well known for. `AutogenerateOptions::renamed_columns` lets you say "this table's column was renamed from X to Y" up front, producing a single data-preserving `AlterTable::rename_column` instead — but only when the hint actually matches what's live (the old name still present, the new name expected but not yet live); a stale or irrelevant hint is silently ignored rather than forced onto a shape it no longer describes.
- **Still never diffs column type.** A live column's `type_name` is dialect-native, verbatim text with no portable representation to compare `ColumnType` against without reimplementing `automap::rust_type_for`'s heuristic in reverse, per dialect — changing a field's type without renaming it produces no suggested statement at all; review type-level changes by hand.

```rust
let options = AutogenerateOptions {
    renamed_columns: vec![("users".to_string(), "nickname".to_string(), "display_name".to_string())],
    allow_drop_tables: vec!["legacy_sessions".to_string()],
};
let statements = engine
    .autogenerate_migration(&[TableSpec::of::<User>()], &options)
    .await?;
```

`AlterTable` also gained a third operation for this, `rename_column`, spelled identically (`ALTER TABLE t RENAME COLUMN old TO new`) on all three dialects — SQLite has supported it since 3.25.0, MySQL/MariaDB since 8.0/10.5.2, both well below anything else this crate assumes.

Generated statements are never executed automatically — review them (or hand them to your own `Migration`) before running anything, the same "generate for review" philosophy `automap` already established for struct generation.

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

### Automap: generating `#[derive(Mapped)]` structs from a live database

`Engine::automap_table`/`automap_all` build on schema reflection to generate `#[derive(Mapped)]` struct source for an existing database's tables — a starting point for mapping a database you didn't design in Rust, not a fully automatic replacement for writing the struct by hand:

```rust
let source = engine.automap_table("users").await?;
println!("{source}");
// #[derive(Mapped, Debug, Clone)]
// #[table(name = "users")]
// struct Users {
//     #[table(primary_key)]
//     #[table(column = "id")]
//     id: i64,
//     #[table(column = "name")]
//     name: String,
//     #[table(column = "nickname")]
//     nickname: Option<String>,
// }

let everything = engine.automap_all().await?; // same, for every table via list_tables()
```

Paste the generated text into a `.rs` file (with `use rusty_db::prelude::*;` in scope) and it compiles as-is through the real derive macro. What it does: maps each column's dialect-specific `type_name` (Postgres's `"timestamp without time zone"`, MySQL's `"int(11) unsigned"`, SQLite's `"INTEGER"`, ...) to a Rust type via a best-effort, case-insensitive heuristic — correct for the common, standard column types, falling back to `String` for anything it doesn't recognize; wraps a nullable, non-primary-key column in `Option<T>`; and escapes a column name that isn't already a valid Rust identifier (a leading digit, an invalid character, or a Rust keyword like `type` becoming `r#type`) rather than assuming the common case always holds. What it doesn't do: infer `#[table(version)]`/`#[table(soft_delete)]` (no schema convention to detect either from), or generate `has_many`/`belongs_to`/`many_to_many` relationship fields — detected foreign keys are only listed as a trailing comment, for wiring up by hand.

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

### Aggregate functions and expression columns

`Select` previously only took plain `Column`s. `SelectExpr` widens that: it wraps any `Expr` — an aggregate (`Column::count`/`sum`/`avg`/`min`/`max`, or `Expr::count_all()` for `COUNT(*)`), an `Expr::text(...)` fragment, or anything else the builder can express — with an optional `.alias(...)` for `AS`:

```rust
let orders = Table::new("orders");

let row = engine
    .fetch_one(&Select::from(&orders).columns([
        SelectExpr::from(Expr::count_all()).alias("count"),
        SelectExpr::from(orders.col("amount").sum()).alias("total"),
        SelectExpr::from(orders.col("amount").avg()).alias("average"),
    ]))
    .await?;
let total: i64 = row.get_by_name("total")?;
```

`Column`/`Expr` both convert into `SelectExpr` via `Into`, so plain columns still work in `.columns(...)` unchanged — but only one concrete item type per call, since `IntoIterator` needs one. Mixing plain and expression columns in the same `SELECT` means wrapping the plain ones in `SelectExpr::from(...)` too:

```rust
let query = Select::from(&orders).columns([
    SelectExpr::from(orders.col("customer")),
    SelectExpr::from(orders.col("amount").sum()).alias("total"),
]);
```

`COUNT`/`SUM`/`AVG`/`MIN`/`MAX` are ANSI-standard and render identically on every backend — no dialect-specific handling needed, unlike `ILIKE` above.

### GROUP BY and HAVING

`Select::group_by(...)` accepts the same `Column`/`Expr` mix `.columns(...)` and `SelectExpr` do; `.having(...)` is a second filter applied after grouping collapses rows, so — unlike `.filter(...)`'s `WHERE`, which runs before grouping and can't see an aggregate — it can reference one:

```rust
let orders = Table::new("orders");

let rows = engine
    .fetch_all(
        &Select::from(&orders)
            .columns([
                SelectExpr::from(orders.col("customer")),
                SelectExpr::from(orders.col("amount").sum()).alias("total"),
            ])
            .group_by([orders.col("customer")])
            .having(orders.col("amount").sum().gt(100_i64)),
    )
    .await?;
```

`.having(orders.col("amount").sum().gt(100_i64))` works because comparison methods (`.eq`/`.lt`/`.gt`/etc.) now exist on `Expr` itself, not just `Column` — the same methods `Column`'s own build the same way, just reachable on any expression (an aggregate, an `Expr::text(...)` fragment, or anything else), which is exactly what `HAVING` needs. Calling `.having(...)` more than once combines every condition with `AND`, the same as repeated `.filter(...)`.

### Set operations (UNION, INTERSECT, EXCEPT)

`Select::union`/`union_all`/`intersect`/`except` combine two `Select`s' result sets, returning a `SetOperation` that's chainable the same way to combine more than two:

```rust
let active = Table::new("active_users");
let archived = Table::new("archived_users");

let query = Select::from(&active)
    .columns([active.col("email")])
    .union(Select::from(&archived).columns([archived.col("email")]));
let rows = engine.fetch_all(&query).await?;
```

`.union` deduplicates matching rows across both arms; `.union_all` keeps duplicates; `.intersect` keeps only rows present in both; `.except` keeps rows from the first arm that aren't also in the second. All four are ANSI-standard and work the same way on every backend (`INTERSECT`/`EXCEPT` need MySQL 8.0.31+/MariaDB 10.3+ — an older server surfaces a plain SQL syntax error, since there's no reasonable fallback rendering that would still mean the same thing). Not modeled: giving the combined result its own `ORDER BY`/`LIMIT` distinct from an individual arm's — only each arm's own `Select` methods are available for that.

### SQL functions, arithmetic, `CASE`, and `COALESCE`

`Column`/`Expr` get `.lower()`/`.upper()` (`LOWER`/`UPPER`), `.concat(other)` (string concatenation), `.add`/`.sub`/`.mul`/`.div(other)` (`+`/`-`/`*`/`/`), and `Expr::now()` (`CURRENT_TIMESTAMP`); `Expr::coalesce([...])` builds `COALESCE(...)`; and `Case` builds a full `CASE WHEN ... THEN ... [ELSE ...] END`:

```rust
let orders = Table::new("orders");

let tier = Case::new()
    .when(orders.col("amount").gt(100_i64), Expr::lit("gold"))
    .when(orders.col("amount").gt(50_i64), Expr::lit("silver"))
    .otherwise(Expr::lit("bronze"));

let query = Select::from(&orders).columns([
    SelectExpr::from(orders.col("customer").upper()).alias("customer"),
    SelectExpr::from(tier).alias("tier"),
]);
```

`Column`'s comparison methods (`.eq`/`.lt`/`.gt`/etc.) take a literal value, which is no help for filtering against `Expr::now()` (not a literal) or another aggregate — `Expr` itself has `.eq_expr`/`.lt_expr`/`.gt_expr`/etc. for exactly that, comparing two expressions against each other: `Expr::col(orders.col("created_at")).lt_expr(Expr::now())` renders `"orders"."created_at" < CURRENT_TIMESTAMP`.

Everything here is ANSI-standard and renders identically on every backend, with one exception: MySQL/MariaDB's `||` operator means logical `OR` under the default `sql_mode`, not string concatenation (confirmed empirically: `SELECT 'foo' || 'bar'` returns `0` there, not `'foobar'`) — `.concat(...)` renders `a || b` on Postgres/SQLite but `CONCAT(a, b)` on MySQL/MariaDB to actually mean the same thing everywhere. Deliberately out of scope: date arithmetic (`date_col + INTERVAL '1 day'` vs. `DATE_ADD(...)` vs. SQLite's `date(...)` function — genuinely different syntax on all three backends, enough of its own project to warrant a dedicated pass rather than folding it in here); use `Expr::text(...)` as the escape hatch for it in the meantime.

### Subqueries

A `Select` can nest inside another query's filter or column list three ways: `.in_subquery(...)` for `IN (SELECT ...)`, `Expr::exists(...)` for `EXISTS (SELECT ...)`, and `Expr::subquery(...)` for a scalar subquery usable as an ordinary column or comparison operand:

```rust
let orders = Table::new("orders");
let users = Table::new("users");

// IN (subquery): users who placed a big order.
let big_orders = Select::from(&orders)
    .columns([orders.col("user_id")])
    .filter(orders.col("amount").gt(1000_i64));
let query = Select::from(&users).filter(users.col("id").in_subquery(big_orders));

// Correlated EXISTS: users who have placed any order at all.
let has_orders = Select::from(&orders).filter(orders.col("user_id").eq_col(&users.col("id")));
let query = Select::from(&users).filter(Expr::exists(has_orders));

// Scalar subquery: each user alongside their total spend.
let order_total = Select::from(&orders)
    .columns([SelectExpr::from(orders.col("amount").sum())])
    .filter(orders.col("user_id").eq_col(&users.col("id")));
let query = Select::from(&users).columns([
    SelectExpr::from(users.col("name")),
    SelectExpr::from(Expr::subquery(order_total)).alias("total_spend"),
]);
```

A subquery is correlated simply by referencing the outer table's columns in its own `.filter(...)` (`orders.col("user_id").eq_col(&users.col("id"))` above) — no special API for it, since `Column` already qualifies itself by table name regardless of which `Select` it's built into, so both tables just show up in the rendered SQL the way a hand-written correlated subquery would. `NOT IN`/`NOT EXISTS` aren't separate variants; wrap either with `.not()` (`Expr::exists(has_orders).not()`). Bind parameters from the outer query and the nested one share a single, correctly-ordered parameter list, so Postgres's `$1, $2, ...` numbering stays correct across both. Not modeled: a subquery in a `FROM` clause (a derived table) — only nesting inside a filter or column list is supported.

### Common table expressions (`WITH`, `WITH RECURSIVE`)

`Cte::new(name, query)` names a `Select` as a CTE; `Select::with(...)` prefixes an outer query with one or more of them, referenceable in that outer query's own `FROM`/`JOIN`/subqueries by name (an ordinary `Table::new(name)` reaches it):

```rust
let orders = Table::new("orders");
let big_orders = Cte::new(
    "big_orders",
    Select::from(&orders)
        .columns([orders.col("id"), orders.col("customer")])
        .filter(orders.col("amount").gt(1000_i64)),
);

let cte_ref = Table::new("big_orders");
let query = Select::from(&cte_ref).with([big_orders]);
```

A recursive CTE combines an anchor `Select` with a recursive term via `Cte::recursive_union_all`/`recursive_union` (the latter deduplicating like plain `UNION` does), attached with `Select::with_recursive(...)` instead of `.with(...)` (`WITH RECURSIVE` is a different keyword). The recursive term recurses by referencing the CTE's own name in its `FROM`/`JOIN` — again just an ordinary `Table::new(name)`, no special self-reference API:

```rust
let employees = Table::new("employees");
let org_chart = Table::new("org_chart");

let anchor = Select::from(&employees)
    .columns([
        SelectExpr::from(employees.col("id")),
        SelectExpr::from(employees.col("name")),
        SelectExpr::from(Expr::lit(1_i64)).alias("depth"),
    ])
    .filter(employees.col("manager_id").is_null());
let recursive_term = Select::from(&employees)
    .columns([
        SelectExpr::from(employees.col("id")),
        SelectExpr::from(employees.col("name")),
        SelectExpr::from(org_chart.col("depth").add(Expr::lit(1_i64))).alias("depth"),
    ])
    .join(&org_chart, employees.col("manager_id").eq_col(&org_chart.col("id")));
let cte = Cte::recursive_union_all("org_chart", anchor, recursive_term);

let query = Select::from(&org_chart).with_recursive([cte]);
```

`WITH`/`WITH RECURSIVE` are both ANSI-standard and render identically on every backend this crate supports. As with subqueries, the CTE's own bind parameters and the outer query's share one correctly-ordered parameter list. Not modeled: a `WITH` clause attached to a `SetOperation` rather than a single `Select` (though a CTE-carrying `Select` can still be one arm of a `UNION`/etc., since `WITH`'s scope naturally extends over the whole statement that follows it — nothing special needed there), and multiple independent recursive CTEs referencing each other in the same `WITH` clause.

### Window functions

`Expr::row_number()`/`.rank()`/`.dense_rank()` are ranking functions, and any aggregate (`.sum()`/`.count()`/etc.) can also run as a window function — either way, call `.over(...)` with a `Window` to turn it into one: `function OVER (PARTITION BY ... ORDER BY ...)`:

```rust
let orders = Table::new("orders");

// Running total per customer, in id order.
let running_total = orders.col("amount").sum().over(
    Window::new()
        .partition_by([orders.col("customer")])
        .order_by(orders.col("id").asc()),
);

// This customer's orders, each numbered by recency.
let row_num = Expr::row_number().over(
    Window::new()
        .partition_by([orders.col("customer")])
        .order_by(orders.col("id").asc()),
);

let query = Select::from(&orders).columns([
    SelectExpr::from(orders.col("id")),
    SelectExpr::from(running_total).alias("running_total"),
    SelectExpr::from(row_num).alias("order_number"),
]);
```

Both `Window` clauses are optional and independent: `.partition_by(...)` alone splits rows into groups the window function is computed independently within, without ordering them; `.order_by(...)` alone treats the whole result as one ordered partition; `Window::new()` on its own (no clauses at all) means one unordered partition covering every row, still valid SQL as `OVER ()`. `RANK`/`DENSE_RANK` differ only in how they handle ties in the window's `ORDER BY`: `RANK` skips ahead by the tie's size afterward (`1, 2, 2, 4, ...`), `DENSE_RANK` never skips (`1, 2, 2, 3, ...`).

Window functions are ANSI-standard and render identically on every backend this crate supports, but need a modern-enough server: SQLite 3.25+ (2018), MySQL 8.0+/MariaDB 10.2+, and any currently supported Postgres version — an older server surfaces a plain SQL syntax error, since there's no reasonable fallback rendering that would still mean the same thing.

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

Field types are limited to whatever `Value` already converts from (`bool`, `i64`, `i32`, `f64`, `String`, `Vec<u8>`, `Uuid`, `BigDecimal`, `Json`, `NaiveDate`, `NaiveTime`, `NaiveDateTime`, `DateTime<Utc>`, `Vec<T>` for a handful of element types (see "Array columns" below), and `Option<_>` of any of those) — plus any type deriving `MappedEnum`/`MappedNewtype`, or implementing `Into<Value>`/`FromValue` by hand (see "Custom types" below). A field additionally marked `#[table(version)]` (requires `#[table(primary_key)]` too) turns on optimistic locking — see below.

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

### Temporal values

Four more dedicated variants — `Value::Date`/`Value::Time`/`Value::DateTime`/`Value::Timestamp` — back `chrono`'s `NaiveDate`/`NaiveTime`/`NaiveDateTime`/`DateTime<Utc>` (all four re-exported, same as `Uuid`/`BigDecimal`/`Json` above):

```rust
#[derive(Mapped)]
#[table(name = "events")]
struct Event {
    #[table(primary_key)]
    id: i64,
    day: NaiveDate,
    logged_at: NaiveDateTime,
    happened_at: DateTime<Utc>,
}

let event = Event {
    id: 1,
    day: "2024-01-15".parse().unwrap(),
    logged_at: "2024-01-15T10:30:00".parse().unwrap(),
    happened_at: "2024-01-15T10:30:00Z".parse().unwrap(),
};
engine.execute(&event.insert()).await?;
```

`NaiveDateTime` (`Value::DateTime`) is the timezone-naive "wall clock" reading SQLAlchemy's own `DateTime` has — no attached offset at all. `DateTime<Utc>` (`Value::Timestamp`) is a genuine UTC-normalized instant. Postgres and MySQL/MariaDB both have native column types for all four and round-trip each variant directly over their own binary wire formats — `DATE`/`TIME`/`TIMESTAMP` (Postgres) or `DATE`/`TIME`/`DATETIME` (MySQL/MariaDB) decode as `Value::Date`/`Value::Time`/`Value::DateTime`; Postgres's `TIMESTAMPTZ` and MySQL/MariaDB's `TIMESTAMP` (which, unlike its plain `DATETIME`, MySQL itself always stores and reports as UTC) both decode as `Value::Timestamp` instead — same split, by name rather than by wire format, since `DATETIME` and `TIMESTAMP` are actually the same packed bytes on MySQL's wire protocol. SQLite has no native temporal type of its own at all, so all four flatten to `Value::Text` there (each one's own ISO 8601/RFC 3339 form) — `FromValue` for each of the four chrono types parses that text form back, so a mapped struct's temporal fields round-trip correctly on every backend, just without a native wire format on SQLite specifically. `FromValue for DateTime<Utc>` also accepts a `Value::DateTime` (treating it as already being in UTC, the same assumption MySQL's own `TIMESTAMP` makes), and `FromValue for NaiveDateTime` accepts a `Value::Timestamp` right back (dropping its always-UTC offset) — so either Rust type still decodes correctly even against the "wrong" one of the two column types.

### Enum columns

`#[derive(MappedEnum)]` maps a fieldless (unit-variant-only) Rust enum onto a single column, so it can be used directly as a `#[derive(Mapped)]` field type — no built-in `Value` variant for this one, since an enum is your own type, not something this crate defines:

```rust
#[derive(Debug, Clone, Copy, PartialEq, MappedEnum)]
enum Status {
    Active,
    Inactive,
    #[mapped_enum(rename = "banned_user")]
    Banned,
}

#[derive(Mapped)]
#[table(name = "accounts")]
struct Account {
    #[table(primary_key)]
    id: i64,
    status: Status,
}

let account = Account { id: 1, status: Status::Banned };
engine.execute(&account.insert()).await?; // stores status as the text "banned_user"
```

By default each variant stores as `Value::Text` holding its own snake_case name (`Active` → `"active"`) — override one with `#[mapped_enum(rename = "...")]` on that specific variant, as `Banned` does above. `#[mapped_enum(as_int)]` on the enum itself switches the whole thing to `Value::I64` instead, storing each variant's own discriminant (`v as i64`, so explicit `= N` values on individual variants are honored) rather than its name; unlike the text form, this doesn't survive the enum's variants being reordered or renumbered later; whichever mode is picked, decoding an unrecognized stored value (an unmapped string, or an integer with no matching variant) is an error rather than a silent fallback. This works identically on every backend, since it's an ordinary `TEXT`/`INTEGER` column underneath, not a database-native enum type (Postgres's own `CREATE TYPE ... AS ENUM` isn't required, or supported specially, here).

### Array columns

`Value::Array` backs `Vec<T>` for a handful of element types — `bool`, `i64`, `f64`, `String`, `Uuid`, `BigDecimal`, the four temporal types above, and `Value` itself as a fully-generic escape hatch:

```rust
#[derive(Mapped)]
#[table(name = "playlists")]
struct Playlist {
    #[table(primary_key)]
    id: i64,
    track_ids: Vec<i64>,
    tags: Vec<String>,
    featured_ids: Option<Vec<i64>>,
}

let playlist = Playlist {
    id: 1,
    track_ids: vec![10, 20, 30],
    tags: vec!["rock".into(), "live".into()],
    featured_ids: None,
};
engine.execute(&playlist.insert()).await?;
```

Postgres has a native array type for virtually every scalar column type (`INTEGER[]`/`TEXT[]`/`UUID[]`/...) and round-trips this variant directly over its binary wire format. MySQL/MariaDB and SQLite have no array column type at all, so on those two an array field is stored as a JSON array instead (both already support JSON, natively or as text) — `FromValue for Vec<T>` parses that JSON-array form back too, so a mapped struct's `Vec<T>` field round-trips correctly on every backend, just without Postgres's native wire format (or a native array type at all) on the other two.

One sharp edge on Postgres specifically: binding an array picks its native Postgres array type by inspecting the first non-null element, since `Value::Array` itself carries no element-type tag once constructed (unlike a mapped struct field's own static Rust type, which the query-building layer doesn't see by the time it's binding parameters). An empty array, or one whose every element is `Value::Null`, has no element to inspect — that case binds as `TEXT[]`, which needs an explicit cast (or, more simply, a `TEXT[]` target column) if that's not what the column actually expects. A Postgres array containing a genuine `NULL` element decodes fine into the `Vec<Value>` escape hatch (each element independently, `Value::Null` included) but not into a concrete `Vec<i64>` field, which has nowhere to put a `NULL` — that's a decode error rather than silently dropping or defaulting it.

### Custom types

A `#[derive(Mapped)]` field's type is never actually limited to the list above — that list is just what `Value` converts from out of the box. Any type implementing `Into<Value>` (on an owned value) and `FromValue` works as a field type, whether or not this crate has ever heard of it, because both are ordinary public traits:

```rust
struct Email(String);

impl From<Email> for Value {
    fn from(v: Email) -> Value {
        Value::Text(v.0)
    }
}

impl FromValue for Email {
    fn from_value(value: &Value) -> Result<Self, String> {
        String::from_value(value).map(Email)
    }
}
```

`#[derive(MappedNewtype)]` generates exactly that pair of impls for the common case — a single-field tuple struct that should just delegate straight through to its own field's conversion:

```rust
#[derive(Debug, Clone, PartialEq, MappedNewtype)]
struct Email(String);

#[derive(Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    email: Email,
}
```

It composes with `MappedEnum` too — `#[derive(MappedNewtype)] struct AccountTier(Tier)` works as long as `Tier` itself is `Value`-compatible (a `MappedEnum`, another `MappedNewtype`, or anything else satisfying the same two traits). Reach past `MappedNewtype` for the hand-written form above once there's more to the conversion than pure delegation — validating a raw value on the way in, rejecting a malformed one on the way out, or combining more than one column into a single field — none of which needs a macro or any special support from this crate.

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

### Column defaults

A `#[table(default = "...")]` field takes a raw SQL fragment — a literal (`"0"`, `"'pending'"`) or an expression (`"CURRENT_TIMESTAMP"`) — substituted into the generated `INSERT` whenever this struct's field currently equals `Default::default()` for its type, so `Session::add`/`insert()` can leave a field at its type's default and still get a real value in the row instead of that default itself:

```rust
#[derive(Mapped)]
#[table(name = "tasks")]
struct Task {
    #[table(primary_key)]
    id: i64,
    #[table(default = "'pending'")]
    status: String,
    label: String,
}

session.add(&Task { id: 1, status: String::new(), label: "write tests".into() });
session.commit().await?; // status is stored as "pending", not ""

session.add(&Task { id: 2, status: "active".into(), label: "ship it".into() });
session.commit().await?; // status is stored as "active" — a non-default value is left alone
```

This is a mapping-layer default, distinct from a database-side column `DEFAULT` — `TableSchema`'s own `ColumnInfo::default` (see "Schema introspection" above) reflects the latter, and the two are unrelated; a column can have either, both, or neither. Since Rust has no "unset" field state, there's no way to distinguish "this field was deliberately left at its default" from "this field was explicitly set to a value that happens to equal the type's default" (e.g. a genuine `0`, or an explicitly empty `String`) — both get the default fragment substituted, which is the one sharp edge of this feature: pick a field's zero value carefully if a real, meaningful zero needs to reach the row untouched. Not usable on a `#[table(primary_key)]` field — a compile error, since a primary key's value must always be supplied explicitly rather than left for a default to fill in.

### Computed / hybrid properties

SQLAlchemy's `@hybrid_property` is a single Python method that runs as plain code on an instance but compiles to a SQL expression when accessed on the class (e.g. in a filter) — Rust has no equivalent of one piece of syntax dispatching differently on instance-vs-class access, so `#[hybrid(name = "...", expr = "...")]` splits it into two generated items instead, both derived from one small arithmetic expression so they can't drift apart from each other:

```rust
#[derive(Mapped)]
#[table(name = "line_items")]
#[hybrid(name = "total", expr = "price * quantity")]
struct LineItem {
    #[table(primary_key)]
    id: i64,
    price: i64,
    quantity: i64,
}

let item = LineItem { id: 1, price: 10, quantity: 3 };
item.total(); // 30 — plain Rust, computed from this instance's own fields

let rows: Vec<LineItem> = engine
    .fetch_all_as(&Select::from(&LineItem::table()).filter(LineItem::total_expr().gt(15_i64)))
    .await?; // the same computation, as a portable SQL expression
```

`expr` accepts `+`/`-`/`*`/`/` over the struct's own field names, integer/float literals, and parentheses for grouping, plus — at most once, at the very top of the expression — a `<`/`<=`/`>`/`>=`/`==`/`!=` comparison of two such arithmetic sub-expressions:

```rust
#[hybrid(name = "is_expensive", expr = "price > 50")]
```

`is_expensive()` returns `bool` (inferred automatically for a comparison, instead of the usual "first referenced field's type" inference) and `is_expensive_expr()` is already a complete boolean condition — pass it straight to `.filter(...)`, no `.gt(...)`/etc. needed on top the way a bare arithmetic `_expr()` like `total_expr()` does. Comparisons can't be chained (`a < b < c`, same restriction Rust's own grammar has) or nested inside an arithmetic sub-expression or parenthesized group — only ever one, at the top.

Still deliberately nothing richer beyond that (no string functions, `CASE`/`COALESCE`, boolean combinators like `&&`/`||`, or referencing another table's columns), since this is meant to render identically as both a plain Rust expression and a portable `Expr` tree, and that only holds for the arithmetic/comparison ANSI SQL already shares with Rust. `#[hybrid(...)]` is repeatable (stack as many as a struct needs) and struct-level, not field-level, since a hybrid property isn't backed by a column of its own; `ty` is optional, inferred from the first field the expression references when omitted (or as `bool` for a comparison), or given explicitly (`ty = "i64"`) when that inference would guess wrong. `total_expr()`/`is_expensive_expr()` are usable anywhere an `Expr` is accepted — `.filter()`, `.columns()` — but not `.order_by()`, which only accepts a bare `Column` today, not an arbitrary expression.

Both halves come from the same parsed tree, so they can't disagree with each other about what "the same" computation means — but nothing checks that the expression string itself is valid SQL until it actually runs, unlike every other `Expr`-building method this crate offers, and referencing a field that doesn't exist on the struct (a typo) is caught at macro-expansion time, not silently ignored.

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

### Lifecycle hooks

The hooks above are session-level — they don't know which entity is being written, just that *something* is. A type that implements the `Lifecycle` trait by hand (hook bodies are arbitrary application logic, so this can't be derived) gets entity-level hooks instead, run by `Session::add_mut`/`update_mut`/`delete_mut` — opt-in siblings of `add`/`update`/`delete`, which stay exactly as they were, unhooked and infallible, for every type and every existing call site:

```rust
#[derive(Debug, Clone, Mapped)]
#[table(name = "documents")]
struct Document {
    #[table(primary_key)]
    id: i64,
    title: String,
    word_count: i64,
}

impl Lifecycle for Document {
    fn before_insert(&mut self) {
        self.title = self.title.trim().to_string(); // can mutate self
    }
    fn after_insert(&self) {
        println!("document {} inserted", self.id); // read-only; runs on a snapshot
    }
    fn validate(&self) -> rusty_db::Result<()> {
        if self.word_count < 0 {
            return Err(rusty_db::Error::QueryBuilder("word_count cannot be negative".into()));
        }
        Ok(())
    }
}

let mut doc = Document { id: 1, title: "  draft  ".into(), word_count: 2 };
session.add_mut(&mut doc)?; // before_insert trims the title, validate() passes, then it's queued
session.commit().await?; // after_insert prints once the insert actually lands
```

Every hook has a no-op (or `Ok(())`) default, so implementing only the ones a type needs is fine. The `before_*` hooks (the only ones taking `&mut self`) run synchronously inside `add_mut`/`update_mut`/`delete_mut` itself, before anything is queued — the only point where mutating a field can still affect the `INSERT`/`UPDATE` about to be built. `validate()` runs right after (skipped for `delete_mut` — there's nothing about a delete worth validating): returning `Err` rejects the write outright, propagated straight from `add_mut`/`update_mut` themselves, with nothing queued at all. The `after_*` hooks run later, on a snapshot of the entity taken right after `before_*`/`validate` ran (so `T: Clone` is required) — once this specific write has actually been sent successfully as part of a flush, matching `on_after_flush`'s own timing exactly: never for a write that fails and rolls back (including one rolled back because a *different*, later write in the same flush batch failed), and not gated on a later `commit()` either.

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

### Subquery eager loading

Every loader above is what SQLAlchemy calls `selectinload`: collect the batch's parent keys into Rust, then send them back as a literal `WHERE ... IN (?, ?, ...)` list. `rusty_db::relations` also has a `subqueryload`-style alternative for all four relationship shapes — `load_many_via_subquery`/`load_has_one_via_subquery`/`load_one_via_subquery`/`load_many_to_many_via_subquery` — that instead takes a `Select` picking out the parent side's key column, and finds matching rows by joining directly against that `Select` (wrapped as a `WITH` CTE) rather than shipping a key list back and forth:

```rust
let users_table = User::table();
let parent_ids = Select::from(&users_table)
    .columns([users_table.col("id")])
    .filter(users_table.col("active").eq(true));

let orders_by_user: HashMap<i64, Vec<Order>> =
    rusty_db::relations::load_many_via_subquery(&engine, parent_ids, "id", "user_id").await?;
```

This is the same trick that gives this crate `IN (subquery)` support and, for free, a genuine `FROM`-clause subquery — a CTE is already just a named, referenceable result set (see `Cte`/`Select::with`), so no new "derived table" query-builder primitive was needed to add it. It's a better fit than the `selectin` loaders above when the parent batch is itself the result of a nontrivial query, or large enough that materializing every key into a Rust `Vec` and back out as bind parameters is wasteful — for an already-in-hand `Vec` of parents (the common case, e.g. rows you just fetched), the plain `load_many`/etc. above stay simpler and avoid the extra `WITH`/`JOIN`.

`#[has_many(...)]`/`#[has_one(...)]`/`#[belongs_to(...)]`/`#[many_to_many(...)]` each also generate a `_via_subquery`-suffixed convenience method around the matching function above, alongside their existing select-in method — no separate `strategy = "..."` attribute parameter needed, both loaders are always generated together:

```rust
let users_table = User::table();
let parent_ids = Select::from(&users_table)
    .columns([users_table.col("id")])
    .filter(users_table.col("active").eq(true));

let orders_by_user: HashMap<i64, Vec<Order>> =
    User::load_orders_via_subquery(&engine, parent_ids).await?;
```

The parent primary key column (`has_many`/`has_one`/`many_to_many`) or the `foreign_key` column named in the attribute (`belongs_to`) is filled in automatically from the type's own mapping, the same way the select-in method's key column already is — `parent_ids`/`foreign_key_ids` only needs to select that one column, however it's filtered/joined.

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

This covers Core (query builder — including aggregate functions/expression columns via `SelectExpr`, `GROUP BY`/`HAVING`, set operations (`UNION`/`UNION ALL`/`INTERSECT`/`EXCEPT`) via `SetOperation`, SQL functions/arithmetic/`CASE`/`COALESCE`, subqueries (`IN (subquery)`, correlated `EXISTS`, scalar subqueries), CTEs (`WITH`/`WITH RECURSIVE` via `Cte`), and window functions (`ROW_NUMBER`/`RANK`/`DENSE_RANK`, and aggregates as window functions, via `.over(...)`/`Window`), a portable DDL builder (`CreateTable`/`DropTable`/`CreateIndex`/`DropIndex`/`AlterTable` — including `rename_column`, spelled identically on all three dialects — with a portable `ColumnType`), and Alembic-style autogenerate diffing a mapped type's expected shape against a live database (`Engine::autogenerate_migration`, `AutogenerateOptions` opting into caller-hinted column renames and allow-listed whole-table drops beyond the conservative add/drop-column default — still no type-change detection either way) — connections (pooling/tuning, connection-level event hooks on connect/checkout/checkin, tunable per-connection statement-cache capacity, streaming query results via `fetch_stream`/`fetch_stream_as`, TLS, read replicas, sharding via `ShardRouter` (naive modulo or consistent-hash routing), query timeouts), first-class `Uuid`/`BigDecimal`/`Json`/temporal (`NaiveDate`/`NaiveTime`/`NaiveDateTime`/`DateTime<Utc>`)/array (`Vec<T>`) value types), a thin mapping layer (`#[derive(Mapped)]`, `#[derive(MappedEnum)]`/`#[derive(MappedNewtype)]` for custom field types, joins, has-many/has-one/belongs-to/many-to-many select-in eager loading plus a generated `_via_subquery` convenience method for each (joining directly against a caller-supplied `Select` wrapped as a CTE instead of shipping a parent key list back and forth — also callable directly as `rusty_db::relations::load_many_via_subquery`/`load_has_one_via_subquery`/`load_one_via_subquery`/`load_many_to_many_via_subquery`), cascade delete/orphan rules, mapping-level column defaults via `#[table(default = "...")]`, computed/hybrid properties via `#[hybrid(name = "...", expr = "...")]` (arithmetic, plus a single top-level comparison producing a `bool`-typed hybrid — see "Richer hybrid-property expressions" below for what's still missing), versioned migrations (standalone via `Migrator`, or folded into a session's transaction via `session.migrate`), automap-style struct generation from schema reflection (`Engine::automap_table`/`automap_all`), a hand-implemented `Lifecycle` trait for entity-level before/after/`validate` hooks (`Session::add_mut`/`update_mut`/`delete_mut`), and a unit-of-work `Session` with autoflush and an identity map (including eviction on delete). Three drivers exist — SQLite, PostgreSQL, and MySQL/MariaDB — all built the same way (wrapping `sqlx`) and all exercised by the test suite. The Postgres and MySQL tests run against real servers when reachable (`POSTGRES_TEST_URL`/`MYSQL_TEST_URL`, defaulting to local `rusty`/`rusty` test databases) and just skip themselves rather than fail if one isn't — so `cargo test` stays green without either installed, but this environment does have both, and both are actually exercised here.

`tests/concurrent_sessions.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover multiple `Session`s sharing one `Engine`/connection pool at the same time — `Session` is intentionally `!Send` (it hands out `Rc`s for the identity map), so these run via `tokio::task::LocalSet`/`spawn_local` rather than `tokio::spawn`, which is the standard way to get genuinely concurrent, interleaved execution of `!Send` futures on one thread. They cover independent commits landing correctly under a burst of concurrent sessions, one session's flushed-but-uncommitted write staying invisible to a concurrent reader on a separate connection (deterministically ordered via a `oneshot` channel, not timing), and two sessions never sharing identity-map state for the same row. Same skip-if-unreachable behavior as the Postgres/MySQL smoke tests; each test uses its own table to avoid colliding with other tests running concurrently against the same live server.

`tests/session_expire_on_commit.rs` (SQLite) covers `Session::with_expire_on_commit`: the identity map is cleared after a real commit but left untouched by a no-op `commit()` (nothing was ever flushed or read, so no transaction was ever opened), untouched entirely without the option set, and a row fetched after expiration reflects the database's actual post-commit state — including a change made directly through the underlying `Engine`, bypassing the session — rather than a stale in-memory edit or the old cached handle.

`tests/pool_exhaustion.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `PoolConfig` itself: a `connect()` beyond a pool's `max_connections` blocks rather than erroring or handing out a duplicate connection, and succeeds once an outstanding connection is dropped back to the pool (proven with `tokio::time::timeout` rather than assumptions about scheduling order); a higher `max_connections` is honored before the pool starts blocking; a configured `acquire_timeout` errors out promptly instead of blocking forever; and a burst of `Session`s serializes correctly (no lost or corrupted writes) when they're all forced to take turns on a single-connection pool.

`tests/two_phase_commit.rs` (SQLite) confirms `begin_two_phase`/`commit_prepared`/`rollback_prepared` report `Error::Unsupported` there, since SQLite has no concept of a transaction prepared independently of its connection; `_postgres`/`_mysql` exercise the real thing against a live server — a prepared-but-not-yet-committed write stays invisible, `commit_prepared` makes it visible, `rollback_prepared` discards it instead — with `commit_prepared`/`rollback_prepared` always issued from a connection distinct from the one that prepared, since a real coordinator resolving a distributed transaction typically isn't the same connection (or even the same process) that prepared it. Getting MySQL/MariaDB's `XA` transactions working correctly through a pooled connection took two fixes beyond the query builder itself: `XA COMMIT`/`XA ROLLBACK` don't reliably resolve a transaction prepared on a different connection when sent through `sqlx`'s prepared-statement protocol (works fine as plain text SQL, the same wire format the `mysql` CLI uses — an `execute_unprepared` `Connection` method, MySQL-only override, sends these as raw text instead), and a connection that just ran `XA PREPARE` is left unable to run anything else at all until it resolves its own prepared transaction, so `Transaction::prepare` closes that connection outright afterward instead of returning it to the pool for reuse (which would otherwise break whoever got it next).

`tests/replica_set.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `ReplicaSet`: reads round-robin across healthy replicas (verified by seeding each stand-in replica with its own marker row and checking which one a read actually returned, rather than peeking at any internal routing state); a down replica fails over to the next healthy one instead of surfacing an error; every replica being down falls back to the primary; a `ReplicaSet` with zero replicas configured always uses the primary; and writes (`execute`, `Session`) always land on the primary regardless of replica health. Since real database replication is a server-side feature this crate can't spin up in a sandbox, a "down" replica/primary is simulated with a minimal fake `Driver` whose `connect()` always returns `Error::Connection` — the same failure shape a genuinely unreachable server produces — rather than by actually taking a live server down mid-suite; the Postgres/MySQL versions otherwise use real, separate tables on the live servers as replica stand-ins.

`tests/relations.rs` (SQLite) now also covers `#[has_one(...)]`: a batch of parents where only some have a matching child comes back with entries only for those that do (same "no entry at all" shape `has_many` already had for a childless parent); and a parent with *two* matching child rows — a relationship that isn't actually one-to-one — returns `Error::Conflict` rather than silently keeping or dropping one of them.

It also covers `#[many_to_many(...)]`: a batch of parents joined through a join table to a shared and a distinct set of targets comes back grouped correctly per parent (a post tagged `rust`+`systems` and another tagged `rust`+`databases` both get the right two tags, with `rust` correctly appearing under both), a parent with no join-table rows at all has no entry in the map, and empty input returns an empty map, same as the other three relationship kinds.

It also covers all four `*_via_subquery` functions: `load_many_via_subquery` matches `load_many`'s result exactly for a parent batch narrowed by the caller's own `Select` filter (a user excluded by that filter is absent from the map even though its orders genuinely exist, proving the filter — not just "has no children" — is what's driving the result); `load_has_one_via_subquery` matches `load_has_one` and reports the identical `Error::Conflict` for the same not-really-1:1 data; `load_one_via_subquery` matches `load_one`'s belongs-to result; and `load_many_to_many_via_subquery` matches `load_many_to_many`'s join-table result. Each test builds the same fixture data the select-in version's own test uses and asserts the two strategies agree, rather than re-deriving expectations from scratch. A further test exercises all four *derive-generated* `_via_subquery` methods directly (`User::load_orders_via_subquery`, `User::load_profile_via_subquery`, `Order::load_user_via_subquery`, `Post::load_tags_via_subquery`) rather than the plain functions they wrap, confirming the parent/foreign-key column the macro fills in automatically (from the type's own primary key or the attribute's `foreign_key`) is actually correct end to end.

It also covers cascade rules (`delete_cascading`): deleting a user with `cascade = "delete"` `has_many`/`has_one` relations removes both its orders and its profile along with the user itself, while a different user's own orders are left completely untouched; deleting a team with a `cascade = "orphan"` `has_many` leaves its players in place but with their foreign key nulled out rather than deleting them; and deleting a post with a `cascade = "delete"` `many_to_many` removes only that post's own join-table rows — a tag shared with another post survives, and so does the other post's own join row and its own view of that tag through `load_tags`.

`tests/uuid_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Uuid`/`Uuid`: a mapped struct's `Uuid` field (including an `Option<Uuid>` one) round-trips correctly on every backend, and — Postgres-only — a native `UUID` column decodes as `Value::Uuid` directly rather than `Value::Text`. Getting a `NULL` into a nullable `Uuid` column on Postgres surfaced a real, pre-existing bug unrelated to UUIDs specifically: binding a `NULL` parameter always declared it as an `int8` (`query.bind(None::<i64>)`), which Postgres's strict per-parameter type checking then rejected for a target column of any type without an implicit/assignment cast from `int8` (`UUID`, `BOOLEAN`, and `JSON` all reproduce it) — a bug that had simply never been hit yet, since no earlier test happened to insert an explicit `NULL` into one of those column types. Fixed at the query-builder level, not the driver: `Insert`/`Update`/`BulkInsert` now render a `Value::Null` assignment as the bare SQL literal `NULL` instead of a bound placeholder, sidestepping the type-declaration conflict entirely (and doing so for every dialect, not just Postgres, since a literal `NULL` has no type to conflict with anywhere).

`tests/decimal_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Decimal`/`BigDecimal`: a mapped struct's `BigDecimal` field (including an `Option<BigDecimal>` one) round-trips correctly on every backend, and — Postgres-only — a native `NUMERIC` column decodes as `Value::Decimal` directly rather than `Value::Text`.

`tests/json_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Json`/`Json`: a mapped struct's `Json` field (including an `Option<Json>` one) round-trips correctly on every backend, and — Postgres-only — a native `JSONB` column decodes as `Value::Json` directly rather than `Value::Text`. Getting this working on MySQL/MariaDB surfaced a real quirk: its `JSON` columns report as one of MySQL's own `BLOB`-family types at the wire-protocol level, so they decode as `Value::Bytes`, not `Value::Text` the way `DECIMAL`/`UUID`-as-text columns do elsewhere on that backend — `FromValue for Json` accepts that form too (via UTF-8 first, then JSON parsing), so a `Json` field round-trips there without the caller ever needing to know about the difference.

`tests/temporal_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Date`/`Value::Time`/`Value::DateTime`/`Value::Timestamp`: a mapped struct's four temporal fields round-trip correctly on every backend; on SQLite specifically, a `NaiveDateTime` field parses back correctly regardless of whether the stored text is space- or `T`-separated, and (as a standalone conversion-logic check, not a database one, since SQLite can't tell the two apart at decode time to begin with) `Value::DateTime`/`Value::Timestamp` fall back to each other correctly. Postgres and MySQL each get a test confirming all four decode as their native variant directly rather than `Value::Text` — MySQL's specifically confirms its `DATETIME`/`TIMESTAMP` split decodes as `Value::DateTime`/`Value::Timestamp` respectively despite being identical bytes on the wire.

`tests/array_value.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `Value::Array`/`Vec<T>`: a mapped struct's array fields (including an `Option<Vec<T>>` one, and an empty array) round-trip correctly on every backend; on SQLite, a `Vec<T>` field also parses back correctly from raw JSON array text inserted outside this crate's own binding. Postgres gets a test confirming all four array columns decode as `Value::Array` directly rather than `Value::Text`, plus two tests pinning down the one documented sharp edge: an empty array round-trips into a `TEXT[]` column (where the binding default happens to match) but is a clear database error against a differently-typed empty array column, and a native array containing a genuine `NULL` element decodes fine into the `Vec<Value>` escape hatch but is a decode error into a concrete `Vec<i64>` field. MySQL's confirms the same JSON-array flattening `tests/json_value_mysql.rs` already covers for `Value::Json` applies here too.

`tests/mapped_enum.rs` (SQLite) covers `#[derive(MappedEnum)]`: a text-mode enum field round-trips through a mapped struct, including a `#[mapped_enum(rename = "...")]`'d variant storing its overridden text rather than the default snake_case name; an unrenamed variant stores its plain snake_case name; an `as_int`-mode enum field round-trips using each variant's own discriminant, including an explicit `= N` one; and a stored value with no matching variant (an unrecognized string, in text mode) is a decode error rather than a silent fallback. No `_postgres`/`_mysql` counterparts — the generated `From`/`FromValue` impls are pure Rust-side conversions to/from `Value::Text`/`Value::I64`, both already exercised per-driver everywhere else, with nothing left that's actually driver-specific to prove again.

`tests/mapped_newtype.rs` (SQLite) covers `#[derive(MappedNewtype)]`: a mapped struct's newtype fields (wrapping `String`, `i64`, and — composed — a `MappedEnum`) round-trip correctly, each storing in its wrapped type's own form; and an `Option<Newtype>` field stores and reads back as `NULL` when `None`. Same reasoning as `MappedEnum` above for skipping `_postgres`/`_mysql` counterparts — the generated impls just delegate to the wrapped field's own already-covered conversion.

`tests/query_timeout.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `with_timeout`: an operation finishing inside its timeout succeeds normally; a genuinely slow/blocked operation is cancelled and returns `Error::Timeout` instead of hanging; a cancelled operation leaves the pool usable afterward rather than stuck; a timeout on one call has no lingering effect on later calls; and aborting a task running a slow operation (`JoinHandle::abort`, the other standard way to cancel a Rust future) cancels it the same way a timeout would. The SQLite version gets a genuinely blocked (not simulated) query via real lock contention — holding SQLite's write lock on one connection while a second connection attempts a conflicting write, which sqlx's SQLite driver retries against its own 5-second `busy_timeout` rather than erroring immediately. The Postgres/MySQL versions use their real, built-in `pg_sleep()`/`SLEEP()` functions for a genuinely slow query instead.

`tests/schema_introspection.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `list_tables`/`table_schema`: created tables are reported (and, for SQLite, internal `sqlite_*` tables never are); a table's columns come back with the right type names, nullability, and primary-key flags; a table that doesn't exist reports `Ok(None)` rather than an error; a column's `DEFAULT` reflects as `Some(...)` verbatim text (`None` for a column with none); a `UNIQUE` constraint's name and covered column(s) come back correctly; and a `CHECK` constraint's expression comes back correctly. Chasing down why the SQLite version's PRAGMA-based columns were coming back entirely `Null` uncovered a real bug in the SQLite driver's row decoder: it was keying off each column's *declared* type (`"NULL"` when SQLite can't infer one statically — true for `PRAGMA` output and other non-plain-table results, not just when the value itself is null) instead of that value's actual *runtime* type, so it silently decoded every column as `Value::Null` whenever SQLite left the static type undeclared. Fixed by falling back to the per-value runtime type (via `try_get_raw`) exactly when the declared type is `"NULL"`, leaving the existing behavior for ordinary table queries (which do have a meaningful declared type) unchanged — confirmed by the rest of the suite still passing unmodified. The MySQL version's `table_schema_reports_defaults_unique_and_check_constraints` test also covers a MariaDB-specific `default` quirk found the same way (against a real server, not just the query builder): a nullable column with no `DEFAULT` clause at all, one with an explicit `DEFAULT NULL`, and one with a genuine string-literal default of the text `NULL` (quoted `'NULL'` in the catalog) — the first two both normalize to `None`, and the third reflects `Some("'NULL'")`, not the ambiguous ones.

`tests/automap.rs` (SQLite, requires the `derive` feature) covers `Engine::automap_table`/`automap_all`: the generated source structurally matches a real reflected schema (right struct/table name, `#[table(primary_key)]` on the right field, a nullable column wrapped in `Option<T>`); `automap_all` concatenates every table and lists a detected foreign key as a comment; and a nonexistent table errors instead of generating garbage. The test that actually proves the generated *pattern* is valid, working code — since compiling the generated string itself isn't practical inside a test — is a hand-written struct following exactly the shape `automap_table` would produce for the same table (including a Rust-keyword column name escaped to `r#type`-style via a raw identifier), which is then round-tripped through a real insert/fetch via the actual derive macro. The type-mapping heuristic itself (`rust_type_for`) and the identifier-sanitizing logic (`sanitize_field_name`) are unit-tested directly in `crates/rusty-db-core/src/automap.rs`, covering every dialect's spelling of the common column types (Postgres's `"timestamp without time zone"` vs. `"with time zone"`, MySQL's `"int(11) unsigned"`, SQLite's bare `"INTEGER"`) and the identifier edge cases (a leading digit, an invalid character, a keyword).

Reflecting `CHECK` constraints turned up two more real per-backend quirks, each confirmed against a live server rather than assumed: Postgres's `information_schema.check_constraints` also reports a synthetic entry for every `NOT NULL` column (its catalog-level way of representing `NOT NULL`, already covered by `ColumnInfo::nullable`) — fixed by querying `pg_catalog.pg_constraint` directly and filtering to `contype = 'c'` (genuine `CHECK`), which also gives a cleaner expression via `pg_get_expr` than `information_schema`'s doubly-parenthesized text. And SQLite has no catalog for `CHECK` constraints at all, so `crates/rusty-db-sqlite/src/lib.rs`'s `check_constraints` module recovers them with its own small tokenizer over the table's `CREATE TABLE` text (unit-tested directly, independent of a live database, with 6 tests covering a named constraint, synthetic positional names for anonymous ones, an inline column-level `CHECK`, nested parens and string literals inside the expression, a `CHECK`-like word inside a string literal not being mistaken for a real one, and a table with no `CHECK` constraints at all).

Foreign key reflection (`schema.foreign_keys`) is covered by the same `tests/schema_introspection*.rs` files: both a single-column and a genuinely composite (two-column) foreign key come back with their columns correctly paired against the referenced table's columns, in order. Getting that pairing right for a composite key surfaced a real `information_schema` pitfall on Postgres: joining `key_column_usage` to `constraint_column_usage` by constraint name alone (the usual approach) has no shared ordinal between the two, so it cross-joins every local column with every referenced column of a multi-column key instead of pairing them up correctly. Fixed by querying `pg_catalog.pg_constraint`'s own `conkey`/`confkey` arrays instead and pairing them positionally via `unnest(...) WITH ORDINALITY`, which is exact regardless of how many columns are involved. MySQL/MariaDB's `information_schema.key_column_usage` already includes `referenced_table_name`/`referenced_column_name` directly on each row (a MySQL-specific extension beyond the SQL standard), so no equivalent care was needed there. SQLite doesn't name foreign keys at all, so `ForeignKey::name` there is synthetic (`"fk_1"`, `"fk_2"`, ...), grouped from `PRAGMA foreign_key_list`'s rows sharing the same `id`.

Index reflection (`schema.indexes`) rounds out the same test files: a `UNIQUE`-backed index and a plain, non-unique multi-column index both come back with the right name, columns (in index order), and `unique` flag, and the primary key's own automatically-created index is never included among them (already `ColumnInfo::primary_key`) — verified on Postgres via a `NOT EXISTS` against `pg_constraint` (the reliable way to identify it, since its name isn't predictable), on MySQL/MariaDB by excluding `index_name = 'PRIMARY'`, and on SQLite by excluding `PRAGMA index_list`'s `origin = "pk"` row.

`tests/backup_restore.rs` (SQLite) and its `_postgres`/`_mysql` counterparts cover `backup`/`restore`: a backup captures every row of every table; restoring returns the database to exactly its backed-up state after rows are deleted, updated, and added; a dump backed up from one `Engine` restores correctly into a completely different one; a failing restore (a corrupted dump with a duplicate primary key partway through) rolls back the *entire* transaction rather than leaving the table partially wiped; and backing up an empty table round-trips correctly. The Postgres/MySQL versions scope every backup/restore to their own table via `backup_tables`, since a whole-database `restore()` would otherwise risk wiping tables that other tests running concurrently against the same shared live server still need.

`tests/tls_postgres.rs`/`tests/tls_mysql.rs` cover encrypted connections (no SQLite equivalent — it has no network/TLS concept at all): the default `sslmode`/`ssl-mode` (`prefer`/`PREFERRED`) already opportunistically encrypts against a server that supports it, verified against the server's own bookkeeping (`pg_stat_ssl`, `SHOW STATUS LIKE 'Ssl_cipher'`) rather than just assuming the connection attempt succeeding means it's encrypted; `disable`/`DISABLED` produces a genuinely plain connection; `require`/`REQUIRED` encrypts without verifying the certificate; and `verify-full`/`verify-ca`/`VERIFY_CA` succeed with the server's actual CA (and, for `verify-full`, matching hostname) but fail closed against a CA that doesn't match — proving certificate verification actually verifies something rather than silently accepting any well-formed root. This environment's Postgres already has TLS enabled by default (a self-signed cert the Debian/Ubuntu package generates automatically); MariaDB needed a one-time setup of a self-signed CA/server certificate (`ssl-ca`/`ssl-cert`/`ssl-key` in `/etc/mysql/mariadb.conf.d/50-server.cnf`) to have any TLS support to test against at all.

`tests/audit_log.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly exercise change tracking, since the identity-map/uncommitted-write-visibility behavior is already covered by the SQLite version and doesn't need re-proving per driver) cover `Session`'s opt-in audit logging: a plain session never creates an audit table at all; insert/update/delete each get recorded with the right table, operation, and rendered SQL/params; a failed write's audit entry rolls back right along with the write itself (proving the audit trail shares the write's own transaction rather than being an independent, potentially-inconsistent side effect); the session's own `audit_log()` sees its not-yet-committed entries (autoflush + read-through-transaction, same as `get`/`load_all`); and a custom audit table name is honored. The Postgres/MySQL versions use their own audit table name per test (`with_audit_log_table`) to avoid colliding with other tests running concurrently against the same shared live server, and are careful to `commit()` (closing the transaction `audit_log()` itself opens) before any cleanup `DROP TABLE` from a separate connection — otherwise the cleanup deadlocks waiting on a lock the still-open session transaction is holding.

`tests/optimistic_locking.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — the two conflict-detection tests, since the identity/no-op-without-`#[table(version)]` cases don't need re-proving per driver) cover `#[table(version)]`: updating with the current version succeeds and increments the stored version; updating with a stale version (superseded by someone else's edit) fails with `Error::Conflict` and leaves the other edit intact; updating a row someone else already deleted also conflicts; the same two cases for `delete`; and a type with no `#[table(version)]` field keeps its pre-existing behavior of a silent no-op on a stale/missing-row write — proving the feature is fully opt-in.

`tests/bulk_insert.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — the round-trip and rollback tests, since `BulkInsert::combine`'s own rendering/validation is pure query-builder logic that doesn't need a live server to re-prove per driver) cover `BulkInsert`/`Session::add_all`: combining several `Insert`s renders one statement with one `VALUES` group per row; combining zero inserts yields `None` rather than an invalid empty statement; combining `Insert`s from different tables is rejected; `add_all` queues exactly one pending write for the whole slice (not one per entity) and an empty slice is a no-op; a failing bulk insert (a duplicate primary key partway through) rolls back the entire batch, including rows that would've inserted fine on their own; and a `BulkInsert` works standalone through `engine.execute()`, without a `Session` at all.

`tests/soft_delete.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly exercise the feature end to end, since the query-builder-level `not_deleted_filter`/`load_active` coverage and the plain-type-is-unaffected case don't need re-proving per driver) cover `#[table(soft_delete)]`: `Session::delete` marks the row (`SET <column> = true`) instead of removing it; `Session::get` treats an already-marked row the same as one that was never there, including from a fresh `Session` with no identity-map cache from before the delete; `Mapped::not_deleted_filter()` and `Session::load_active` both exclude marked rows from query results; calling an entity's own `delete_query()` directly still issues a real, unmarked `DELETE`; and a type with no `#[table(soft_delete)]` field keeps `Session::delete`'s pre-existing behavior of a genuine hard delete, proving the feature is fully opt-in.

`tests/mapped_defaults.rs` (SQLite) covers `#[table(default = "...")]`: a field left at its type's default (`String::new()`, `0_i64`) gets the mapping-level default substituted on insert instead; a field explicitly set to a non-default value is preserved untouched; a genuine value that happens to equal the type's default (a deliberate `0`, not "left unset") also gets the default substituted, documenting the feature's one sharp edge; and `BulkInsert::combine` correctly handles a batch mixing defaulted and explicit rows for the same column. The query-builder level (`Insert::raw_value`/`maybe_raw_value`, and `BulkInsert` combining rows with a mix of raw and bound assignments for one column) is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`.

`tests/pool_stats.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests proving `pool_stats()` against a real network-backed pool, since the pure counting behavior doesn't need re-proving per driver) cover `Engine::pool_stats()`: checking out a connection is reflected immediately (`in_use` up, `total_acquires` up), and releasing it moves it back to `idle` without touching `total_acquires`; the counter keeps accumulating across repeated checkouts rather than just tracking the current one; and `waiters` goes to `1` while a second acquire is genuinely blocked behind a pool of size one, then back to `0` once it unblocks. Discovering the exact numbers to assert here surfaced two things worth knowing about sqlx's own pool: it eagerly opens (and keeps, idle) one connection up front to validate the URL when the pool is first constructed, so a "fresh" pool already reports that one as active/idle even though `total_acquires` correctly stays `0` (that startup connection never went through `Driver::connect`); and releasing a connection back to the pool isn't synchronous inside `drop` — the tests give it a brief moment (`tokio::time::sleep`) before reading the snapshot back, the same pattern already used elsewhere in this suite for other not-quite-synchronous async cleanup. `waiters`/`total_acquires` are the two numbers sqlx doesn't expose on its own, so each driver keeps a small `PoolMetrics` (a couple of atomics behind an `Arc`) alongside its pool for just those two; every other `PoolStats` field is a zero-cost read of the pool itself.

`tests/pool_hooks.rs` (SQLite) covers `PoolConfig::with_on_connect` behaviorally, not just by checking that the SQL string got accepted somewhere: `PRAGMA case_sensitive_like` is per-connection and off by default, so a fresh connection without the hook still matches `'ABC' LIKE 'abc'` while one built with `.with_on_connect("PRAGMA case_sensitive_like = ON")` genuinely stops matching it. `pool_hooks_postgres.rs` covers all three hooks against a real server, using Postgres's arbitrary custom GUC variables (`SET myapp.whatever = ...` / `current_setting(...)`, no predeclaration needed) to prove each one actually executed on the exact physical connection handed back: `.with_on_connect` sets `application_name` once, confirmed via `SHOW application_name`; and — with the pool constrained to size one so the same physical connection is guaranteed reused — `.with_before_acquire` and `.with_after_release` are both confirmed to have run on that one connection by checking two different marker variables after each of two successive acquires. `pool_hooks_mysql.rs` gets a reduced single test (`.with_on_connect` setting a user-defined `@variable`, MySQL's equivalent of a session-local marker) confirming the same driver-agnostic hook-wiring mechanism actually executes against a real MySQL/MariaDB connection too.

`tests/pool_statement_cache.rs` (SQLite) and its `_postgres` counterpart cover `PoolConfig::with_statement_cache_capacity` via `Connection::cached_statement_count()`, a direct observation rather than an inference from timing: five genuinely distinct query shapes (not just different parameter values) run against one connection with no capacity set leave all five cached, well under the underlying driver's default of 100; the same five against a connection configured with `.with_statement_cache_capacity(2)` leave exactly two cached, the rest LRU-evicted. Postgres gets the same test again since it's the backend where a cached prepared statement genuinely saves a server-side parse/plan round trip — the actual point of the feature, not just a portable LRU-counting exercise.

`tests/streaming.rs` (SQLite) covers `Engine::fetch_stream`/`fetch_stream_as`: streamed rows come back in the same order and with the same values `fetch_all` would return; `fetch_stream_as` decodes each one into a `#[derive(Mapped)]` type as it arrives; and — the one thing that actually distinguishes genuine row-at-a-time streaming from a `fetch_all`-then-wrap-in-a-`Stream` shortcut — against a pool constrained to size one, the connection a stream checked out still shows as `in_use` after only the first row has been read, and only drops back to idle once the stream itself is dropped, not before. `streaming_postgres.rs` proves the same connection-holding behavior over a real network-backed pool, not just SQLite's in-process one.

`tests/query_builder_extras.rs` (SQLite) and its `_postgres` counterpart (a reduced version there — just the things that are actually Postgres-specific: `ILIKE`, `RETURNING` on `UPDATE`/`DELETE`, and a `SetOperation`'s bind parameters landing correctly with Postgres's numbered `$1, $2, ...` placeholders, since SQLite/MySQL's `?` placeholders don't encode a position at all and so carry no comparable risk; `DISTINCT`/`BETWEEN`/`Table::alias`/`Expr::text`/aggregates/`GROUP BY`/`HAVING` have no dialect-specific behavior and don't need re-proving against a live server) cover the newer query-builder additions against a real SQL engine rather than just checking rendered SQL strings: `Select::distinct()` actually dedupes matching rows; `Column::between` includes both boundaries inclusively; `Column::ilike` matches case-insensitively via its portable `LIKE` fallback on SQLite and via Postgres's native `ILIKE` keyword; `.returning(...)` on `Update`/`Delete` actually returns the requested columns from a real Postgres server (and is silently ignored on SQLite, whose dialect doesn't support `RETURNING` at all); `Table::alias` supports a genuine self-join (an `employees` table joined to itself to pair each employee with their manager's name); `Expr::text` composes a raw SQL fragment (with its own `?` placeholder) together with an ordinary builder-constructed filter via `.and(...)`; `Expr::count_all()`/`Column::sum`/`avg`/`min`/`max` execute and decode correctly against a real (if in-memory) SQLite engine, including a plain column and an aggregate composed in the same `SELECT` via `SelectExpr`, and an arbitrary (non-aggregate) expression column; `Select::group_by`/`.having` correctly group case-sensitive customer names, sum each group's amounts, and `HAVING` narrows to just the group whose total actually exceeds the threshold; and `Select::union`/`union_all`/`intersect`/`except` dedupe, keep duplicates, intersect, and subtract two overlapping result sets exactly as each operator should, including chaining three arms together and (Postgres-only) a two-arm `UNION` whose bind parameters land in the right arm despite both being plain literal-comparison filters. The pure rendering side of all of these — including that `RETURNING` is dialect-gated, `ilike_operator()` picks the right keyword per dialect, an alias renders `<table> AS <alias>` while an unaliased `Table` is unchanged, `text()`'s `?` placeholders get rewritten to each dialect's real placeholder syntax in order, every aggregate/an arbitrary expression column renders with its `AS alias`, `GROUP BY` renders after `WHERE` and before `ORDER BY`, repeated `.having(...)` calls combine with `AND` the same as repeated `.filter(...)`, each `SetOperation` operator renders its own keyword, and — the one thing that would actually be wrong if it weren't true — a `SetOperation`'s bind parameters are numbered sequentially across every arm rather than each arm restarting its own placeholder count from one — is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`, alongside the rest of the query builder's SQL generation.

The same file also covers `Column::lower`/`upper`/`concat`/`add`/`sub`/`mul`/`div`, `Expr::now`/`coalesce`, `Case`, and `.eq_expr`/`.lt_expr`/etc.: `LOWER`/`UPPER`/concatenation/arithmetic execute and decode correctly (a computed `customer` name lowercased, uppercased, and concatenated with a literal; an `amount` doubled); `Case`/`Expr::coalesce` correctly tier customers by amount and fall back from a `NULL` nickname to a name; and `Expr::now()` composes with `.lt_expr(...)` to filter directly, not just render, since `Column`'s own literal-only comparisons can't take an arbitrary `Expr` like `Expr::now()` at all. `tests/query_builder_extras_mysql.rs` gets its own live-server test for the one thing here that's actually MySQL-specific: `.concat(...)` renders `CONCAT(a, b)` on that backend and actually returns the concatenated string against a real server, rather than the `0` a naive `a || b` would silently produce there instead (MySQL/MariaDB's `||` means logical `OR` under the default `sql_mode`, confirmed empirically against this exact server).

`query_builder_extras.rs` also covers subqueries against a real (customers-and-orders) schema: `.in_subquery(...)` filters rows against a `GROUP BY`/`HAVING`-shaped nested `Select` (only the customer whose grouped total actually clears the threshold matches); `Expr::exists(...)` and `.not()` wrapping it correctly split customers into "has at least one order" and "has none at all", including a customer with zero orders that an `INNER JOIN` would have silently dropped instead; and `Expr::subquery(...)` computes a real per-row aggregate (each customer's order total) and decodes as `None` — not `0` — for the customer with no matching orders, since `SUM` over zero rows is `NULL` in SQL. `query_builder_extras_postgres.rs` adds the one thing with real per-dialect risk here too: an outer filter and a nested subquery's own filter binding through the same, correctly-ordered `$1, $2, ...` parameter list rather than each restarting its own count. The pure rendering side — `IN (subquery)`, `EXISTS`, `NOT` wrapping `EXISTS`, a scalar subquery composing into a `SELECT` column with its own alias, and (again) Postgres parameter numbering across an outer query and its nested subquery — is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`.

`query_builder_extras.rs` also covers CTEs: `.with(...)` filters real rows through a named CTE (only orders clearing the CTE's own `amount > 40` filter come back); and `.with_recursive(...)` walks a genuine three-level management hierarchy (an `employees` table seeded with a root and two levels of reports) via `Cte::recursive_union_all`, computing each person's depth by having the recursive term add one to the CTE's own `depth` column on every step, and returning exactly the right person/depth pairs in order. `query_builder_extras_postgres.rs` adds the matching per-dialect-risk case: a CTE's own filter and the outer query's filter binding through the same, correctly-ordered `$1, $2, ...` parameter list. The pure rendering side — plain `WITH` prefixing a query that then selects from the CTE by name, `WITH RECURSIVE` rendering the anchor `UNION ALL`/`UNION` the recursive term, and (again) Postgres parameter numbering across a CTE and the outer query — is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`.

`query_builder_extras.rs` also covers window functions: `Expr::row_number()` resets to `1` at the start of each `PARTITION BY` group and counts up in `ORDER BY` order within it; `RANK`/`DENSE_RANK` are checked side by side against the same tied data (two orders with equal amounts) to confirm `RANK` skips ahead by the tie's size afterward while `DENSE_RANK` never does; and `.sum().over(...)` computes a genuine running total, per partition, that only ever grows within a partition and resets for the next one. `query_builder_extras_mysql.rs` gets its own live-server test too — not because the SQL differs (window functions are ANSI-standard, no `Dialect` hook needed, unlike `.concat`), but because they need a modern-enough server (MySQL 8.0+/MariaDB 10.2+) and MySQL's `SUM` over an integer column returns `DECIMAL` (sent over the wire as text) rather than the plain integer SQLite returns, which is worth an actually-executed check rather than just trusting the rendered SQL. The pure rendering side — `ROW_NUMBER`/`RANK`/`DENSE_RANK`, an aggregate composed with `.over(...)`, and a `Window` with only `PARTITION BY`, only `ORDER BY`, or neither (`OVER ()`) — is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`.

`tests/ddl.rs` (SQLite) covers the DDL builder end to end — every statement it renders is actually executed, then the resulting schema is used through ordinary inserts/selects, not just checked as rendered SQL text: a `CreateTable` with several `ColumnType`s builds a table that accepts real inserts and selects; `.if_not_exists()` makes a second `CREATE TABLE` a no-op instead of an error; a `.unique()` column and a `.check(...)` constraint both actually reject a row that violates them; a `.foreign_key(...)` clause is at least accepted as valid SQL (SQLite doesn't enforce foreign keys by default, and this crate doesn't turn `PRAGMA foreign_keys` on, so real enforcement is covered on Postgres/MySQL instead, below); `.primary_key().autoincrement()` assigns increasing ids to rows that never specify one; `.default_raw(...)` fills in a real database-side default when an `INSERT` omits that column entirely (distinct from `#[table(default = "...")]`, which substitutes at the Rust-struct level instead); `DropTable` actually removes a table (and `.if_exists()` makes a second drop a no-op); `CreateIndex::unique()` rejects a duplicate value until `DropIndex` lifts it, after which the same duplicate succeeds; and `AlterTable::add_column`/`drop_column` genuinely add/remove a column — verified through a *fresh* `Engine` reopened against the same file rather than the one that ran the `ALTER TABLE`, exactly the pattern `AlterTable`'s own docs recommend, since reusing the original connection is what triggers the upstream SQLite/`sqlx` panic documented there. `tests/ddl_postgres.rs`/`ddl_mysql.rs` are reduced versions covering just what's genuinely dialect-specific: native column-type mapping (`Uuid` round-trips as Postgres's real `UUID` type; `Json` round-trips as MySQL's native `JSON` type) alongside `.autoincrement()`'s per-dialect syntax (Postgres's `GENERATED ALWAYS AS IDENTITY`, MySQL's `AUTO_INCREMENT`); a foreign key genuinely rejecting an insert to a nonexistent parent row on both (unlike SQLite); `AlterTable::add_column`/`drop_column` again (Postgres's version also reopens a fresh `Engine` before verifying the drop, sidestepping its own gentler cached-plan-mismatch quirk; MySQL needs no such care, since it hits neither issue); and, MySQL-only, `DropIndex` actually needing `ON <table>` to resolve (confirmed by dropping a unique index and having the same previously-rejected duplicate insert succeed afterward). The pure rendering side — every column type's per-dialect spelling, a composite (multi-column) `.primary_key()` rendering as a table-level constraint, `.autoincrement()` panicking on a non-`ColumnType::I64` column, `.foreign_key(...)`/`.check(...)` rendering, `AlterTable::add_column`/`drop_column` rendering (including that `.not_null()`/`.default_raw(...)` after `.drop_column(...)` panics), and `DropIndex` only adding `ON <table>` for MySQL — is unit-tested directly in `crates/rusty-db-core/src/query/tests.rs`.

`tests/autogenerate.rs` (SQLite) covers `Engine::autogenerate_migration`/`TableSpec` end to end: a table missing entirely gets exactly one `CreateTable` statement (including its primary key), which — once executed — is genuinely usable through ordinary `#[derive(Mapped)]` inserts/selects, and re-diffing afterward finds nothing left to do; an already-up-to-date table generates nothing; a field added to the struct becomes a single `AlterTable::add_column`, and a field removed from it becomes a single `AlterTable::drop_column` — both verified by actually applying the statement and re-diffing (through a fresh `Engine`, per `AlterTable`'s own docs) to confirm convergence; a live table with no corresponding entry in `expected` at all (standing in for this crate's own migration-bookkeeping table, or a table tracked elsewhere) is never mentioned in the generated statements and is left completely untouched, whether or not any `expected` tables are actually present; a matching `AutogenerateOptions::renamed_columns` hint produces a single `AlterTable::rename_column` instead of a data-losing drop-plus-add, verified by actually applying it and confirming the row's pre-rename data survived under the new column name; and a table named in `AutogenerateOptions::allow_drop_tables` gets a real `DropTable` when it's live but absent from `expected` — while the exact same live table, with the option left off, is correctly left alone. `tests/autogenerate_postgres.rs`/`autogenerate_mysql.rs` are reduced single-test versions confirming the same generate-apply-reconverge round trip produces genuinely dialect-correct DDL there too (a nullable `Uuid`/`Json` field on each, respectively). The type-inference side (`#[derive(Mapped)]` mapping each documented field type — including `Option<T>` nullability, `Vec<u8>` vs. any other `Vec<T>`, and an unrecognized custom type — to its portable `ColumnType`) is unit-tested directly in `crates/rusty-db-derive/src/lib.rs`; the pure diff logic (create/add-column/drop-column decisions, a matching rename hint pre-empting the ordinary add/drop pair, a stale/irrelevant rename hint being ignored and falling back to add/drop, and an allow-listed vs. non-allow-listed table drop decision) is unit-tested directly in `crates/rusty-db-core/src/autogenerate.rs`; `AlterTable::rename_column`'s identical-across-dialects rendering (and that `.not_null()`/`.default_raw(...)` still panic after it, same as after `.drop_column(...)`) is unit-tested in `crates/rusty-db-core/src/query/tests.rs`.

`tests/bulk_update_delete.rs` (SQLite) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two round-trip tests, since the identity-map-bypass/audit-log/rollback behavior is `Session`-level logic that doesn't depend on which driver is underneath) cover `Session::bulk_update`/`bulk_delete`: a filter-scoped update/delete changes/removes every matching row in one statement (`pending_len()` stays `1` regardless of how many rows match); an already-cached identity-mapped instance stays exactly as it was in memory after a `bulk_update` touches its row (the database itself is genuinely updated, confirmed by re-fetching through a fresh `Session`); `bulk_delete` is always a real, hard `DELETE` even against a `#[table(soft_delete)]` type, bypassing the soft-delete column entirely; both are recorded the same way ordinary `add`/`update`/`delete` writes are when audit logging is enabled; and a failing write queued in the same batch (a duplicate primary key) rolls the bulk write back too, since they share one all-or-nothing transaction.

`tests/savepoints.rs` (SQLite, 6 tests) and its `_postgres`/`_mysql` counterparts (a reduced version there — just the two tests that most directly prove `SAVEPOINT`/`ROLLBACK TO SAVEPOINT`/`RELEASE SAVEPOINT` actually work against a real server, since the rest is `Session`-level logic independent of the driver underneath) cover `Session::savepoint`/`rollback_to_savepoint`/`release_savepoint`: rolling back to a savepoint undoes a sub-unit of work (whether already flushed or still only queued) without aborting the rest of the transaction, which keeps committing normally afterward; releasing a savepoint keeps its effects and lets the transaction continue; savepoints nest, rolling back an inner one independently of an outer one still open around it; and both an unreleased savepoint and a full `Session::rollback()` behave correctly regardless of whether a savepoint is still open — standard SQL behavior this crate doesn't need to do anything special to get right, beyond generating unique, safe (unquoted) savepoint names so nested savepoints within one session never collide.

`tests/session_query.rs` (SQLite) covers `Session::query`/`SessionQuery`: `.filter`/`.order_by` narrow and order the result set; `.limit`/`.offset` page through it; `.first()` returns just the first matching row (`None` when nothing matches); results come back through the identity map exactly like `load_all`'s do, including reflecting an in-memory change made through a handle fetched a different way; and `.active_only()` excludes soft-deleted rows for a `#[table(soft_delete)]` type, the same as `load_active`. No `_postgres`/`_mysql` counterparts — this is `Session`-level logic built entirely on `load_all` and `Select`'s own filter/order/limit/offset, both already exercised per-driver elsewhere.

`tests/session_hooks.rs` (SQLite) covers `Session::on_before_flush`/`on_after_flush`/`on_before_commit`/`on_after_commit`/`on_after_rollback`: `on_before_flush` only fires when a flush actually has something queued to send, not for a no-op flush; `on_after_flush` fires after a successful flush and in the right order relative to `on_before_flush`, but never fires at all for a flush that failed; `on_before_commit` fires on every `commit()` call regardless of whether anything was pending; `on_after_commit`/`on_after_rollback` only fire when a transaction actually existed to commit/roll back (not when `commit()`/`rollback()` was a no-op because nothing was ever flushed or read); and multiple hooks registered on the same event run in registration order. No `_postgres`/`_mysql` counterparts — hooks are plain `Session`-level callbacks with no interaction with which driver is underneath.

`tests/lifecycle_hooks.rs` (SQLite) covers `Lifecycle`/`Session::add_mut`/`update_mut`/`delete_mut`: `before_insert` can mutate the entity (a trimmed string) before it's queued; a failing `validate()` is returned immediately with nothing queued at all; `after_insert` hasn't run yet right after `add_mut` (only `before_insert` has), and only runs once `commit()` actually flushes the write; a rolled-back write (discarded before ever flushing) never fires `after_insert`; the same `before`/`validate`/`after` sequencing for `update_mut`, plus the updated row actually landing; `delete_mut` runs `before_delete`/`after_delete` but never `validate()` (a row with data that would fail `validate()` deletes fine); `add`/`update`/`delete` stay completely unhooked (no `Lifecycle` method ever runs for them, proven with data that would otherwise fail `validate()` or need trimming); and — the trickiest case — `after_insert` still doesn't fire for an `add_mut`'d write that succeeded *within* a transaction later rolled back by a *different*, unhooked write's primary-key collision failing later in the same flush batch. No `_postgres`/`_mysql` counterparts — this is `Session`-level logic with no interaction with which driver is underneath.

`tests/shard.rs` (SQLite) covers `ShardRouter`: a write routed by a given key lands on exactly the shard that key hashes to, and not on any other shard that a differently-hashing key would land on (verified the same marker-row way `replica_set.rs` verifies replica routing, since `ShardRouter` exposes no internal routing state to peek at); the same key always routes to the same shard across repeated calls; `fetch_all_as`/`fetch_one_as`/`session` all route one given key to the same shard as each other, proven by writing through `session()` and reading the write back through a plain `fetch_all_as` with the same key; `shard(index)`/`shards()` expose every underlying `Engine` by 0-based index (and `None` past the end); `ShardRouter::new` rejects an empty shard list; `ShardRouter::new_consistent` routes writes the same correct way `new` does and rejects both an empty shard list and zero virtual nodes per shard. No `_postgres`/`_mysql` counterparts — routing is pure `Session`/`Engine`-agnostic logic (a hash, then a modulo or a ring lookup), already fully exercised against SQLite; which driver backs each shard doesn't change what's being tested. The ring math itself (`build_ring`/`ring_lookup`) is unit-tested directly in `crates/rusty-db-core/src/shard.rs`: a ring lookup finds the first position at or after a hash and wraps around past the last one; `build_ring` produces `virtual_nodes * shard_count` entries in ascending order; and — the actual point of consistent hashing — appending a fifth shard to a four-shard ring remaps well under half of a 2,000-key sample, with every remapped key landing specifically on the new shard rather than being reshuffled among the existing ones, in contrast with naive modulo hashing remapping the vast majority of the same sample under the same before/after shard counts.

`tests/hybrid_properties.rs` (SQLite) covers `#[hybrid(...)]`: the generated plain-Rust method computes the correct value from an instance's own fields; the generated `_expr()` method filters real rows to exactly the same set the Rust-side computation would select, for both a simple `price * quantity` expression and a parenthesized, explicit-`ty` one (`(price * quantity) - discount`); it works as a `.columns()` entry too, with its value matching each row's own Rust-side computation exactly. A `price > 50` comparison hybrid is covered separately: its Rust-side method returns the correct `bool` for both a value under and over the threshold, and its `_expr()` — used directly as a `.filter()` condition, with no further comparison chained onto it — selects real rows matching exactly what the Rust-side `bool` would keep, against sample data deliberately containing both a match and a non-match (so a filter that's silently always-true or always-false would be caught, not just a filter that happens to match everything). The parser/codegen themselves (operator precedence and left-associativity, parenthesized grouping, unsuffixed-literal codegen so a literal adapts to the referenced field's own numeric type, unknown-field and malformed-expression rejection, that the SQL side qualifies by column name rather than Rust field name, every comparison operator tokenizing/parsing/rendering its matching `Expr` method, a comparison's operands accepting arbitrary arithmetic rather than just bare fields, chained comparisons being rejected, a bare `=` never being silently treated as `==`, and a comparison inferring `bool` rather than an operand's own type when `ty` is omitted) are unit-tested directly in `crates/rusty-db-derive/src/lib.rs`.

## Running tests

```
cargo test --workspace --all-features
```
