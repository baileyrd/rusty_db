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
}

impl BinOp {
    fn as_sql(self) -> &'static str {
        match self {
            BinOp::Eq => "=",
            BinOp::NotEq => "<>",
            BinOp::Lt => "<",
            BinOp::LtEq => "<=",
            BinOp::Gt => ">",
            BinOp::GtEq => ">=",
            BinOp::Like => "LIKE",
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
}

impl Expr {
    pub fn col(column: Column) -> Self {
        Expr::Column(column)
    }

    pub fn lit(value: impl Into<Value>) -> Self {
        Expr::Literal(value.into())
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
                    op.as_sql(),
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
