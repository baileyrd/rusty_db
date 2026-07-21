#![cfg(feature = "sqlite")]

//! Exercises `Value::Date`/`Value::Time`/`Value::DateTime`/`Value::Timestamp`
//! and their `NaiveDate`/`NaiveTime`/`NaiveDateTime`/`DateTime<Utc>`
//! conversions against SQLite, which has no native temporal column type of
//! its own — every one of these flattens to `Value::Text` there, the same
//! treatment `Uuid`/`BigDecimal`/`Json` already get on their own
//! non-native backends.

use rusty_db::prelude::*;
use rusty_db::FromValue;

#[cfg(feature = "derive")]
#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "events")]
struct Event {
    #[table(primary_key)]
    id: i64,
    day: NaiveDate,
    time_of_day: NaiveTime,
    logged_at: NaiveDateTime,
    happened_at: DateTime<Utc>,
    canceled_on: Option<NaiveDate>,
}

async fn engine_with_schema() -> rusty_db::Result<Engine> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE events (id INTEGER PRIMARY KEY, day DATE NOT NULL, time_of_day TIME \
             NOT NULL, logged_at DATETIME NOT NULL, happened_at TIMESTAMP NOT NULL, canceled_on \
             DATE)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn temporal_values_round_trip_through_the_query_builder() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;
    let events = Table::new("events");

    let day: NaiveDate = "2024-01-15".parse().unwrap();
    let time_of_day: NaiveTime = "10:30:00".parse().unwrap();
    let logged_at: NaiveDateTime = "2024-01-15T10:30:00".parse().unwrap();
    let happened_at: DateTime<Utc> = "2024-01-15T10:30:00Z".parse().unwrap();

    engine
        .execute(
            &Insert::into_table(&events)
                .value("id", 1_i64)
                .value("day", day)
                .value("time_of_day", time_of_day)
                .value("logged_at", logged_at)
                .value("happened_at", happened_at),
        )
        .await?;

    let rows = engine.fetch_all(&Select::from(&events)).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get_by_name::<NaiveDate>("day")?, day);
    assert_eq!(
        rows[0].get_by_name::<NaiveTime>("time_of_day")?,
        time_of_day
    );
    assert_eq!(
        rows[0].get_by_name::<NaiveDateTime>("logged_at")?,
        logged_at
    );
    assert_eq!(
        rows[0].get_by_name::<DateTime<Utc>>("happened_at")?,
        happened_at
    );

    // SQLite has no native temporal type: every one of these flattens to
    // Value::Text underneath, in the ISO 8601 form this crate itself
    // produces when binding.
    assert_eq!(rows[0].get_by_name::<String>("day")?, "2024-01-15");
    assert_eq!(rows[0].get_by_name::<String>("time_of_day")?, "10:30:00");
    assert_eq!(
        rows[0].get_by_name::<String>("logged_at")?,
        "2024-01-15 10:30:00"
    );
    assert_eq!(
        rows[0].get_by_name::<String>("happened_at")?,
        "2024-01-15T10:30:00+00:00"
    );

    Ok(())
}

#[cfg(feature = "derive")]
#[tokio::test]
async fn mapped_struct_temporal_fields_round_trip() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;

    let event = Event {
        id: 1,
        day: "2024-01-15".parse().unwrap(),
        time_of_day: "10:30:00".parse().unwrap(),
        logged_at: "2024-01-15T10:30:00".parse().unwrap(),
        happened_at: "2024-01-15T10:30:00Z".parse().unwrap(),
        canceled_on: None,
    };
    engine.execute(&event.insert()).await?;

    let fetched: Event = engine
        .fetch_one_as(&Select::from(&Event::table()).filter(Event::table().col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, event);

    Ok(())
}

#[tokio::test]
async fn naive_datetime_field_accepts_both_space_and_t_separated_text() -> rusty_db::Result<()> {
    let engine = engine_with_schema().await?;
    let events = Table::new("events");

    // Insert via raw text (bypassing this crate's own binding), using the
    // 'T'-separated form rather than the space-separated one `to_string()`
    // itself produces.
    engine
        .connect()
        .await?
        .execute(
            "INSERT INTO events (id, day, time_of_day, logged_at, happened_at) VALUES (1, \
             '2024-01-15', '10:30:00', '2024-01-15T10:30:00', '2024-01-15T10:30:00Z')",
            &[],
        )
        .await?;

    let rows = engine.fetch_all(&Select::from(&events)).await?;
    let expected: NaiveDateTime = "2024-01-15T10:30:00".parse().unwrap();
    assert_eq!(rows[0].get_by_name::<NaiveDateTime>("logged_at")?, expected);

    Ok(())
}

// Not a database test: SQLite flattens both Value::DateTime and
// Value::Timestamp to the same untagged Value::Text, so it can't exercise
// the cross-variant fallback below (that needs a backend where the two
// stay distinct at decode time — see temporal_value_postgres.rs /
// temporal_value_mysql.rs). This exercises the conversion logic directly.
#[test]
fn timestamp_and_naive_datetime_fall_back_to_each_other() {
    let naive: NaiveDateTime = "2024-01-15T10:30:00".parse().unwrap();
    let utc: DateTime<Utc> = "2024-01-15T10:30:00Z".parse().unwrap();

    // A naive Value::DateTime has no offset at all; DateTime<Utc> treats
    // it as already being in UTC rather than reject it.
    assert_eq!(
        DateTime::<Utc>::from_value(&Value::DateTime(naive)),
        Ok(utc)
    );
    // A tz-aware Value::Timestamp still carries a wall-clock moment;
    // NaiveDateTime drops its (always-UTC) offset rather than reject it.
    assert_eq!(NaiveDateTime::from_value(&Value::Timestamp(utc)), Ok(naive));
}
