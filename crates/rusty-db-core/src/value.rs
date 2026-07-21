use std::fmt;
use std::str::FromStr;

use bigdecimal::BigDecimal;
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use serde_json::Value as Json;
use uuid::Uuid;

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
    /// A UUID. Postgres has a native `UUID` column type and round-trips
    /// this variant directly; MySQL/MariaDB and SQLite have no such type
    /// (a UUID column there is really just `CHAR(36)`/`TEXT`), so those
    /// drivers bind this as its hyphenated string form and decode UUID
    /// columns back as `Value::Text` — `FromValue for Uuid` parses that
    /// text form too, so a mapped struct's `Uuid` field still round-trips
    /// correctly everywhere, just without Postgres's native wire format.
    Uuid(Uuid),
    /// An arbitrary-precision decimal (a `NUMERIC`/`DECIMAL` column).
    /// Postgres has a native `NUMERIC` type and round-trips this variant
    /// directly; MySQL/MariaDB sends `DECIMAL` as text on its own wire
    /// protocol, and SQLite has no such type at all (a `NUMERIC`-affinity
    /// column there decodes as whatever runtime type the stored value
    /// actually has) — `FromValue for BigDecimal` accepts `Value::Text`/
    /// `Value::I64`/`Value::F64` too, so a mapped struct's `BigDecimal`
    /// field still round-trips correctly everywhere, just without
    /// Postgres's native wire format (and, on SQLite/via `f64`, without
    /// arbitrary precision — only as much as an `f64` itself preserves).
    Decimal(BigDecimal),
    /// A JSON value (a `JSON`/`JSONB` column), backed by `serde_json`'s own
    /// `Value` type (re-exported as `rusty_db::Json` to avoid colliding
    /// with this very type's name). Postgres has native `JSON`/`JSONB`
    /// types and round-trips this variant directly. SQLite has no JSON
    /// type at all (a JSON column there is really just `TEXT`), so it
    /// decodes a JSON column back as `Value::Text`; MySQL/MariaDB's own
    /// `JSON` type reports as one of its `BLOB`-family types at the
    /// wire-protocol level (even though the bytes themselves are plain
    /// UTF-8 JSON text), so it decodes back as `Value::Bytes` instead —
    /// `FromValue for Json` parses both of those forms too, so a mapped
    /// struct's `Json` field still round-trips correctly everywhere, just
    /// without Postgres's native wire format.
    Json(Json),
    /// A calendar date with no time-of-day or time zone (a `DATE` column).
    /// Postgres and MySQL/MariaDB both have a native `DATE` type and
    /// round-trip this variant directly; SQLite has no such type at all
    /// (a `DATE`-declared column there is really just `TEXT`), so it
    /// decodes back as `Value::Text` — `FromValue for NaiveDate` parses
    /// that text form too (the same ISO 8601 form this crate itself
    /// produces when binding a `NaiveDate` there), so a mapped struct's
    /// `NaiveDate` field still round-trips correctly everywhere, just
    /// without a native temporal column type on SQLite specifically.
    Date(NaiveDate),
    /// A time-of-day with no date or time zone (a `TIME` column). Same
    /// split as `Date`: native on Postgres and MySQL/MariaDB, flattened to
    /// `Value::Text` (and parsed back by `FromValue for NaiveTime`) on
    /// SQLite.
    Time(NaiveTime),
    /// A date and time-of-day with no time zone (a `TIMESTAMP` column on
    /// Postgres, `DATETIME` on MySQL/MariaDB) — the "wall clock" reading
    /// with no attached offset, same meaning SQLAlchemy's timezone-naive
    /// `DateTime` has. Same split as `Date`/`Time`: native on Postgres and
    /// MySQL/MariaDB, flattened to `Value::Text` (and parsed back by
    /// `FromValue for NaiveDateTime`) on SQLite.
    DateTime(NaiveDateTime),
    /// A UTC-normalized instant (a `TIMESTAMPTZ` column on Postgres, or a
    /// MySQL/MariaDB `TIMESTAMP` column — which MySQL itself always
    /// stores and reports as UTC, unlike its plain `DATETIME`). Postgres
    /// and MySQL/MariaDB both round-trip this variant directly; SQLite
    /// has no such type at all, so it decodes back as `Value::Text` (its
    /// RFC 3339 form) — `FromValue for DateTime<Utc>` parses that text
    /// form too, so a mapped struct's `DateTime<Utc>` field still
    /// round-trips correctly everywhere, just without a native temporal
    /// column type on SQLite specifically.
    Timestamp(DateTime<Utc>),
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
            Value::Uuid(u) => write!(f, "{u}"),
            Value::Decimal(d) => write!(f, "{d}"),
            Value::Json(j) => write!(f, "{j}"),
            Value::Date(d) => write!(f, "{d}"),
            Value::Time(t) => write!(f, "{t}"),
            Value::DateTime(dt) => write!(f, "{dt}"),
            Value::Timestamp(ts) => write!(f, "{ts}"),
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
impl_from!(Uuid, Uuid);
impl_from!(BigDecimal, Decimal);
impl_from!(Json, Json);
impl_from!(NaiveDate, Date);
impl_from!(NaiveTime, Time);
impl_from!(NaiveDateTime, DateTime);
impl_from!(DateTime<Utc>, Timestamp);

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

impl FromValue for Uuid {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Uuid(u) => Ok(*u),
            // MySQL/MariaDB and SQLite have no native UUID column type, so
            // a UUID column there decodes as plain text; parse it rather
            // than requiring the native `Value::Uuid` form only Postgres
            // ever actually produces.
            Value::Text(s) => Uuid::parse_str(s).map_err(|e| format!("invalid UUID {s:?}: {e}")),
            other => Err(format!("expected uuid, got {other}")),
        }
    }
}

impl FromValue for BigDecimal {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Decimal(d) => Ok(d.clone()),
            // MySQL/MariaDB sends DECIMAL as text, and SQLite has no
            // NUMERIC type of its own at all, so a decimal column on
            // either can decode as text or as a plain number depending on
            // how the value was actually stored — accept all three rather
            // than requiring the native `Value::Decimal` form only
            // Postgres ever actually produces.
            Value::Text(s) => {
                BigDecimal::from_str(s).map_err(|e| format!("invalid decimal {s:?}: {e}"))
            }
            Value::I64(i) => Ok(BigDecimal::from(*i)),
            Value::F64(f) => {
                BigDecimal::try_from(*f).map_err(|e| format!("invalid decimal from {f}: {e}"))
            }
            other => Err(format!("expected decimal, got {other}")),
        }
    }
}

impl FromValue for Json {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Json(j) => Ok(j.clone()),
            // SQLite has no JSON type of its own at all, so a JSON column
            // there decodes as plain text; parse it rather than requiring
            // the native `Value::Json` form only Postgres ever actually
            // produces.
            Value::Text(s) => {
                serde_json::from_str(s).map_err(|e| format!("invalid JSON {s:?}: {e}"))
            }
            // MySQL/MariaDB's JSON columns report as one of its BLOB-family
            // types at the wire-protocol level (despite the bytes
            // themselves being plain UTF-8 JSON text), so they decode as
            // `Value::Bytes` rather than `Value::Text` — parse the same
            // way, just via UTF-8 first.
            Value::Bytes(b) => std::str::from_utf8(b)
                .map_err(|e| format!("invalid UTF-8 in JSON bytes: {e}"))
                .and_then(|s| {
                    serde_json::from_str(s).map_err(|e| format!("invalid JSON {s:?}: {e}"))
                }),
            other => Err(format!("expected json, got {other}")),
        }
    }
}

impl FromValue for NaiveDate {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Date(d) => Ok(*d),
            // SQLite has no native DATE type, so a DATE column there
            // decodes as plain text; parse the same ISO 8601 form this
            // crate itself produces when binding a NaiveDate there.
            Value::Text(s) => NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|e| format!("invalid date {s:?}: {e}")),
            other => Err(format!("expected date, got {other}")),
        }
    }
}

impl FromValue for NaiveTime {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Time(t) => Ok(*t),
            // Same reasoning as NaiveDate above: SQLite flattens to text.
            Value::Text(s) => NaiveTime::parse_from_str(s, "%H:%M:%S%.f")
                .map_err(|e| format!("invalid time {s:?}: {e}")),
            other => Err(format!("expected time, got {other}")),
        }
    }
}

impl FromValue for NaiveDateTime {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::DateTime(dt) => Ok(*dt),
            // A tz-aware Value::Timestamp still carries a wall-clock
            // moment; drop its (always-UTC) offset rather than reject it.
            Value::Timestamp(dt) => Ok(dt.naive_utc()),
            // Same reasoning as NaiveDate above: SQLite flattens to text.
            // Accepts both the space- and 'T'-separated forms, since both
            // are common enough to be worth not rejecting outright.
            Value::Text(s) => NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S%.f")
                .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f"))
                .map_err(|e| format!("invalid datetime {s:?}: {e}")),
            other => Err(format!("expected datetime, got {other}")),
        }
    }
}

impl FromValue for DateTime<Utc> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Timestamp(dt) => Ok(*dt),
            // A naive Value::DateTime carries no offset at all (MySQL's
            // own DATETIME/TIMESTAMP split treats a DATETIME the same
            // way); treat it as already being in UTC rather than reject
            // it, matching how MySQL's own TIMESTAMP columns behave.
            Value::DateTime(dt) => Ok(Utc.from_utc_datetime(dt)),
            // SQLite has no native TIMESTAMPTZ-equivalent type, so a
            // timestamp column there decodes as plain text; parse the
            // same RFC 3339 form this crate itself produces when binding
            // a DateTime<Utc> there.
            Value::Text(s) => DateTime::parse_from_rfc3339(s)
                .map(|dt| dt.with_timezone(&Utc))
                .map_err(|e| format!("invalid timestamp {s:?}: {e}")),
            other => Err(format!("expected timestamp, got {other}")),
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
