mod bulk_insert;
mod delete;
mod expr;
mod insert;
mod join;
mod select;
mod table;
#[cfg(test)]
mod tests;
mod update;

pub use bulk_insert::BulkInsert;
pub use delete::Delete;
pub use expr::{BinOp, Expr};
pub use insert::Insert;
pub use join::{Join, JoinKind};
pub use select::Select;
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
