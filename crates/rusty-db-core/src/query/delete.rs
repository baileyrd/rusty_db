use super::expr::Expr;
use super::table::Table;
use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct Delete {
    table: Table,
    filter: Option<Expr>,
}

impl Delete {
    pub fn from(table: &Table) -> Self {
        Delete {
            table: table.clone(),
            filter: None,
        }
    }

    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = Some(match self.filter {
            Some(existing) => existing.and(expr),
            None => expr,
        });
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

        (sql, params)
    }
}
