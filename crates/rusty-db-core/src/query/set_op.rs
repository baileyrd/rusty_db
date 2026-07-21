use super::select::Select;
use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

/// A SQL set operator combining two `SELECT`s' result sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    Union,
    UnionAll,
    Intersect,
    Except,
}

impl SetOp {
    fn as_sql(self) -> &'static str {
        match self {
            SetOp::Union => "UNION",
            SetOp::UnionAll => "UNION ALL",
            SetOp::Intersect => "INTERSECT",
            SetOp::Except => "EXCEPT",
        }
    }
}

/// Two or more `Select`s combined with `UNION`/`UNION ALL`/`INTERSECT`/
/// `EXCEPT` — built via `Select::union`/`union_all`/`intersect`/`except`,
/// and chainable the same way to combine more than two:
///
/// ```
/// # use rusty_db_core::{Select, Table};
/// let active = Table::new("active_users");
/// let archived = Table::new("archived_users");
/// let pending = Table::new("pending_users");
///
/// let query = Select::from(&active)
///     .union(Select::from(&archived))
///     .union_all(Select::from(&pending));
/// ```
///
/// Every dialect this crate supports has `UNION`/`UNION ALL`; `INTERSECT`/
/// `EXCEPT` are ANSI-standard too and supported by Postgres and modern
/// MySQL/MariaDB (MySQL 8.0.31+, MariaDB 10.3+) — an older server without
/// them surfaces a plain SQL syntax error from the database itself, since
/// there's no reasonable fallback rendering for either that would still
/// mean the same thing.
///
/// Not modeled here (out of scope for now): giving the combined result its
/// own `ORDER BY`/`LIMIT`/`OFFSET` distinct from any individual arm's —
/// only each arm's own `Select` methods are available. Relatedly, an arm
/// with its own `ORDER BY`/`LIMIT` is portable on Postgres/MySQL but not
/// SQLite, whose grammar only allows a trailing `ORDER BY`/`LIMIT` on the
/// compound statement as a whole, not on an individual arm.
#[derive(Debug, Clone)]
pub struct SetOperation {
    first: Select,
    rest: Vec<(SetOp, Select)>,
}

impl SetOperation {
    pub(crate) fn new(first: Select, op: SetOp, next: Select) -> Self {
        SetOperation {
            first,
            rest: vec![(op, next)],
        }
    }

    /// `... UNION next`.
    pub fn union(mut self, next: Select) -> Self {
        self.rest.push((SetOp::Union, next));
        self
    }

    /// `... UNION ALL next`.
    pub fn union_all(mut self, next: Select) -> Self {
        self.rest.push((SetOp::UnionAll, next));
        self
    }

    /// `... INTERSECT next`.
    pub fn intersect(mut self, next: Select) -> Self {
        self.rest.push((SetOp::Intersect, next));
        self
    }

    /// `... EXCEPT next`.
    pub fn except(mut self, next: Select) -> Self {
        self.rest.push((SetOp::Except, next));
        self
    }
}

impl ToSql for SetOperation {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut params = Vec::new();
        let mut sql = self.first.render_into(dialect, &mut params);
        for (op, select) in &self.rest {
            sql.push(' ');
            sql.push_str(op.as_sql());
            sql.push(' ');
            sql.push_str(&select.render_into(dialect, &mut params));
        }
        (sql, params)
    }
}
