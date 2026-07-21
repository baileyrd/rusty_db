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
        }
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
