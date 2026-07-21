use super::insert::Insert;
use super::table::Table;
use super::{render_value_placeholder, ToSql};
use crate::dialect::Dialect;
use crate::error::{Error, Result};
use crate::value::Value;

/// A multi-row `INSERT INTO table (...) VALUES (...), (...), ...` —
/// one round trip for many rows, instead of one `Insert` (and one round
/// trip) per row.
///
/// Built by combining ordinary single-row `Insert`s — the same ones
/// `Entity::insert()` (and so `#[derive(Mapped)]`) already produces —
/// rather than a separate row-building API, so nothing needs to change
/// about how a single row is described.
#[derive(Debug, Clone)]
pub struct BulkInsert {
    table: Table,
    columns: Vec<String>,
    rows: Vec<Vec<Value>>,
}

impl BulkInsert {
    /// Combines several single-row `Insert`s into one `BulkInsert`.
    /// Returns `Ok(None)` for an empty input (there's no such thing as a
    /// zero-row `INSERT`). Every `Insert` must target the same table and
    /// assign the same columns in the same order — true of any set of
    /// `Insert`s built from the same `#[derive(Mapped)]` type (e.g.
    /// `entities.iter().map(Entity::insert)`); combining `Insert`s from
    /// different types (or with columns assigned in a different order)
    /// is an `Error::QueryBuilder`.
    pub fn combine(inserts: impl IntoIterator<Item = Insert>) -> Result<Option<Self>> {
        let mut inserts = inserts.into_iter();
        let Some(first) = inserts.next() else {
            return Ok(None);
        };

        let (table, first_assignments) = first.into_parts();
        let columns: Vec<String> = first_assignments
            .iter()
            .map(|(column, _)| column.clone())
            .collect();
        let mut rows = vec![values_of(first_assignments)];

        for insert in inserts {
            let (row_table, assignments) = insert.into_parts();
            if row_table.name() != table.name() {
                return Err(Error::QueryBuilder(format!(
                    "BulkInsert::combine: every row must target the same table \
                     (expected {:?}, got {:?})",
                    table.name(),
                    row_table.name()
                )));
            }
            let row_columns: Vec<&str> = assignments
                .iter()
                .map(|(column, _)| column.as_str())
                .collect();
            if row_columns != columns.iter().map(String::as_str).collect::<Vec<_>>() {
                return Err(Error::QueryBuilder(
                    "BulkInsert::combine: every row must assign the same columns, \
                     in the same order"
                        .to_string(),
                ));
            }
            rows.push(values_of(assignments));
        }

        Ok(Some(BulkInsert {
            table,
            columns,
            rows,
        }))
    }

    /// How many rows this statement inserts.
    pub fn row_count(&self) -> usize {
        self.rows.len()
    }
}

fn values_of(assignments: Vec<(String, Value)>) -> Vec<Value> {
    assignments.into_iter().map(|(_, value)| value).collect()
}

impl ToSql for BulkInsert {
    fn to_sql(&self, dialect: &dyn Dialect) -> (String, Vec<Value>) {
        let mut params = Vec::with_capacity(self.columns.len() * self.rows.len());

        let columns_sql = self
            .columns
            .iter()
            .map(|c| dialect.quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");

        let rows_sql = self
            .rows
            .iter()
            .map(|row| {
                let placeholders = row
                    .iter()
                    .map(|value| render_value_placeholder(value, dialect, &mut params))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({placeholders})")
            })
            .collect::<Vec<_>>()
            .join(", ");

        let sql = format!(
            "INSERT INTO {} ({columns_sql}) VALUES {rows_sql}",
            dialect.quote_ident(self.table.name())
        );

        (sql, params)
    }
}
