use std::fmt;

/// A dynamically-typed value that can be bound as a query parameter or
/// decoded out of a result row.
///
/// This is the common currency between the query builder, the `Row` type,
/// and every driver crate: each driver is responsible for translating
/// `Value` to and from whatever its underlying database/client library
/// expects, so the rest of rusty_db never needs to know which backend is
/// actually in use.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    Text(String),
    Bytes(Vec<u8>),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Null => write!(f, "NULL"),
            Value::Bool(b) => write!(f, "{b}"),
            Value::I64(i) => write!(f, "{i}"),
            Value::F64(v) => write!(f, "{v}"),
            Value::Text(s) => write!(f, "{s:?}"),
            Value::Bytes(b) => write!(f, "<{} bytes>", b.len()),
        }
    }
}

macro_rules! impl_from {
    ($ty:ty, $variant:ident) => {
        impl From<$ty> for Value {
            fn from(v: $ty) -> Self {
                Value::$variant(v.into())
            }
        }
    };
}

impl_from!(bool, Bool);
impl_from!(i64, I64);
impl_from!(i32, I64);
impl_from!(f64, F64);
impl_from!(String, Text);
impl_from!(Vec<u8>, Bytes);

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::Text(v.to_owned())
    }
}

impl<T> From<Option<T>> for Value
where
    Value: From<T>,
{
    fn from(v: Option<T>) -> Self {
        match v {
            Some(v) => v.into(),
            None => Value::Null,
        }
    }
}

/// Fallible conversions out of a decoded `Value`, used by `Row::get`.
pub trait FromValue: Sized {
    fn from_value(value: &Value) -> Result<Self, String>;
}

impl FromValue for Value {
    fn from_value(value: &Value) -> Result<Self, String> {
        Ok(value.clone())
    }
}

impl FromValue for bool {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bool(b) => Ok(*b),
            Value::I64(i) => Ok(*i != 0),
            other => Err(format!("expected bool, got {other}")),
        }
    }
}

impl FromValue for i64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::I64(i) => Ok(*i),
            other => Err(format!("expected i64, got {other}")),
        }
    }
}

impl FromValue for i32 {
    fn from_value(value: &Value) -> Result<Self, String> {
        i64::from_value(value).map(|v| v as i32)
    }
}

impl FromValue for f64 {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::F64(f) => Ok(*f),
            Value::I64(i) => Ok(*i as f64),
            other => Err(format!("expected f64, got {other}")),
        }
    }
}

impl FromValue for String {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Text(s) => Ok(s.clone()),
            other => Err(format!("expected text, got {other}")),
        }
    }
}

impl FromValue for Vec<u8> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Bytes(b) => Ok(b.clone()),
            other => Err(format!("expected bytes, got {other}")),
        }
    }
}

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}
