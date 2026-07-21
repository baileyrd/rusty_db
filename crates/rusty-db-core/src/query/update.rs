use super::expr::Expr;
use super::table::Table;
use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct Update {
    table: Table,
    assignments: Vec<(String, Value)>,
    filter: Option<Expr>,
}

impl Update {
    pub fn table(table: &Table) -> Self {
        Update {
            table: table.clone(),
            assignments: Vec::new(),
            filter: None,
        }
    }

    pub fn set(mut self, column: impl Into<String>, value: impl Into<Value>) -> Self {
        self.assignments.push((column.into(), value.into()));
        self
    }

    pub fn filter(mut self, expr: Expr) -> Self {
        self.filter = Some(match self.filter {
            Some(existing) => existing.and(expr),
            None => expr,
        });
        self
    }
}

impl ToSql for Update {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut params = Vec::new();

        let set_sql = self
            .assignments
            .iter()
            .map(|(col, value)| {
                params.push(value.clone());
                format!(
                    "{} = {}",
                    dialect.quote_ident(col),
                    dialect.placeholder(params.len())
                )
            })
            .collect::<Vec<_>>()
            .join(", ");

        let mut sql = format!(
            "UPDATE {} SET {set_sql}",
            dialect.quote_ident(self.table.name())
        );

        if let Some(filter) = &self.filter {
            sql.push_str(" WHERE ");
            sql.push_str(&filter.render(dialect, &mut params));
        }

        (sql, params)
    }
}
