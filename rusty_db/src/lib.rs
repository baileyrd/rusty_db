//! rusty_db: a Rust take on SQLAlchemy Core — a single query-building and
//! connection API that works identically no matter which database is
//! actually running underneath.
//!
//! ```no_run
//! use rusty_db::prelude::*;
//!
//! # #[cfg(feature = "sqlite")]
//! # async fn example() -> rusty_db::Result<()> {
//! let engine = rusty_db::sqlite::SqliteDriver::engine("sqlite::memory:").await?;
//!
//! let users = Table::new("users");
//! let query = Select::from(&users).filter(users.col("active").eq(true));
//! let rows = engine.fetch_all(&query).await?;
//! # Ok(())
//! # }
//! ```
//!
//! Swapping to PostgreSQL means changing one line — constructing the
//! `Engine` from `rusty_db::postgres::PostgresDriver` instead — the query
//! builder code above is untouched.

pub use rusty_db_core::*;

#[cfg(feature = "sqlite")]
pub use rusty_db_sqlite as sqlite;

#[cfg(feature = "postgres")]
pub use rusty_db_postgres as postgres;

/// Re-exports the pieces most programs need in scope.
pub mod prelude {
    pub use rusty_db_core::{
        Column, Delete, Engine, Expr, Insert, Row, Select, Table, ToSql, Update, Value,
    };

    #[cfg(feature = "sqlite")]
    pub use rusty_db_sqlite::SqliteDriver;

    #[cfg(feature = "postgres")]
    pub use rusty_db_postgres::PostgresDriver;
}
