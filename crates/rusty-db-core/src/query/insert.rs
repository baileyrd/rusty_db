use super::table::Table;
use super::ToSql;
use crate::dialect::Dialect;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct Insert {
    table: Table,
    assignments: Vec<(String, Value)>,
    returning: Vec<String>,
}

impl Insert {
    pub fn into_table(table: &Table) -> Self {
        Insert {
            table: table.clone(),
            assignments: Vec::new(),
            returning: Vec::new(),
        }
    }

    pub fn value(mut self, column: impl Into<String>, value: impl Into<Value>) -> Self {
        self.assignments.push((column.into(), value.into()));
        self
    }

    /// Request columns back via `RETURNING` (only honored by dialects where
    /// `Dialect::supports_returning()` is true; ignored otherwise).
    pub fn returning(mut self, columns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.returning = columns.into_iter().map(Into::into).collect();
        self
    }

    /// This insert's table and its `(column, value)` assignments, in the
    /// order `.value(...)` was called — used by `BulkInsert::combine` to
    /// merge several single-row `Insert`s into one multi-row statement.
    pub(crate) fn into_parts(self) -> (Table, Vec<(String, Value)>) {
        (self.table, self.assignments)
    }
}

impl ToSql for Insert {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut params = Vec::with_capacity(self.assignments.len());
        let columns_sql = self
            .assignments
            .iter()
            .map(|(c, _)| dialect.quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");

        let placeholders_sql = self
            .assignments
            .iter()
            .map(|(_, v)| {
                params.push(v.clone());
                dialect.placeholder(params.len())
            })
            .collect::<Vec<_>>()
            .join(", ");

        let mut sql = format!(
            "INSERT INTO {} ({columns_sql}) VALUES ({placeholders_sql})",
            dialect.quote_ident(self.table.name())
        );

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
