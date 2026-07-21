#![cfg(feature = "postgres")]

use rusty_db::prelude::*;

/// Connects to a real PostgreSQL server for this test. There's no way to
/// spin one up portably in every environment this test suite runs in, so
/// this is opt-in: point `POSTGRES_TEST_URL` at a scratch database (its
/// schema is created and dropped by this test) or the test skips itself
/// instead of failing when no server is reachable.
async fn test_engine() -> Option<Engine> {
    let url = std::env::var("POSTGRES_TEST_URL")
        .unwrap_or_else(|_| "postgres://rusty:rusty@127.0.0.1/rusty_db_test".to_string());
    match PostgresDriver::engine(&url).await {
        Ok(engine) => Some(engine),
        Err(err) => {
            eprintln!("skipping Postgres test: could not connect to {url}: {err}");
            None
        }
    }
}

#[tokio::test]
async fn crud_roundtrip_against_postgres() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS users", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id BIGINT PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;

    let users = Table::new("users");

    let rows_inserted = engine
        .execute(
            &Insert::into_table(&users)
                .value("id", 1_i64)
                .value("name", "ada")
                .value("active", true),
        )
        .await?;
    assert_eq!(rows_inserted, 1);

    engine
        .execute(
            &Insert::into_table(&users)
                .value("id", 2_i64)
                .value("name", "grace")
                .value("active", false),
        )
        .await?;

    // Query builder round-trip: filter + order_by + limit.
    let active_users = Select::from(&users)
        .columns([users.col("id"), users.col("name")])
        .filter(users.col("active").eq(true))
        .order_by(users.col("id").asc())
        .limit(10);

    let rows = engine.fetch_all(&active_users).await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<i64>(0)?, 1);
    assert_eq!(rows[0].get::<String>(1)?, "ada");

    // Update, then re-query.
    let updated = engine
        .execute(
            &Update::table(&users)
                .set("active", true)
                .filter(users.col("id").eq(2_i64)),
        )
        .await?;
    assert_eq!(updated, 1);

    let all_active = engine
        .fetch_all(&Select::from(&users).filter(users.col("active").eq(true)))
        .await?;
    assert_eq!(all_active.len(), 2);

    // Delete, then confirm gone.
    let deleted = engine
        .execute(&Delete::from(&users).filter(users.col("id").eq(1_i64)))
        .await?;
    assert_eq!(deleted, 1);

    let remaining = engine.fetch_all(&Select::from(&users)).await?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].get_by_name::<String>("name")?, "grace");

    // Transaction rollback leaves state untouched.
    let mut txn = engine.begin().await?;
    txn.execute(
        "INSERT INTO users (id, name, active) VALUES ($1, $2, $3)",
        &[
            Value::I64(3),
            Value::Text("linus".into()),
            Value::Bool(true),
        ],
    )
    .await?;
    txn.rollback().await?;

    let after_rollback = engine.fetch_all(&Select::from(&users)).await?;
    assert_eq!(after_rollback.len(), 1);

    // RETURNING is honored on Postgres.
    let returned = engine
        .fetch_one(
            &Insert::into_table(&users)
                .value("id", 4_i64)
                .value("name", "linus")
                .value("active", true)
                .returning(["id", "name"]),
        )
        .await?;
    assert_eq!(returned.get_by_name::<i64>("id")?, 4);
    assert_eq!(returned.get_by_name::<String>("name")?, "linus");

    engine
        .connect()
        .await?
        .execute("DROP TABLE users", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn wider_column_types_decode_correctly() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };

    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS widgets", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE widgets (
                id UUID PRIMARY KEY,
                weight NUMERIC(10,2) NOT NULL,
                created_on DATE NOT NULL,
                created_at TIMESTAMPTZ NOT NULL,
                metadata JSONB,
                notes VARCHAR(255)
            )",
            &[],
        )
        .await?;

    engine
        .connect()
        .await?
        .execute(
            "INSERT INTO widgets (id, weight, created_on, created_at, metadata, notes) VALUES (\
                '11111111-1111-1111-1111-111111111111', \
                3.50, \
                '2024-01-15', \
                '2024-01-15T10:30:00Z', \
                '{\"color\": \"red\"}', \
                NULL\
            )",
            &[],
        )
        .await?;

    let widgets = Table::new("widgets");
    let rows = engine.fetch_all(&Select::from(&widgets)).await?;
    assert_eq!(rows.len(), 1);
    // A native Postgres UUID column decodes as Value::Uuid, not text.
    assert_eq!(
        rows[0].get_by_name::<Uuid>("id")?,
        "11111111-1111-1111-1111-111111111111"
            .parse::<Uuid>()
            .unwrap()
    );
    // A native Postgres NUMERIC column decodes as Value::Decimal, not
    // text; BigDecimal's own equality is value-based, so this holds
    // regardless of exactly how many digits of scale the column reports.
    assert_eq!(
        rows[0].get_by_name::<BigDecimal>("weight")?,
        "3.5".parse::<BigDecimal>().unwrap()
    );
    // A native Postgres DATE/TIMESTAMPTZ column decodes as Value::Date/
    // Value::Timestamp directly, not text.
    assert_eq!(
        rows[0].get_by_name::<NaiveDate>("created_on")?,
        "2024-01-15".parse::<NaiveDate>().unwrap()
    );
    assert_eq!(
        rows[0].get_by_name::<DateTime<Utc>>("created_at")?,
        "2024-01-15T10:30:00Z".parse::<DateTime<Utc>>().unwrap()
    );
    // A native Postgres JSONB column decodes as Value::Json, not text.
    assert_eq!(
        rows[0].get_by_name::<Json>("metadata")?,
        serde_json::json!({"color": "red"})
    );
    assert_eq!(rows[0].get_by_name::<Option<String>>("notes")?, None);

    engine
        .connect()
        .await?
        .execute("DROP TABLE widgets", &[])
        .await?;

    Ok(())
}
