use super::expr::{Case, Expr};
use super::join::{Join, JoinKind};
use super::set_op::{SetOp, SetOperation};
use super::table::{Column, Table};
use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

/// One item in a `SELECT` column list: a plain `Column`, or an arbitrary
/// `Expr` (an aggregate — `Expr::count_all()`/`Column::sum()`/etc — a raw
/// `Expr::text(...)` fragment, or any other expression), optionally
/// aliased with `AS`.
///
/// `Column` and `Expr` both convert into this via `Into`, so `.columns(...)`
/// accepts either directly — but only one at a time per call, since a
/// single `impl IntoIterator` call needs one concrete item type. Mixing
/// plain columns and expression columns in the same `SELECT` means
/// wrapping the plain ones in `SelectExpr::from(...)` too:
///
/// ```
/// # use rusty_db_core::{Expr, Select, SelectExpr, Table};
/// let orders = Table::new("orders");
/// let query = Select::from(&orders).columns([
///     SelectExpr::from(orders.col("user_id")),
///     SelectExpr::from(orders.col("amount").sum()).alias("total"),
/// ]);
/// ```
#[derive(Debug, Clone)]
pub struct SelectExpr {
    expr: Expr,
    alias: Option<String>,
}

impl SelectExpr {
    pub fn new(expr: Expr) -> Self {
        SelectExpr { expr, alias: None }
    }

    /// `<expr> AS <alias>`.
    pub fn alias(mut self, alias: impl Into<String>) -> Self {
        self.alias = Some(alias.into());
        self
    }

    fn render(&self, dialect: &dyn Dialect, params: &mut Vec<Value>) -> String {
        let rendered = self.expr.render(dialect, params);
        match &self.alias {
            Some(alias) => format!("{rendered} AS {}", dialect.quote_ident(alias)),
            None => rendered,
        }
    }
}

impl From<Column> for SelectExpr {
    fn from(column: Column) -> Self {
        SelectExpr::new(Expr::Column(column))
    }
}

impl From<Expr> for SelectExpr {
    fn from(expr: Expr) -> Self {
        SelectExpr::new(expr)
    }
}

impl From<Case> for SelectExpr {
    fn from(case: Case) -> Self {
        SelectExpr::new(case.into())
    }
}

#[derive(Debug, Clone)]
pub struct Select {
    table: Table,
    columns: Vec<SelectExpr>,
    distinct: bool,
    joins: Vec<Join>,
    filter: Option<Expr>,
    group_by: Vec<Expr>,
    having: Option<Expr>,
    order_by: Vec<(Column, bool)>,
    limit: Option<i64>,
    offset: Option<i64>,
}

impl Select {
    /// `SELECT * FROM table` (add `.columns(...)` to select specific columns).
    pub fn from(table: &Table) -> Self {
        Select {
            table: table.clone(),
            columns: Vec::new(),
            distinct: false,
            joins: Vec::new(),
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    /// Accepts plain `Column`s (`SELECT` as before) or `SelectExpr`s (for
    /// aggregates/arbitrary expressions, optionally aliased) — see
    /// `SelectExpr`'s own doc for how to mix the two in one call.
    pub fn columns<C: Into<SelectExpr>>(mut self, columns: impl IntoIterator<Item = C>) -> Self {
        self.columns = columns.into_iter().map(Into::into).collect();
        self
    }

    /// `SELECT DISTINCT ...` — dedupe rows that are identical across every
    /// selected column.
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    /// `INNER JOIN other ON on`.
    pub fn join(mut self, other: &Table, on: Expr) -> Self {
        self.joins
            .push(Join::new(JoinKind::Inner, other.clone(), on));
        self
    }

    /// `LEFT JOIN other ON on`.
    pub fn left_join(mut self, other: &Table, on: Expr) -> Self {
        self.joins
            .push(Join::new(JoinKind::Left, other.clone(), on));
        self
    }

    /// `RIGHT JOIN other ON on`.
    pub fn right_join(mut self, other: &Table, on: Expr) -> Self {
        self.joins
            .push(Join::new(JoinKind::Right, other.clone(), on));
        self
    }

    /// `FULL JOIN other ON on`.
    pub fn full_join(mut self, other: &Table, on: Expr) -> Self {
        self.joins
            .push(Join::new(JoinKind::Full, other.clone(), on));
        self
    }

    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = Some(match self.filter {
            Some(existing) => existing.and(expr),
            None => expr,
        });
        self
    }

    /// `GROUP BY`. Accepts plain `Column`s (the common case) or arbitrary
    /// `Expr`s, same as `.columns(...)` — mixing the two in one call means
    /// converting the plain ones with `Expr::col(...)` first, for the same
    /// one-concrete-item-type-per-call reason `SelectExpr` documents.
    pub fn group_by<E: Into<Expr>>(mut self, columns: impl IntoIterator<Item = E>) -> Self {
        self.group_by = columns.into_iter().map(Into::into).collect();
        self
    }

    /// `HAVING` — a second filter applied after `GROUP BY` collapses rows,
    /// so (unlike `.filter(...)`'s `WHERE`) it can reference an aggregate
    /// (`orders.col("amount").sum().gt(100)`). Calling this more than once
    /// combines every condition with `AND`, same as repeated `.filter(...)`.
    pub fn having(mut self, expr: Expr) -> Self {
        self.having = Some(match self.having {
            Some(existing) => existing.and(expr),
            None => expr,
        });
        self
    }

    pub fn order_by(mut self, ordering: (Column, bool)) -> Self {
        self.order_by.push(ordering);
        self
    }

    pub fn limit(mut self, limit: i64) -> Self {
        self.limit = Some(limit);
        self
    }

    pub fn offset(mut self, offset: i64) -> Self {
        self.offset = Some(offset);
        self
    }

    /// `self UNION other` — combines both result sets, deduplicating
    /// matching rows (see `.union_all(...)` to keep duplicates). Chain
    /// further `.union`/`.union_all`/`.intersect`/`.except` calls on the
    /// resulting `SetOperation` to combine more than two.
    pub fn union(self, other: Select) -> SetOperation {
        SetOperation::new(self, SetOp::Union, other)
    }

    /// `self UNION ALL other` — like `.union(...)`, but keeps duplicate
    /// rows instead of deduplicating them.
    pub fn union_all(self, other: Select) -> SetOperation {
        SetOperation::new(self, SetOp::UnionAll, other)
    }

    /// `self INTERSECT other` — only rows present in both result sets.
    pub fn intersect(self, other: Select) -> SetOperation {
        SetOperation::new(self, SetOp::Intersect, other)
    }

    /// `self EXCEPT other` — rows in `self`'s result set that aren't also
    /// in `other`'s.
    pub fn except(self, other: Select) -> SetOperation {
        SetOperation::new(self, SetOp::Except, other)
    }
}

impl Select {
    /// Renders this `SELECT`'s SQL, pushing its bind parameters onto an
    /// existing `params` list instead of starting a fresh one — needed by
    /// `SetOperation`, which stitches more than one `Select` into a single
    /// statement and so needs their placeholders numbered sequentially
    /// across all of them (Postgres's `$1, $2, ...` would otherwise
    /// restart from `$1` in every arm, colliding instead of continuing).
    pub(crate) fn render_into(&self, dialect: &dyn Dialect, params: &mut Vec<Value>) -> String {
        let columns_sql = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns
                .iter()
                .map(|c| c.render(dialect, params))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let distinct_sql = if self.distinct { "DISTINCT " } else { "" };
        let mut sql = format!(
            "SELECT {distinct_sql}{columns_sql} FROM {}",
            self.table.as_clause_sql(dialect)
        );

        for join in &self.joins {
            sql.push(' ');
            sql.push_str(join.kind.as_sql());
            sql.push(' ');
            sql.push_str(&join.table.as_clause_sql(dialect));
            sql.push_str(" ON ");
            sql.push_str(&join.on.render(dialect, params));
        }

        if let Some(filter) = &self.filter {
            sql.push_str(" WHERE ");
            sql.push_str(&filter.render(dialect, params));
        }

        if !self.group_by.is_empty() {
            let group_sql = self
                .group_by
                .iter()
                .map(|e| e.render(dialect, params))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(" GROUP BY ");
            sql.push_str(&group_sql);
        }

        if let Some(having) = &self.having {
            sql.push_str(" HAVING ");
            sql.push_str(&having.render(dialect, params));
        }

        if !self.order_by.is_empty() {
            let order_sql = self
                .order_by
                .iter()
                .map(|(col, asc)| {
                    format!(
                        "{} {}",
                        col.qualified_sql(dialect),
                        if *asc { "ASC" } else { "DESC" }
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(" ORDER BY ");
            sql.push_str(&order_sql);
        }

        if let Some(limit) = self.limit {
            sql.push_str(&format!(" LIMIT {limit}"));
        }

        if let Some(offset) = self.offset {
            sql.push_str(&format!(" OFFSET {offset}"));
        }

        sql
    }
}

impl ToSql for Select {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut params = Vec::new();
        let sql = self.render_into(dialect, &mut params);
        (sql, params)
    }
}
