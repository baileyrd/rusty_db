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

/// `#[derive(Mapped)]`: maps a struct onto a table (see `rusty-db-derive`
/// for the attributes it accepts).
#[cfg(feature = "derive")]
pub use rusty_db_derive::Mapped;

/// `#[derive(MappedEnum)]`: maps a fieldless enum onto a single column
/// (see `rusty-db-derive` for the attributes it accepts).
#[cfg(feature = "derive")]
pub use rusty_db_derive::MappedEnum;

/// `#[derive(MappedNewtype)]`: maps a single-field tuple struct onto
/// whatever `Value` its own field already converts to/from.
#[cfg(feature = "derive")]
pub use rusty_db_derive::MappedNewtype;

#[cfg(feature = "sqlite")]
pub use rusty_db_sqlite as sqlite;

#[cfg(feature = "postgres")]
pub use rusty_db_postgres as postgres;

#[cfg(feature = "mysql")]
pub use rusty_db_mysql as mysql;

/// Re-exports the pieces most programs need in scope.
pub mod prelude {
    pub use rusty_db_core::{
        with_timeout, AuditEntry, AuditOperation, BigDecimal, BulkInsert, Case, Column, ColumnInfo,
        Cte, DatabaseDump, DateTime, Delete, Engine, Entity, Expr, FromRow, Identifiable, Insert,
        Join, JoinKind, Json, Mapped, Migration, Migrator, NaiveDate, NaiveDateTime, NaiveTime,
        PoolConfig, PoolStats, ReplicaSet, Row, Savepoint, Select, SelectExpr, Session,
        SessionQuery, SetOperation, Table, TableDump, TableSchema, ToSql, Transaction, Update, Utc,
        Uuid, Value,
    };

    // `Mapped` above is the trait (type namespace); this is the derive
    // macro of the same name (macro namespace) — no conflict.
    #[cfg(feature = "derive")]
    pub use rusty_db_derive::Mapped;

    #[cfg(feature = "derive")]
    pub use rusty_db_derive::MappedEnum;

    #[cfg(feature = "derive")]
    pub use rusty_db_derive::MappedNewtype;

    #[cfg(feature = "sqlite")]
    pub use rusty_db_sqlite::SqliteDriver;

    #[cfg(feature = "postgres")]
    pub use rusty_db_postgres::PostgresDriver;

    #[cfg(feature = "mysql")]
    pub use rusty_db_mysql::MySqlDriver;
}
