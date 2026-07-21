use super::expr::Expr;
use super::table::Table;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinKind {
    Inner,
    Left,
    Right,
    Full,
}

impl JoinKind {
    pub(super) fn as_sql(self) -> &'static str {
        match self {
            JoinKind::Inner => "INNER JOIN",
            JoinKind::Left => "LEFT JOIN",
            JoinKind::Right => "RIGHT JOIN",
            JoinKind::Full => "FULL JOIN",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Join {
    pub(super) kind: JoinKind,
    pub(super) table: Table,
    pub(super) on: Expr,
}

impl Join {
    pub fn new(kind: JoinKind, table: Table, on: Expr) -> Self {
        Join { kind, table, on }
    }
}
