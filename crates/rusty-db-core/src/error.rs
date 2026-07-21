use std::fmt;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("database error: {0}")]
    Database(String),

    #[error("connection error: {0}")]
    Connection(String),

    #[error("no rows returned")]
    RowNotFound,

    #[error("column {0:?} not found in row")]
    ColumnNotFound(ColumnRef),

    #[error("could not convert column {0:?} value: {1}")]
    TypeConversion(ColumnRef, String),

    #[error("query builder error: {0}")]
    QueryBuilder(String),
}

/// Identifies a column either by its position or its name, for error reporting.
#[derive(Debug, Clone)]
pub enum ColumnRef {
    Index(usize),
    Name(String),
}

impl fmt::Display for ColumnRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ColumnRef::Index(i) => write!(f, "#{i}"),
            ColumnRef::Name(n) => write!(f, "{n:?}"),
        }
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
