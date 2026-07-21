# rusty_db

A Rust take on [SQLAlchemy Core](https://docs.sqlalchemy.org/en/20/core/): a single, database-agnostic query builder and connection API that lets you swap the underlying database without touching application code.

## Architecture

```
crates/rusty-db-core      database-agnostic layer: query builder, Row/Value, Driver/Connection traits, Engine
crates/rusty-db-derive    #[derive(Mapped)] proc macro: maps a struct onto a table
crates/rusty-db-sqlite    SQLite Driver impl (wraps sqlx::SqlitePool)
crates/rusty-db-postgres  PostgreSQL Driver impl (wraps sqlx::PgPool)
rusty_db/                 facade crate: re-exports core + feature-gated drivers ("sqlite", "postgres", "derive")
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

The query builder never talks to a database directly — it renders `(String, Vec<Value>)` via `ToSql::to_sql(&dialect)`, and `Engine` hands that off to whichever `Connection` the configured `Driver` produced. `rusty-db-sqlite` and `rusty-db-postgres` both implement this by wrapping `sqlx`, decoding sqlx rows into `Value` based on each column's runtime type.

### Transactions

```rust
let mut txn = engine.begin().await?;
txn.execute("UPDATE accounts SET balance = balance - ? WHERE id = ?", &[100.into(), from_id.into()]).await?;
txn.execute("UPDATE accounts SET balance = balance + ? WHERE id = ?", &[100.into(), to_id.into()]).await?;
txn.commit().await?; // or txn.rollback().await?
```

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

`Engine::session()` (or `Session::new(engine)`) gives you a unit of work: queue writes with `add`/`update`/`delete` (available for any `#[derive(Mapped)]` type — `add` needs the `Entity` impl every mapped type gets, `update`/`delete` need `Identifiable`, which requires a `#[table(primary_key)]` field), then flush them all in a single transaction with `commit()`:

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

If any statement in the batch fails, `commit()` rolls back the whole transaction and leaves the queue untouched, so you can fix the problem and call `commit()` again. This is a unit-of-work, not a full ORM session: there's no identity map and no autoflush, so reads (`engine.fetch_all_as`, etc.) never see writes that are queued but not yet committed — always `commit()` before reading anything you just wrote through a `Session`.

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

## Status

This covers Core (query builder, connections), a thin mapping layer (`#[derive(Mapped)]`, joins, has-many/belongs-to eager loading), a unit-of-work `Session`, and versioned migrations. Still missing: an identity map/autoflush. Postgres and SQLite are the only drivers; both are implemented but only SQLite is exercised by the test suite in this environment (no live Postgres server available here).

## Running tests

```
cargo test --workspace --all-features
```
