use super::expr::Expr;
use super::table::Table;
use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct Delete {
    table: Table,
    filter: Option<Expr>,
    returning: Vec<String>,
}

impl Delete {
    pub fn from(table: &Table) -> Self {
        Delete {
            table: table.clone(),
            filter: None,
            returning: Vec::new(),
        }
    }

    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = Some(match self.filter {
            Some(existing) => existing.and(expr),
            None => expr,
        });
        self
    }

    /// Request columns back via `RETURNING` (only honored by dialects where
    /// `Dialect::supports_returning()` is true; ignored otherwise).
    pub fn returning(mut self, columns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.returning = columns.into_iter().map(Into::into).collect();
        self
    }
}

impl ToSql for Delete {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut params = Vec::new();
        let mut sql = format!("DELETE FROM {}", dialect.quote_ident(self.table.name()));

        if let Some(filter) = &self.filter {
            sql.push_str(" WHERE ");
            sql.push_str(&filter.render(dialect, &mut params));
        }

        if !self.returning.is_empty() && dialect.supports_returning() {
            let returning_sql = self
                .returning
                .iter()
                .map(|c| dialect.quote_ident(c))
                .collect::<Vec<_>>()
                .join(", ");
            sql.push_str(" RETURNING ");
            sql.push_str(&returning_sql);
        }

        (sql, params)
    }
}
