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

## Status

This covers Core (query builder, connections) plus a thin mapping layer (`#[derive(Mapped)]`, joins). Still missing: a real session/unit-of-work layer, relationships/eager-loading, and migrations. Postgres and SQLite are the only drivers; both are implemented but only SQLite is exercised by the test suite in this environment (no live Postgres server available here).

## Running tests

```
cargo test --workspace --all-features
```
