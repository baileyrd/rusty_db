#![cfg(all(feature = "mysql", feature = "derive"))]

//! Exercises `Value::Date`/`Value::Time`/`Value::DateTime`/`Value::Timestamp`
//! against a real MySQL/MariaDB server, which has native `DATE`/`TIME`/
//! `DATETIME`/`TIMESTAMP` column types — unlike SQLite (see
//! `temporal_value.rs`), a column reflected/decoded here should come back
//! as the matching native variant directly, not `Value::Text`. `DATETIME`
//! and `TIMESTAMP` are the same packed bytes on MySQL's own wire protocol,
//! but this crate still tells them apart by column type name: `DATETIME`
//! decodes as `Value::DateTime` (no time zone at all, MySQL's own
//! semantics), `TIMESTAMP` as `Value::Timestamp` (MySQL always stores and
//! reports it as UTC).

use rusty_db::prelude::*;

/// Connects to a real MySQL/MariaDB server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `MYSQL_TEST_URL` at a scratch database (its schema
/// is created and dropped by this test) or the test skips itself instead of
/// failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("MYSQL_TEST_URL")
        .unwrap_or_else(|_| "mysql://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match MySqlDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping MySQL test: could not connect to {url}: {err}");
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "temporal_value_mysql_events")]
struct Event {
    #[table(primary_key)]
    id: i64,
    day: NaiveDate,
    time_of_day: NaiveTime,
    logged_at: NaiveDateTime,
    happened_at: DateTime<Utc>,
    canceled_on: Option<NaiveDate>,
}

#[tokio::test]
async fn temporal_fields_round_trip_through_native_column_types() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS temporal_value_mysql_events", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE temporal_value_mysql_events (\
                 id BIGINT PRIMARY KEY, day DATE NOT NULL, time_of_day TIME NOT NULL, \
                 logged_at DATETIME NOT NULL, happened_at TIMESTAMP NOT NULL, \
                 canceled_on DATE\
             )",
            &[],
        )
        .await?;

    let event = Event {
        id: 1,
        day: "2024-01-15".parse().unwrap(),
        time_of_day: "10:30:00".parse().unwrap(),
        logged_at: "2024-01-15T10:30:00".parse().unwrap(),
        happened_at: "2024-01-15T10:30:00Z".parse().unwrap(),
        canceled_on: None,
    };
    engine.execute(&event.insert()).await?;

    let table = Event::table();
    let fetched: Event = engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert_eq!(fetched, event);

    // Confirm the native path is actually taken for all four, not
    // text-flattened — and that DATETIME/TIMESTAMP, despite being the same
    // packed bytes on the wire, decode as the two different variants.
    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    assert!(matches!(row.value(1), Some(Value::Date(_))));
    assert!(matches!(row.value(2), Some(Value::Time(_))));
    assert!(matches!(row.value(3), Some(Value::DateTime(_))));
    assert!(matches!(row.value(4), Some(Value::Timestamp(_))));

    engine
        .connect()
        .await?
        .execute("DROP TABLE temporal_value_mysql_events", &[])
        .await?;
    Ok(())
}

#[tokio::test]
async fn null_temporal_field_round_trips_as_none() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS temporal_value_mysql_null_events", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE temporal_value_mysql_null_events (\
                 id BIGINT PRIMARY KEY, day DATE NOT NULL, time_of_day TIME NOT NULL, \
                 logged_at DATETIME NOT NULL, happened_at TIMESTAMP NOT NULL, \
                 canceled_on DATE\
             )",
            &[],
        )
        .await?;

    let table = Table::new("temporal_value_mysql_null_events");
    engine
        .execute(
            &Insert::into_table(&table)
                .value("id", 1_i64)
                .value("day", "2024-01-15".parse::<NaiveDate>().unwrap())
                .value("time_of_day", "10:30:00".parse::<NaiveTime>().unwrap())
                .value(
                    "logged_at",
                    "2024-01-15T10:30:00".parse::<NaiveDateTime>().unwrap(),
                )
                .value(
                    "happened_at",
                    "2024-01-15T10:30:00Z".parse::<DateTime<Utc>>().unwrap(),
                )
                .value("canceled_on", Value::Null),
        )
        .await?;

    let row = engine
        .fetch_one(&Select::from(&table).filter(table.col("id").eq(1_i64)))
        .await?;
    let canceled_on: Option<NaiveDate> = row.get_by_name("canceled_on")?;
    assert_eq!(canceled_on, None);

    engine
        .connect()
        .await?
        .execute("DROP TABLE temporal_value_mysql_null_events", &[])
        .await?;
    Ok(())
}
