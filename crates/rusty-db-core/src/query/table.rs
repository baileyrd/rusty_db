use super::expr::{BinOp, Expr};
use crate::dialect::Dialect;
use crate::value::Value;

/// A reference to a table, used to build queries against it.
#[derive(Debug, Clone)]
pub struct Table {
    name: String,
    alias: Option<String>,
}

impl Table {
    pub fn new(name: impl Into<String>) -> Self {
        Table {
            name: name.into(),
            alias: None,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// A second, aliased reference to the same underlying table — for a
    /// self-join, or anywhere the same table needs to appear more than
    /// once in one query. `.col(...)` on the result qualifies columns with
    /// the alias, not the original table name, and `Select`/`join` render
    /// `<table> AS <alias>` for it.
    ///
    /// ```
    /// # use rusty_db_core::Table;
    /// let employees = Table::new("employees");
    /// let managers = employees.alias("managers");
    /// let query = employees.col("manager_id").eq_col(&managers.col("id"));
    /// ```
    pub fn alias(&self, alias: impl Into<String>) -> Self {
        Table {
            name: self.name.clone(),
            alias: Some(alias.into()),
        }
    }

    /// The name queries should qualify this table's columns with: the
    /// alias if one was given, otherwise the table's own name.
    fn qualifier(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }

    /// Reference a column belonging to this table (qualified by its alias,
    /// if it has one).
    pub fn col(&self, name: impl Into<String>) -> Column {
        Column {
            table: self.qualifier().to_string(),
            name: name.into(),
        }
    }

    /// This table as it appears in a `FROM`/`JOIN` clause: just its quoted
    /// name, or `<name> AS <alias>` if it was given one via `.alias(...)`.
    pub(crate) fn as_clause_sql(&self, dialect: &dyn Dialect) -> String {
        match &self.alias {
            Some(alias) => format!(
                "{} AS {}",
                dialect.quote_ident(&self.name),
                dialect.quote_ident(alias)
            ),
            None => dialect.quote_ident(&self.name),
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

    /// `COUNT(self)`.
    pub fn count(&self) -> Expr {
        Expr::Column(self.clone()).count()
    }

    /// `SUM(self)`.
    pub fn sum(&self) -> Expr {
        Expr::Column(self.clone()).sum()
    }

    /// `AVG(self)`.
    pub fn avg(&self) -> Expr {
        Expr::Column(self.clone()).avg()
    }

    /// `MIN(self)`.
    pub fn min(&self) -> Expr {
        Expr::Column(self.clone()).min()
    }

    /// `MAX(self)`.
    pub fn max(&self) -> Expr {
        Expr::Column(self.clone()).max()
    }

    /// `LOWER(self)`.
    pub fn lower(&self) -> Expr {
        Expr::Column(self.clone()).lower()
    }

    /// `UPPER(self)`.
    pub fn upper(&self) -> Expr {
        Expr::Column(self.clone()).upper()
    }

    /// String concatenation — see `Expr::concat`.
    pub fn concat(&self, other: Expr) -> Expr {
        Expr::Column(self.clone()).concat(other)
    }

    /// `self + other`.
    pub fn add(&self, other: Expr) -> Expr {
        Expr::Column(self.clone()).add(other)
    }

    /// `self - other`.
    pub fn sub(&self, other: Expr) -> Expr {
        Expr::Column(self.clone()).sub(other)
    }

    /// `self * other`.
    pub fn mul(&self, other: Expr) -> Expr {
        Expr::Column(self.clone()).mul(other)
    }

    /// `self / other`.
    pub fn div(&self, other: Expr) -> Expr {
        Expr::Column(self.clone()).div(other)
    }

    pub fn asc(&self) -> (Column, bool) {
        (self.clone(), true)
    }

    pub fn desc(&self) -> (Column, bool) {
        (self.clone(), false)
    }
}
