use super::expr::{BinOp, Expr};
use crate::dialect::Dialect;
use crate::value::Value;

/// A reference to a table, used to build queries against it.
#[derive(Debug, Clone)]
pub struct Table {
    name: String,
}

impl Table {
    pub fn new(name: impl Into<String>) -> Self {
        Table { name: name.into() }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Reference a column belonging to this table.
    pub fn col(&self, name: impl Into<String>) -> Column {
        Column {
            table: self.name.clone(),
            name: name.into(),
        }
    }
}

/// A reference to `table.column`, used inside expressions.
#[derive(Debug, Clone)]
pub struct Column {
    table: String,
    name: String,
}

impl Column {
    pub fn table(&self) -> &str {
        &self.table
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn qualified_sql(&self, dialect: &dyn Dialect) -> String {
        format!(
            "{}.{}",
            dialect.quote_ident(&self.table),
            dialect.quote_ident(&self.name)
        )
    }

    fn binop(&self, op: BinOp, value: impl Into<Value>) -> Expr {
        Expr::BinOp(
            Box::new(Expr::Column(self.clone())),
            op,
            Box::new(Expr::Literal(value.into())),
        )
    }

    fn binop_col(&self, op: BinOp, other: &Column) -> Expr {
        Expr::BinOp(
            Box::new(Expr::Column(self.clone())),
            op,
            Box::new(Expr::Column(other.clone())),
        )
    }

    pub fn eq(&self, value: impl Into<Value>) -> Expr {
        self.binop(BinOp::Eq, value)
    }

    /// Compare this column against another column (e.g. a join condition:
    /// `orders.col("user_id").eq_col(&users.col("id"))`).
    pub fn eq_col(&self, other: &Column) -> Expr {
        self.binop_col(BinOp::Eq, other)
    }

    pub fn ne(&self, value: impl Into<Value>) -> Expr {
        self.binop(BinOp::NotEq, value)
    }

    pub fn lt(&self, value: impl Into<Value>) -> Expr {
        self.binop(BinOp::Lt, value)
    }

    pub fn lte(&self, value: impl Into<Value>) -> Expr {
        self.binop(BinOp::LtEq, value)
    }

    pub fn gt(&self, value: impl Into<Value>) -> Expr {
        self.binop(BinOp::Gt, value)
    }

    pub fn gte(&self, value: impl Into<Value>) -> Expr {
        self.binop(BinOp::GtEq, value)
    }

    pub fn like(&self, pattern: impl Into<Value>) -> Expr {
        self.binop(BinOp::Like, pattern)
    }

    /// Case-insensitive `LIKE` — see `Dialect::ilike_operator` for how it
    /// renders per backend.
    pub fn ilike(&self, pattern: impl Into<Value>) -> Expr {
        self.binop(BinOp::ILike, pattern)
    }

    /// `self BETWEEN low AND high` (inclusive of both bounds).
    pub fn between(&self, low: impl Into<Value>, high: impl Into<Value>) -> Expr {
        Expr::Column(self.clone()).between(low, high)
    }

    pub fn is_null(&self) -> Expr {
        Expr::Column(self.clone()).is_null()
    }

    pub fn is_not_null(&self) -> Expr {
        Expr::Column(self.clone()).is_not_null()
    }

    pub fn is_in(&self, values: impl IntoIterator<Item = Value>) -> Expr {
        Expr::Column(self.clone()).is_in(values)
    }

    pub fn asc(&self) -> (Column, bool) {
        (self.clone(), true)
    }

    pub fn desc(&self) -> (Column, bool) {
        (self.clone(), false)
    }
}
