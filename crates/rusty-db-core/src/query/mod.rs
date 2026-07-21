mod bulk_insert;
mod cte;
mod delete;
mod expr;
mod insert;
mod join;
mod select;
mod set_op;
mod table;
#[cfg(test)]
mod tests;
mod update;

pub use bulk_insert::BulkInsert;
pub use cte::Cte;
pub use delete::Delete;
pub use expr::{AggFunc, ArithOp, BinOp, Case, Expr, Window};
pub use insert::Insert;
pub use join::{Join, JoinKind};
pub use select::{Select, SelectExpr};
pub use set_op::{SetOp, SetOperation};
pub use table::{Column, Table};
pub use update::Update;

use crate::dialect::Dialect;
use crate::value::Value;

/// Anything that can be rendered to a `(sql, params)` pair for a specific
/// `Dialect`. Implemented by every query builder type (`Select`, `Insert`,
/// `Update`, `Delete`); `Engine`'s convenience methods accept `&dyn ToSql`.
pub trait ToSql {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>);
}

/// Renders one bound value for a `VALUES`/`SET` clause: a literal `NULL`
/// for `Value::Null` (skipping the placeholder and parameter list
/// entirely), or the dialect's placeholder otherwise.
///
/// Binding `NULL` as a placeholder forces the underlying driver to declare
/// *some* concrete parameter type for it (SQLite/MySQL don't mind, but
/// Postgres's strict parameter typing then rejects assigning that type —
/// whatever it happens to be — into a column of a genuinely different
/// type, e.g. a `UUID`/`BOOLEAN`/`JSON` column, with no implicit cast
/// between them). A bare `NULL` literal has no type to conflict with, so
/// this sidesteps the problem for every dialect, not just Postgres.
pub(crate) fn render_value_placeholder(
    value: &Value,
    dialect: &dyn Dialect,
    params: &mut Vec<Value>,
) -> String {
    if matches!(value, Value::Null) {
        "NULL".to_string()
    } else {
        params.push(value.clone());
        dialect.placeholder(params.len())
    }
}
