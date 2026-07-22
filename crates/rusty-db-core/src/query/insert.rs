use super::table::Table;
use super::{render_insert_value, InsertValue, ToSql};
use crate::dialect::Dialect;
use crate::value::Value;

#[derive(Debug, Clone)]
pub struct Insert {
    table: Table,
    assignments: Vec<(String, InsertValue)>,
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
        self.assignments
            .push((column.into(), InsertValue::Bound(value.into())));
        self
    }

    /// Assigns a column a raw SQL fragment (e.g. `CURRENT_TIMESTAMP`)
    /// inserted verbatim, rather than a bound parameter.
    pub fn raw_value(mut self, column: impl Into<String>, raw_sql: impl Into<String>) -> Self {
        self.assignments
            .push((column.into(), InsertValue::Raw(raw_sql.into())));
        self
    }

    /// Used by `#[table(default = "...")]`: assigns `raw_default` (a raw SQL
    /// fragment) if `value` equals `T::default()`, or binds `value` itself
    /// otherwise. Since Rust has no "unset" field state, this is the only
    /// way to tell "the caller left this at its default" from "the caller
    /// deliberately set it to the same value the type defaults to" — the two
    /// are indistinguishable, so a genuine value equal to `T::default()`
    /// (e.g. an explicit `0`) is treated as "unset" and gets the mapping's
    /// default expression instead of the value itself.
    pub fn maybe_raw_value<T>(
        self,
        column: impl Into<String>,
        raw_default: impl Into<String>,
        value: T,
    ) -> Self
    where
        T: PartialEq + Default + Into<Value>,
    {
        if value == T::default() {
            self.raw_value(column, raw_default)
        } else {
            self.value(column, value)
        }
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
    pub(crate) fn into_parts(self) -> (Table, Vec<(String, InsertValue)>) {
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
            .map(|(_, v)| render_insert_value(v, dialect, &mut params))
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
