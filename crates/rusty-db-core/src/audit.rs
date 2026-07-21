//! Audit logging / change tracking for `Session`: an opt-in, append-only
//! record of every write a session flushes, kept in an ordinary table
//! (`_rusty_db_audit_log` by default) inside the same transaction as the
//! write itself — so an audit entry only ever exists for a change that
//! genuinely took effect; if the transaction rolls back, the audit entry
//! for anything flushed into it rolls back right along with it.
//!
//! This records the rendered SQL statement and its bound parameters
//! (formatted to text) for each write, not a structured before/after
//! diff of column values — a lightweight write-ahead trail (which
//! statement ran, on which table, when), not a full row-history/diffing
//! system.

use crate::value::Value;

/// The kind of write an `AuditEntry` records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditOperation {
    Insert,
    Update,
    Delete,
}

impl AuditOperation {
    pub fn as_str(&self) -> &'static str {
        match self {
            AuditOperation::Insert => "INSERT",
            AuditOperation::Update => "UPDATE",
            AuditOperation::Delete => "DELETE",
        }
    }

    pub(crate) fn parse(s: &str) -> Option<Self> {
        match s {
            "INSERT" => Some(AuditOperation::Insert),
            "UPDATE" => Some(AuditOperation::Update),
            "DELETE" => Some(AuditOperation::Delete),
            _ => None,
        }
    }
}

/// One row of the audit log: a single write a `Session` flushed.
#[derive(Debug, Clone, PartialEq)]
pub struct AuditEntry {
    pub table: String,
    pub operation: AuditOperation,
    /// The exact SQL statement that was executed for this write.
    pub sql: String,
    /// The statement's bound parameters, formatted to text (via
    /// `Value`'s own `Display`) and comma-separated — lossy, but enough
    /// to inspect what a write did without needing to parse the
    /// statement text apart.
    pub params_text: String,
}

pub(crate) fn params_to_text(params: &[Value]) -> String {
    params
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn table_ddl(quoted_table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {quoted_table} (\
            table_name TEXT NOT NULL, \
            operation TEXT NOT NULL, \
            sql_text TEXT NOT NULL, \
            params_text TEXT NOT NULL, \
            recorded_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP\
        )"
    )
}
