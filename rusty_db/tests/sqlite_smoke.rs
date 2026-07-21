#![cfg(feature = "sqlite")]

use rusty_db::prelude::*;

#[tokio::test]
async fn crud_roundtrip_against_sqlite() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;

    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
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
        "INSERT INTO users (id, name, active) VALUES (?, ?, ?)",
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

    Ok(())
}
