use crate::error::{ColumnRef, Error, Result};
use crate::value::{FromValue, Value};
use std::sync::Arc;

/// A database-agnostic result row: a named list of columns and their
/// decoded `Value`s. Every driver produces `Row`s in this shape, so code
/// written against `Row` works identically regardless of which backend
/// executed the query.
#[derive(Debug, Clone)]
pub struct Row {
    columns: Arc<[String]>,
    values: Vec<Value>,
}

impl Row {
    pub fn new(columns: Arc<[String]>, values: Vec<Value>) -> Self {
        debug_assert_eq!(columns.len(), values.len());
        Self { columns, values }
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn index_of(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c == name)
    }

    /// Fetch and decode a column by position.
    pub fn get<T: FromValue>(&self, index: usize) -> Result<T> {
        let value = self
            .values
            .get(index)
            .ok_or(Error::ColumnNotFound(ColumnRef::Index(index)))?;
        T::from_value(value).map_err(|e| Error::TypeConversion(ColumnRef::Index(index), e))
    }

    /// Fetch and decode a column by name.
    pub fn get_by_name<T: FromValue>(&self, name: &str) -> Result<T> {
        let index = self
            .index_of(name)
            .ok_or_else(|| Error::ColumnNotFound(ColumnRef::Name(name.to_owned())))?;
        let value = &self.values[index];
        T::from_value(value).map_err(|e| Error::TypeConversion(ColumnRef::Name(name.to_owned()), e))
    }

    /// The raw, undecoded value at a position.
    pub fn value(&self, index: usize) -> Option<&Value> {
        self.values.get(index)
    }
}
