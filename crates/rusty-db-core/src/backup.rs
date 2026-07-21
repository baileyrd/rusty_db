//! Logical backup/restore: dump every row of every table (via
//! `list_tables`/`table_schema` for structure and the query builder for
//! the data), and restore by replaying it as deletes-then-inserts inside
//! one transaction.
//!
//! This is a *logical* (row-data) backup, not a physical one — it doesn't
//! use any database-specific backup mechanism (`pg_dump`, SQLite's own
//! backup API, etc), so it's the exact same shape regardless of which
//! backend is behind the `Engine`, and a `DatabaseDump` can be restored
//! into a different engine than the one it was backed up from. It also
//! doesn't know about foreign keys: `restore` deletes and re-inserts each
//! table independently in the dump's own table order, so schemas with
//! cross-table foreign key constraints may need the caller to think about
//! table order (or deferred constraints) themselves.

use crate::value::Value;

/// One table's rows, captured at backup time.
#[derive(Debug, Clone, PartialEq)]
pub struct TableDump {
    pub table: String,
    /// Column names, in the order values in `rows` are stored.
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// A full logical backup: every table `Engine::backup()` found, in
/// `list_tables` order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DatabaseDump {
    pub tables: Vec<TableDump>,
}
