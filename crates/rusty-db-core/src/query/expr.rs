use super::select::Select;
use super::table::Column;
use crate::dialect::Dialect;
use crate::value::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    Like,
    /// Case-insensitive `LIKE` — see `Dialect::ilike_operator` for how it
    /// renders per backend.
    ILike,
}

impl BinOp {
    fn as_sql(self, dialect: &dyn Dialect) -> &'static str {
        match self {
            BinOp::Eq => "=",
            BinOp::NotEq => "<>",
            BinOp::Lt => "<",
            BinOp::LtEq => "<=",
            BinOp::Gt => ">",
            BinOp::GtEq => ">=",
            BinOp::Like => "LIKE",
            BinOp::ILike => dialect.ilike_operator(),
        }
    }
}

/// A SQL aggregate function — `COUNT`/`SUM`/`AVG`/`MIN`/`MAX`, all
/// ANSI-standard and identical across every dialect this crate supports,
/// so (unlike `BinOp::ILike`) rendering these needs no `Dialect` hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFunc {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

impl AggFunc {
    fn as_sql(self) -> &'static str {
        match self {
            AggFunc::Count => "COUNT",
            AggFunc::Sum => "SUM",
            AggFunc::Avg => "AVG",
            AggFunc::Min => "MIN",
            AggFunc::Max => "MAX",
        }
    }
}

/// `+`/`-`/`*`/`/`, all ANSI-standard and identical across every dialect
/// this crate supports, so (like `AggFunc`, unlike `BinOp::ILike`)
/// rendering these needs no `Dialect` hook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

impl ArithOp {
    fn as_sql(self) -> &'static str {
        match self {
            ArithOp::Add => "+",
            ArithOp::Sub => "-",
            ArithOp::Mul => "*",
            ArithOp::Div => "/",
        }
    }
}

/// A boolean/scalar SQL expression tree, built up from `Column`s and
/// literal values, rendered to portable SQL by a `Dialect`.
#[derive(Debug, Clone)]
pub enum Expr {
    Column(Column),
    Literal(Value),
    BinOp(Box<Expr>, BinOp, Box<Expr>),
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
    IsNull(Box<Expr>),
    IsNotNull(Box<Expr>),
    In(Box<Expr>, Vec<Expr>),
    /// `expr BETWEEN low AND high` (inclusive of both bounds).
    Between(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A raw SQL fragment inserted verbatim, with `?`-style placeholders
    /// rewritten to the target dialect's actual placeholder syntax in
    /// bind-parameter order. See `text()`.
    Raw(String, Vec<Value>),
    /// `COUNT(*)` — the one aggregate with a wildcard form; every other
    /// `AggFunc` always takes an argument, hence the separate `Agg`
    /// variant below rather than folding this into it as `Agg(_, None)`.
    CountAll,
    /// `COUNT(expr)`/`SUM(expr)`/`AVG(expr)`/`MIN(expr)`/`MAX(expr)`.
    Agg(AggFunc, Box<Expr>),
    /// `LOWER(expr)`.
    Lower(Box<Expr>),
    /// `UPPER(expr)`.
    Upper(Box<Expr>),
    /// String concatenation. See `Dialect::concat_uses_double_pipe` for
    /// why this needs a per-dialect rendering choice, unlike the rest of
    /// this crate's ANSI-standard function/operator variants.
    Concat(Box<Expr>, Box<Expr>),
    /// `expr1 <op> expr2` (`+`/`-`/`*`/`/`).
    Arith(Box<Expr>, ArithOp, Box<Expr>),
    /// `CURRENT_TIMESTAMP` — identical on every dialect this crate
    /// supports (`func.now()`'s counterpart).
    Now,
    /// `COALESCE(expr1, expr2, ...)` — the first non-null value.
    Coalesce(Vec<Expr>),
    /// `CASE WHEN cond1 THEN result1 [WHEN cond2 THEN result2 ...] [ELSE
    /// otherwise] END`. Built via `Case`, not constructed directly.
    Case(Vec<(Expr, Expr)>, Option<Box<Expr>>),
    /// `expr IN (<subquery>)`. See `.in_subquery(...)`.
    InSubquery(Box<Expr>, Box<Select>),
    /// `EXISTS (<subquery>)`. A correlated subquery just means the nested
    /// `Select`'s filter references a `Column` from the outer query's
    /// table — that already renders correctly with no special support,
    /// since `Column` qualifies itself by table name regardless of which
    /// `Select` it's built into. See `Expr::exists(...)`.
    Exists(Box<Select>),
    /// A parenthesized scalar subquery, usable anywhere an ordinary `Expr`
    /// is (a `SELECT` column, either side of a comparison, ...). The
    /// caller is responsible for the subquery returning exactly one column
    /// and (at most) one row — nothing here enforces that. See
    /// `Expr::subquery(...)`.
    Subquery(Box<Select>),
    /// `ROW_NUMBER()` — a 1-based row number within its window, reset at
    /// the start of each partition. Only meaningful behind `.over(...)`
    /// (see `Window`); has no ordinary (non-windowed) form.
    RowNumber,
    /// `RANK()` — like `ROW_NUMBER()`, but rows tying on the window's
    /// `ORDER BY` get the same rank, and the rank after a tie skips ahead
    /// by the tie's size (1, 2, 2, 4, ...). Only meaningful behind
    /// `.over(...)`.
    Rank,
    /// `DENSE_RANK()` — like `RANK()`, but never skips a rank after a tie
    /// (1, 2, 2, 3, ...). Only meaningful behind `.over(...)`.
    DenseRank,
    /// `function OVER (PARTITION BY ... ORDER BY ...)`. Built via
    /// `Window`, not constructed directly.
    Window(Box<Expr>, Vec<Expr>, Vec<(Column, bool)>),
}

impl From<Column> for Expr {
    fn from(column: Column) -> Self {
        Expr::Column(column)
    }
}

impl Expr {
    pub fn col(column: Column) -> Self {
        Expr::Column(column)
    }

    pub fn lit(value: impl Into<Value>) -> Self {
        Expr::Literal(value.into())
    }

    /// A raw SQL fragment that composes into the builder like any other
    /// `Expr` — the escape hatch for things the builder doesn't model yet
    /// (a database-specific function, a fragment too complex to build with
    /// `Expr`, ...) without dropping to `Engine::connect()`/
    /// `Transaction::execute` and losing composability with `Select`'s
    /// other clauses (`.filter(...)`, join conditions, `.and`/`.or` with
    /// ordinary `Expr`s, ...).
    ///
    /// Write `sql` in whichever dialect you're actually targeting — it's
    /// inserted verbatim, so it isn't portable across backends unless you
    /// keep it portable yourself. Bind parameters are written with `?`
    /// placeholders regardless of the target dialect; each one is rewritten
    /// to that dialect's real placeholder syntax (`$1`, `?`, ...) in the
    /// order they appear, matched up with `params` in the same order — so
    /// avoid a literal `?` character elsewhere in the fragment (e.g. inside
    /// a quoted string), since this doesn't parse SQL, it just scans for
    /// `?` characters.
    ///
    /// ```
    /// # use rusty_db_core::{Expr, Select, Table, Value};
    /// let users = Table::new("users");
    /// let query = Select::from(&users)
    ///     .filter(Expr::text("lower(name) = ?", [Value::Text("ada".to_string())]));
    /// ```
    pub fn text(sql: impl Into<String>, params: impl IntoIterator<Item = Value>) -> Self {
        Expr::Raw(sql.into(), params.into_iter().collect())
    }

    fn binop(self, op: BinOp, value: impl Into<Value>) -> Self {
        Expr::BinOp(Box::new(self), op, Box::new(Expr::Literal(value.into())))
    }

    /// `self = value`. `Column`'s own `.eq`/etc. build the same `Expr` for
    /// the common case of comparing a column directly; these exist on
    /// `Expr` itself too so an arbitrary expression — an aggregate for a
    /// `HAVING` clause (`orders.col("amount").sum().gt(100)`), an
    /// `Expr::text(...)` fragment, or anything else — can be compared the
    /// same way.
    pub fn eq(self, value: impl Into<Value>) -> Self {
        self.binop(BinOp::Eq, value)
    }

    pub fn ne(self, value: impl Into<Value>) -> Self {
        self.binop(BinOp::NotEq, value)
    }

    pub fn lt(self, value: impl Into<Value>) -> Self {
        self.binop(BinOp::Lt, value)
    }

    pub fn lte(self, value: impl Into<Value>) -> Self {
        self.binop(BinOp::LtEq, value)
    }

    pub fn gt(self, value: impl Into<Value>) -> Self {
        self.binop(BinOp::Gt, value)
    }

    pub fn gte(self, value: impl Into<Value>) -> Self {
        self.binop(BinOp::GtEq, value)
    }

    pub fn like(self, pattern: impl Into<Value>) -> Self {
        self.binop(BinOp::Like, pattern)
    }

    /// Case-insensitive `LIKE` — see `Dialect::ilike_operator` for how it
    /// renders per backend.
    pub fn ilike(self, pattern: impl Into<Value>) -> Self {
        self.binop(BinOp::ILike, pattern)
    }

    fn binop_expr(self, op: BinOp, other: Expr) -> Self {
        Expr::BinOp(Box::new(self), op, Box::new(other))
    }

    /// `self = other`, comparing this expression against another
    /// expression — `Expr::now()`, an aggregate, another column, ... —
    /// rather than a literal value (see `.eq(...)` for that) or
    /// specifically another column (see `Column::eq_col`, which this
    /// generalizes to any `Expr` on either side).
    pub fn eq_expr(self, other: Expr) -> Self {
        self.binop_expr(BinOp::Eq, other)
    }

    pub fn ne_expr(self, other: Expr) -> Self {
        self.binop_expr(BinOp::NotEq, other)
    }

    pub fn lt_expr(self, other: Expr) -> Self {
        self.binop_expr(BinOp::Lt, other)
    }

    pub fn lte_expr(self, other: Expr) -> Self {
        self.binop_expr(BinOp::LtEq, other)
    }

    pub fn gt_expr(self, other: Expr) -> Self {
        self.binop_expr(BinOp::Gt, other)
    }

    pub fn gte_expr(self, other: Expr) -> Self {
        self.binop_expr(BinOp::GtEq, other)
    }

    pub fn and(self, other: Expr) -> Self {
        match self {
            Expr::And(mut clauses) => {
                clauses.push(other);
                Expr::And(clauses)
            }
            lhs => Expr::And(vec![lhs, other]),
        }
    }

    pub fn or(self, other: Expr) -> Self {
        match self {
            Expr::Or(mut clauses) => {
                clauses.push(other);
                Expr::Or(clauses)
            }
            lhs => Expr::Or(vec![lhs, other]),
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn not(self) -> Self {
        Expr::Not(Box::new(self))
    }

    pub fn is_null(self) -> Self {
        Expr::IsNull(Box::new(self))
    }

    pub fn is_not_null(self) -> Self {
        Expr::IsNotNull(Box::new(self))
    }

    pub fn is_in(self, values: impl IntoIterator<Item = Value>) -> Self {
        Expr::In(
            Box::new(self),
            values.into_iter().map(Expr::Literal).collect(),
        )
    }

    /// `self BETWEEN low AND high` (inclusive of both bounds).
    pub fn between(self, low: impl Into<Value>, high: impl Into<Value>) -> Self {
        Expr::Between(
            Box::new(self),
            Box::new(Expr::Literal(low.into())),
            Box::new(Expr::Literal(high.into())),
        )
    }

    /// `self IN (<subquery>)` — like `.is_in(...)`, but against the result
    /// of a nested `Select` instead of a fixed list of values.
    ///
    /// ```
    /// # use rusty_db_core::{Expr, Select, Table};
    /// let orders = Table::new("orders");
    /// let users = Table::new("users");
    /// let big_spenders = Select::from(&orders)
    ///     .columns([orders.col("user_id")])
    ///     .filter(orders.col("amount").gt(100_i64));
    /// let query = Select::from(&users).filter(users.col("id").in_subquery(big_spenders));
    /// ```
    pub fn in_subquery(self, subquery: Select) -> Self {
        Expr::InSubquery(Box::new(self), Box::new(subquery))
    }

    /// `EXISTS (<subquery>)`. Correlate it with the outer query by
    /// referencing the outer table's columns in the subquery's own
    /// `.filter(...)` — see the module-level doc on `Expr::Exists`.
    ///
    /// ```
    /// # use rusty_db_core::{Expr, Select, Table};
    /// let orders = Table::new("orders");
    /// let users = Table::new("users");
    /// let has_orders = Select::from(&orders).filter(orders.col("user_id").eq_col(&users.col("id")));
    /// let query = Select::from(&users).filter(Expr::exists(has_orders));
    /// ```
    pub fn exists(subquery: Select) -> Self {
        Expr::Exists(Box::new(subquery))
    }

    /// A scalar subquery — `(<subquery>)` — usable as a `SELECT` column or
    /// on either side of a comparison. See `Expr::Subquery` for the
    /// one-column/at-most-one-row caveat.
    ///
    /// ```
    /// # use rusty_db_core::{Expr, Select, SelectExpr, Table};
    /// let orders = Table::new("orders");
    /// let users = Table::new("users");
    /// let order_count = Select::from(&orders)
    ///     .columns([SelectExpr::from(Expr::count_all())])
    ///     .filter(orders.col("user_id").eq_col(&users.col("id")));
    /// let query = Select::from(&users)
    ///     .columns([SelectExpr::from(Expr::subquery(order_count)).alias("order_count")]);
    /// ```
    pub fn subquery(subquery: Select) -> Self {
        Expr::Subquery(Box::new(subquery))
    }

    /// `COUNT(*)`. An associated function rather than a method on `Expr`
    /// like the other aggregates below, since `COUNT(*)` has no inner
    /// expression to call it on.
    pub fn count_all() -> Self {
        Expr::CountAll
    }

    /// `COUNT(self)`.
    pub fn count(self) -> Self {
        Expr::Agg(AggFunc::Count, Box::new(self))
    }

    /// `SUM(self)`.
    pub fn sum(self) -> Self {
        Expr::Agg(AggFunc::Sum, Box::new(self))
    }

    /// `AVG(self)`.
    pub fn avg(self) -> Self {
        Expr::Agg(AggFunc::Avg, Box::new(self))
    }

    /// `MIN(self)`.
    pub fn min(self) -> Self {
        Expr::Agg(AggFunc::Min, Box::new(self))
    }

    /// `MAX(self)`.
    pub fn max(self) -> Self {
        Expr::Agg(AggFunc::Max, Box::new(self))
    }

    /// `LOWER(self)`.
    pub fn lower(self) -> Self {
        Expr::Lower(Box::new(self))
    }

    /// `UPPER(self)`.
    pub fn upper(self) -> Self {
        Expr::Upper(Box::new(self))
    }

    /// String concatenation (`self || other` on Postgres/SQLite,
    /// `CONCAT(self, other)` on MySQL/MariaDB — see
    /// `Dialect::concat_uses_double_pipe`).
    pub fn concat(self, other: Expr) -> Self {
        Expr::Concat(Box::new(self), Box::new(other))
    }

    fn arith(self, op: ArithOp, other: Expr) -> Self {
        Expr::Arith(Box::new(self), op, Box::new(other))
    }

    /// `self + other`.
    #[allow(clippy::should_implement_trait)]
    pub fn add(self, other: Expr) -> Self {
        self.arith(ArithOp::Add, other)
    }

    /// `self - other`.
    #[allow(clippy::should_implement_trait)]
    pub fn sub(self, other: Expr) -> Self {
        self.arith(ArithOp::Sub, other)
    }

    /// `self * other`.
    #[allow(clippy::should_implement_trait)]
    pub fn mul(self, other: Expr) -> Self {
        self.arith(ArithOp::Mul, other)
    }

    /// `self / other`.
    #[allow(clippy::should_implement_trait)]
    pub fn div(self, other: Expr) -> Self {
        self.arith(ArithOp::Div, other)
    }

    /// `CURRENT_TIMESTAMP`.
    pub fn now() -> Self {
        Expr::Now
    }

    /// `COALESCE(exprs[0], exprs[1], ...)` — the first non-null value.
    pub fn coalesce(exprs: impl IntoIterator<Item = Expr>) -> Self {
        Expr::Coalesce(exprs.into_iter().collect())
    }

    /// `ROW_NUMBER()` — pair with `.over(...)` (see `Window`) to actually
    /// use it; on its own it isn't valid SQL.
    pub fn row_number() -> Self {
        Expr::RowNumber
    }

    /// `RANK()` — see `Expr::row_number` for the same "needs `.over(...)`"
    /// caveat.
    pub fn rank() -> Self {
        Expr::Rank
    }

    /// `DENSE_RANK()` — see `Expr::row_number` for the same "needs
    /// `.over(...)`" caveat.
    pub fn dense_rank() -> Self {
        Expr::DenseRank
    }

    /// Turns this expression (a ranking function like `Expr::row_number()`,
    /// or an ordinary aggregate like `.sum()`) into a window function:
    /// `self OVER (PARTITION BY ... ORDER BY ...)`. See `Window`.
    pub fn over(self, window: Window) -> Self {
        Expr::Window(Box::new(self), window.partition_by, window.order_by)
    }

    /// Render this expression to SQL, pushing any literal values into
    /// `params` in the order their placeholders appear.
    pub fn render(&self, dialect: &dyn Dialect, params: &mut Vec<Value>) -> String {
        match self {
            Expr::Column(c) => c.qualified_sql(dialect),
            Expr::Literal(v) => {
                params.push(v.clone());
                dialect.placeholder(params.len())
            }
            Expr::BinOp(lhs, op, rhs) => {
                format!(
                    "{} {} {}",
                    lhs.render(dialect, params),
                    op.as_sql(dialect),
                    rhs.render(dialect, params)
                )
            }
            Expr::And(clauses) => render_bool_chain(clauses, "AND", dialect, params),
            Expr::Or(clauses) => render_bool_chain(clauses, "OR", dialect, params),
            Expr::Not(inner) => format!("NOT ({})", inner.render(dialect, params)),
            Expr::IsNull(inner) => format!("{} IS NULL", inner.render(dialect, params)),
            Expr::IsNotNull(inner) => format!("{} IS NOT NULL", inner.render(dialect, params)),
            Expr::In(inner, values) => {
                let lhs = inner.render(dialect, params);
                if values.is_empty() {
                    // `x IN ()` is invalid SQL; this is always false.
                    return "1 = 0".to_string();
                }
                let rendered: Vec<String> =
                    values.iter().map(|v| v.render(dialect, params)).collect();
                format!("{lhs} IN ({})", rendered.join(", "))
            }
            Expr::Between(target, low, high) => {
                format!(
                    "{} BETWEEN {} AND {}",
                    target.render(dialect, params),
                    low.render(dialect, params),
                    high.render(dialect, params)
                )
            }
            Expr::Raw(sql, values) => {
                let mut rendered = String::with_capacity(sql.len());
                let mut values = values.iter();
                for ch in sql.chars() {
                    if ch == '?' {
                        if let Some(value) = values.next() {
                            params.push(value.clone());
                            rendered.push_str(&dialect.placeholder(params.len()));
                            continue;
                        }
                    }
                    rendered.push(ch);
                }
                rendered
            }
            Expr::CountAll => "COUNT(*)".to_string(),
            Expr::Agg(func, inner) => {
                format!("{}({})", func.as_sql(), inner.render(dialect, params))
            }
            Expr::Lower(inner) => format!("LOWER({})", inner.render(dialect, params)),
            Expr::Upper(inner) => format!("UPPER({})", inner.render(dialect, params)),
            Expr::Concat(lhs, rhs) => {
                let lhs = lhs.render(dialect, params);
                let rhs = rhs.render(dialect, params);
                if dialect.concat_uses_double_pipe() {
                    format!("{lhs} || {rhs}")
                } else {
                    format!("CONCAT({lhs}, {rhs})")
                }
            }
            Expr::Arith(lhs, op, rhs) => {
                format!(
                    "{} {} {}",
                    lhs.render(dialect, params),
                    op.as_sql(),
                    rhs.render(dialect, params)
                )
            }
            Expr::Now => "CURRENT_TIMESTAMP".to_string(),
            Expr::Coalesce(exprs) => {
                let rendered: Vec<String> =
                    exprs.iter().map(|e| e.render(dialect, params)).collect();
                format!("COALESCE({})", rendered.join(", "))
            }
            Expr::Case(arms, otherwise) => {
                let mut sql = "CASE".to_string();
                for (condition, then) in arms {
                    sql.push_str(" WHEN ");
                    sql.push_str(&condition.render(dialect, params));
                    sql.push_str(" THEN ");
                    sql.push_str(&then.render(dialect, params));
                }
                if let Some(otherwise) = otherwise {
                    sql.push_str(" ELSE ");
                    sql.push_str(&otherwise.render(dialect, params));
                }
                sql.push_str(" END");
                sql
            }
            Expr::InSubquery(inner, subquery) => {
                let lhs = inner.render(dialect, params);
                let subquery_sql = subquery.render_into(dialect, params);
                format!("{lhs} IN ({subquery_sql})")
            }
            Expr::Exists(subquery) => {
                format!("EXISTS ({})", subquery.render_into(dialect, params))
            }
            Expr::Subquery(subquery) => {
                format!("({})", subquery.render_into(dialect, params))
            }
            Expr::RowNumber => "ROW_NUMBER()".to_string(),
            Expr::Rank => "RANK()".to_string(),
            Expr::DenseRank => "DENSE_RANK()".to_string(),
            Expr::Window(function, partition_by, order_by) => {
                let function_sql = function.render(dialect, params);
                let mut over = String::new();
                if !partition_by.is_empty() {
                    over.push_str("PARTITION BY ");
                    over.push_str(
                        &partition_by
                            .iter()
                            .map(|e| e.render(dialect, params))
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                if !order_by.is_empty() {
                    if !over.is_empty() {
                        over.push(' ');
                    }
                    over.push_str("ORDER BY ");
                    over.push_str(
                        &order_by
                            .iter()
                            .map(|(col, asc)| {
                                format!(
                                    "{} {}",
                                    col.qualified_sql(dialect),
                                    if *asc { "ASC" } else { "DESC" }
                                )
                            })
                            .collect::<Vec<_>>()
                            .join(", "),
                    );
                }
                format!("{function_sql} OVER ({over})")
            }
        }
    }
}

/// A `CASE WHEN ... THEN ... [ELSE ...] END` expression, built up one arm
/// at a time and converted into an `Expr` via `Into`/`From`.
///
/// ```
/// # use rusty_db_core::{Case, Expr, Select, SelectExpr, Table};
/// let orders = Table::new("orders");
/// let tier = Case::new()
///     .when(orders.col("amount").gt(100_i64), Expr::lit("gold"))
///     .when(orders.col("amount").gt(50_i64), Expr::lit("silver"))
///     .otherwise(Expr::lit("bronze"));
/// let query = Select::from(&orders).columns([SelectExpr::from(tier).alias("tier")]);
/// ```
#[derive(Debug, Clone, Default)]
pub struct Case {
    arms: Vec<(Expr, Expr)>,
    otherwise: Option<Box<Expr>>,
}

impl Case {
    pub fn new() -> Self {
        Case::default()
    }

    /// `WHEN condition THEN then`.
    pub fn when(mut self, condition: Expr, then: Expr) -> Self {
        self.arms.push((condition, then));
        self
    }

    /// `ELSE otherwise`.
    pub fn otherwise(mut self, otherwise: Expr) -> Self {
        self.otherwise = Some(Box::new(otherwise));
        self
    }
}

impl From<Case> for Expr {
    fn from(case: Case) -> Self {
        Expr::Case(case.arms, case.otherwise)
    }
}

/// A window's `PARTITION BY`/`ORDER BY` clause, attached to a ranking
/// function (`Expr::row_number()`/`.rank()`/`.dense_rank()`) or an
/// ordinary aggregate (`.sum()`/`.count()`/etc.) via `.over(...)`:
///
/// ```
/// # use rusty_db_core::{Expr, Select, SelectExpr, Table, Window};
/// let orders = Table::new("orders");
/// let running_total = orders
///     .col("amount")
///     .sum()
///     .over(Window::new().partition_by([orders.col("customer")]).order_by(orders.col("id").asc()));
/// let query = Select::from(&orders).columns([SelectExpr::from(running_total).alias("running_total")]);
/// ```
///
/// Both clauses are optional and independent — `PARTITION BY` alone splits
/// rows into groups without ordering them; `ORDER BY` alone treats the
/// whole result as one partition, ordered; neither (`Window::new()`, or
/// equivalently `.over(Window::new())`) means one unordered partition
/// covering every row, rendering as the still-valid `OVER ()`.
#[derive(Debug, Clone, Default)]
pub struct Window {
    partition_by: Vec<Expr>,
    order_by: Vec<(Column, bool)>,
}

impl Window {
    pub fn new() -> Self {
        Window::default()
    }

    /// `PARTITION BY ...` — splits rows into groups the window function is
    /// computed independently within, the same `Column`/`Expr` mix
    /// `Select::columns`/`.group_by` accept.
    pub fn partition_by<E: Into<Expr>>(mut self, columns: impl IntoIterator<Item = E>) -> Self {
        self.partition_by = columns.into_iter().map(Into::into).collect();
        self
    }

    /// `ORDER BY ...` within the window — determines row order for
    /// `ROW_NUMBER`/`RANK`/`DENSE_RANK`, and which rows are "so far" for a
    /// running aggregate. Calling this more than once adds more `ORDER BY`
    /// columns, the same as repeated `Select::order_by`.
    pub fn order_by(mut self, ordering: (Column, bool)) -> Self {
        self.order_by.push(ordering);
        self
    }
}

fn render_bool_chain(
    clauses: &[Expr],
    joiner: &str,
    dialect: &dyn Dialect,
    params: &mut Vec<Value>,
) -> String {
    if clauses.is_empty() {
        return "1 = 1".to_string();
    }
    let rendered: Vec<String> = clauses
        .iter()
        .map(|c| format!("({})", c.render(dialect, params)))
        .collect();
    rendered.join(&format!(" {joiner} "))
}
