use super::expr::Expr;
use super::join::{Join, JoinKind};
use super::table::{Column, Table};
use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct Select {
    table: Table,
    columns: Vec<Column>,
    distinct: bool,
    joins: Vec<Join>,
    filter: Option<Expr>,
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
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }
    }

    pub fn columns(mut self, columns: impl IntoIterator<Item = Column>) -> Self {
        self.columns = columns.into_iter().collect();
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
}

impl ToSql for Select {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut params = Vec::new();

        let columns_sql = if self.columns.is_empty() {
            "*".to_string()
        } else {
            self.columns
                .iter()
                .map(|c| c.qualified_sql(dialect))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let distinct_sql = if self.distinct { "DISTINCT " } else { "" };
        let mut sql = format!(
            "SELECT {distinct_sql}{columns_sql} FROM {}",
            dialect.quote_ident(self.table.name())
        );

        for join in &self.joins {
            sql.push(' ');
            sql.push_str(join.kind.as_sql());
            sql.push(' ');
            sql.push_str(&dialect.quote_ident(join.table.name()));
            sql.push_str(" ON ");
            sql.push_str(&join.on.render(dialect, &mut params));
        }

        if let Some(filter) = &self.filter {
            sql.push_str(" WHERE ");
            sql.push_str(&filter.render(dialect, &mut params));
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

        (sql, params)
    }
}
