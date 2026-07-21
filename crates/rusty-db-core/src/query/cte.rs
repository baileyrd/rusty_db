use super::select::Select;
use crate::dialect::Dialect;
use crate::value::Value;

/// A CTE's body: a plain `Select`, or — for a recursive CTE — an anchor
/// `Select` combined with a recursive term via `UNION`/`UNION ALL`, where
/// the recursive term references the CTE by its own name (an ordinary
/// `Table::new(name)`) to recurse into it. Not exposed directly; built via
/// `Cte::new`/`Cte::recursive_union`/`Cte::recursive_union_all`.
#[derive(Debug, Clone)]
enum CteBody {
    Select(Box<Select>),
    /// `anchor UNION recursive_term` (`bool` is `true` for `UNION ALL`).
    Union(Box<Select>, bool, Box<Select>),
}

impl CteBody {
    fn render_into(&self, dialect: &dyn Dialect, params: &mut Vec<Value>) -> String {
        match self {
            CteBody::Select(select) => select.render_into(dialect, params),
            CteBody::Union(anchor, all, recursive_term) => {
                let anchor_sql = anchor.render_into(dialect, params);
                let op = if *all { "UNION ALL" } else { "UNION" };
                let recursive_sql = recursive_term.render_into(dialect, params);
                format!("{anchor_sql} {op} {recursive_sql}")
            }
        }
    }
}

/// One `name AS (query)` entry in a `WITH` clause, attached to an outer
/// query via `Select::with`/`.with_recursive`.
///
/// ```
/// # use rusty_db_core::{Cte, Select, Table};
/// let recent = Table::new("orders");
/// let recent_orders = Cte::new(
///     "recent_orders",
///     Select::from(&recent).filter(recent.col("amount").gt(100_i64)),
/// );
/// let cte_ref = Table::new("recent_orders");
/// let query = Select::from(&cte_ref).with([recent_orders]);
/// ```
#[derive(Debug, Clone)]
pub struct Cte {
    name: String,
    body: CteBody,
}

impl Cte {
    /// A plain, non-recursive CTE: `name AS (query)`. Attach it to an
    /// outer query with `Select::with(...)`.
    pub fn new(name: impl Into<String>, query: Select) -> Self {
        Cte {
            name: name.into(),
            body: CteBody::Select(Box::new(query)),
        }
    }

    /// A recursive CTE: `name AS (anchor UNION recursive_term)`.
    /// `recursive_term` recurses by referencing the CTE's own `name` (an
    /// ordinary `Table::new(name)`) in its `FROM`/`JOIN` — no special
    /// self-reference API needed, the same way a correlated subquery just
    /// references the outer table's columns directly. Attach it to an
    /// outer query with `Select::with_recursive(...)`, not `.with(...)`
    /// (`WITH RECURSIVE` is a different keyword from plain `WITH`).
    ///
    /// ```
    /// # use rusty_db_core::{Cte, Select, SelectExpr, Table};
    /// let employees = Table::new("employees");
    /// let org_chart = Table::new("org_chart");
    ///
    /// let anchor = Select::from(&employees)
    ///     .columns([SelectExpr::from(employees.col("id"))])
    ///     .filter(employees.col("manager_id").is_null());
    /// let recursive_term = Select::from(&employees)
    ///     .columns([SelectExpr::from(employees.col("id"))])
    ///     .join(&org_chart, employees.col("manager_id").eq_col(&org_chart.col("id")));
    /// let cte = Cte::recursive_union_all("org_chart", anchor, recursive_term);
    ///
    /// let query = Select::from(&org_chart).with_recursive([cte]);
    /// ```
    pub fn recursive_union_all(
        name: impl Into<String>,
        anchor: Select,
        recursive_term: Select,
    ) -> Self {
        Cte {
            name: name.into(),
            body: CteBody::Union(Box::new(anchor), true, Box::new(recursive_term)),
        }
    }

    /// Like `Cte::recursive_union_all`, but deduplicates rows across the
    /// anchor and every recursive step instead of keeping duplicates.
    pub fn recursive_union(
        name: impl Into<String>,
        anchor: Select,
        recursive_term: Select,
    ) -> Self {
        Cte {
            name: name.into(),
            body: CteBody::Union(Box::new(anchor), false, Box::new(recursive_term)),
        }
    }

    pub(crate) fn render_into(&self, dialect: &dyn Dialect, params: &mut Vec<Value>) -> String {
        format!(
            "{} AS ({})",
            dialect.quote_ident(&self.name),
            self.body.render_into(dialect, params)
        )
    }
}
