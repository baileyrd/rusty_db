//! `rusty-db-core`: the database-agnostic layer of rusty_db.
//!
//! This crate defines everything that does *not* depend on any particular
//! database: a portable query builder (`Table`, `Column`, `Expr`, `Select`,
//! `Insert`, `Update`, `Delete`), a decoded-row type (`Row`, `Value`), and
//! the trait objects (`Driver`, `Connection`, `Executor`) a concrete driver
//! crate (e.g. `rusty-db-sqlite`, `rusty-db-postgres`) must implement.
//!
//! Application code depends only on `Engine` plus the query builder; which
//! database is actually behind it is decided at startup by which `Driver`
//! gets passed to `Engine::new`.

pub mod audit;
pub mod backup;
pub mod connection;
pub mod dialect;
pub mod engine;
pub mod error;
pub mod mapping;
pub mod migration;
pub mod pool;
pub mod query;
pub mod relations;
pub mod replica;
pub mod row;
pub mod schema;
pub mod session;
pub mod timeout;
pub mod value;

pub use audit::{AuditEntry, AuditOperation};
pub use backup::{DatabaseDump, TableDump};
pub use connection::{Connection, Driver, Executor};
pub use dialect::Dialect;
pub use engine::{Engine, Transaction};
pub use error::{Error, Result};
pub use mapping::{Entity, FromRow, Identifiable, Mapped};
pub use migration::{AppliedMigration, Migration, Migrator};
pub use pool::{PoolConfig, PoolMetrics, PoolStats};
pub use query::{
    AggFunc, ArithOp, BulkInsert, Case, Column, Cte, Delete, Expr, Insert, Join, JoinKind, Select,
    SelectExpr, SetOp, SetOperation, Table, ToSql, Update, Window,
};
pub use replica::ReplicaSet;
pub use row::Row;
pub use schema::{
    CheckConstraint, ColumnInfo, ForeignKey, IndexInfo, TableSchema, UniqueConstraint,
};
pub use session::{Savepoint, Session, SessionQuery};
pub use timeout::with_timeout;
pub use value::{FromValue, Value};

/// Re-exported so a `#[derive(Mapped)]` field can be typed `Uuid` without
/// depending on the `uuid` crate directly — this is exactly the type
/// `Value::Uuid` wraps, so the versions can never mismatch.
pub use uuid::Uuid;

/// Re-exported so a `#[derive(Mapped)]` field can be typed `BigDecimal`
/// without depending on the `bigdecimal` crate directly — this is exactly
/// the type `Value::Decimal` wraps, so the versions can never mismatch.
pub use bigdecimal::BigDecimal;

/// `serde_json`'s own `Value` type, re-exported (and renamed, to avoid
/// colliding with this crate's own `Value`) so a `#[derive(Mapped)]` field
/// can be typed `Json` without depending on `serde_json` directly — this
/// is exactly the type `Value::Json` wraps, so the versions can never
/// mismatch.
pub use serde_json::Value as Json;

/// Re-exported so a `#[derive(Mapped)]` field can be typed `NaiveDate`/
/// `NaiveTime`/`NaiveDateTime`/`DateTime<Utc>` without depending on the
/// `chrono` crate directly — these are exactly the types
/// `Value::Date`/`Value::Time`/`Value::DateTime`/`Value::Timestamp` wrap,
/// so the versions can never mismatch.
pub use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};

/// The return type of `Engine::fetch_stream`/`fetch_stream_as`, re-exported
/// so callers can name it without depending on `futures-core` directly.
pub use futures_core::stream::BoxStream;

/// Re-exported so callers can pull `.next()`/etc. off a `BoxStream` from
/// `Engine::fetch_stream`/`fetch_stream_as` without depending on
/// `futures-util` directly.
pub use futures_util::StreamExt;
