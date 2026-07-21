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
    /// An ordered collection of homogeneously-typed values (a Postgres
    /// native array column, e.g. `INTEGER[]`/`TEXT[]`/`UUID[]`). Postgres
    /// has a native array type for virtually every scalar column type and
    /// round-trips this variant directly over its binary wire format.
    /// MySQL/MariaDB and SQLite have no array column type at all, so on
    /// those two an array field is stored as a JSON array instead (both
    /// already support JSON, natively or as text) — `FromValue for
    /// Vec<T>` (for the handful of `T` this crate implements it for —
    /// `bool`, `i64`, `f64`, `String`, `Uuid`, `BigDecimal`, `NaiveDate`,
    /// `NaiveTime`, `NaiveDateTime`, `DateTime<Utc>`, and `Value` itself
    /// as a fully-generic escape hatch) parses that JSON-array form back
    /// too, so a mapped struct's `Vec<T>` field still round-trips
    /// correctly everywhere, just without Postgres's native wire format
    /// (or a native array type at all) on the other two.
    ///
    /// A known limitation on Postgres specifically: binding an array picks
    /// its native Postgres array type by inspecting the first non-null
    /// element, since (unlike a mapped struct field's own static Rust
    /// type) `Value::Array` itself carries no element-type tag once
    /// constructed. An empty array, or one whose every element is
    /// `Value::Null`, has no element to inspect — that case binds as
    /// `TEXT[]`, which needs an explicit cast (or, more simply, a `TEXT[]`
    /// target column) if that's not what the column actually expects.
    Array(Vec<Value>),
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
            Value::Array(items) => {
                write!(f, "[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{item}")?;
                }
                write!(f, "]")
            }
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

/// Converts a single scalar `Value` into JSON, for encoding a
/// `Value::Array` as a JSON array on backends (MySQL/MariaDB, SQLite)
/// with no native array column type of their own.
fn value_to_json(v: &Value) -> Json {
    match v {
        Value::Null => Json::Null,
        Value::Bool(b) => Json::Bool(*b),
        Value::I64(i) => Json::from(*i),
        Value::F64(f) => serde_json::Number::from_f64(*f)
            .map(Json::Number)
            .unwrap_or(Json::Null),
        Value::Text(s) => Json::String(s.clone()),
        Value::Uuid(u) => Json::String(u.to_string()),
        Value::Decimal(d) => Json::String(d.to_string()),
        Value::Json(j) => j.clone(),
        Value::Date(d) => Json::String(d.to_string()),
        Value::Time(t) => Json::String(t.to_string()),
        Value::DateTime(dt) => Json::String(dt.to_string()),
        Value::Timestamp(ts) => Json::String(ts.to_rfc3339()),
        Value::Array(items) => Json::Array(items.iter().map(value_to_json).collect()),
        // Not meaningfully representable as a JSON scalar (no `Vec<u8>`
        // array-element support exists — see the module-level array
        // discussion); round-trips as null rather than erroring outright.
        Value::Bytes(_) => Json::Null,
    }
}

/// Renders a `Value::Array`'s elements as a JSON array — used by drivers
/// to bind `Value::Array` on backends with no native array column type of
/// their own (MySQL/MariaDB, SQLite; both already support storing/binding
/// plain JSON text).
pub fn array_to_json(items: &[Value]) -> Json {
    Json::Array(items.iter().map(value_to_json).collect())
}

/// The inverse of `value_to_json`, used when decoding a `Value::Array`
/// back out of its JSON-flattened form.
fn json_scalar_to_value(j: &Json) -> Value {
    match j {
        Json::Null => Value::Null,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n
            .as_i64()
            .map(Value::I64)
            .unwrap_or_else(|| Value::F64(n.as_f64().unwrap_or(f64::NAN))),
        Json::String(s) => Value::Text(s.clone()),
        Json::Array(_) | Json::Object(_) => Value::Json(j.clone()),
    }
}

/// Implements `From<Vec<$elem_ty>> for Value` and `FromValue for
/// Vec<$elem_ty>` for one array element type. Kept to concrete element
/// types (rather than one blanket `impl<T> ... for Vec<T>`) because a
/// blanket impl here would conflict with the existing dedicated
/// `Vec<u8>`/`Value::Bytes` handling above.
macro_rules! impl_array {
    ($elem_ty:ty) => {
        impl From<Vec<$elem_ty>> for Value {
            fn from(v: Vec<$elem_ty>) -> Self {
                Value::Array(v.into_iter().map(Value::from).collect())
            }
        }

        impl FromValue for Vec<$elem_ty> {
            fn from_value(value: &Value) -> Result<Self, String> {
                match value {
                    Value::Array(items) => items
                        .iter()
                        .map(<$elem_ty as FromValue>::from_value)
                        .collect(),
                    // MySQL/MariaDB and SQLite have no native array column
                    // type at all, so an array is stored as a JSON array
                    // there instead; decode whichever shape that JSON
                    // value actually comes back as on each backend (native
                    // JSON on Postgres/MySQL — see Value::Json's own doc
                    // for why MySQL specifically decodes as bytes — or
                    // text on SQLite).
                    Value::Json(Json::Array(items)) => items
                        .iter()
                        .map(|j| <$elem_ty as FromValue>::from_value(&json_scalar_to_value(j)))
                        .collect(),
                    Value::Text(s) => {
                        let parsed: Json = serde_json::from_str(s)
                            .map_err(|e| format!("invalid array JSON {s:?}: {e}"))?;
                        <Vec<$elem_ty> as FromValue>::from_value(&Value::Json(parsed))
                    }
                    Value::Bytes(b) => {
                        let s = std::str::from_utf8(b)
                            .map_err(|e| format!("invalid UTF-8 in array JSON bytes: {e}"))?;
                        let parsed: Json = serde_json::from_str(s)
                            .map_err(|e| format!("invalid array JSON {s:?}: {e}"))?;
                        <Vec<$elem_ty> as FromValue>::from_value(&Value::Json(parsed))
                    }
                    other => Err(format!("expected array, got {other}")),
                }
            }
        }
    };
}

impl_array!(bool);
impl_array!(i64);
impl_array!(f64);
impl_array!(String);
impl_array!(Uuid);
impl_array!(BigDecimal);
impl_array!(NaiveDate);
impl_array!(NaiveTime);
impl_array!(NaiveDateTime);
impl_array!(DateTime<Utc>);
impl_array!(Value);

impl<T: FromValue> FromValue for Option<T> {
    fn from_value(value: &Value) -> Result<Self, String> {
        match value {
            Value::Null => Ok(None),
            other => T::from_value(other).map(Some),
        }
    }
}
