#![cfg(all(feature = "mysql", feature = "derive"))]

//! Same coverage as `audit_log.rs` (SQLite), against a real
//! MySQL/MariaDB server. Each test uses its own table *and* its own
//! audit table name (`with_audit_log_table`) to avoid colliding with
//! other tests running concurrently against the same shared live
//! server.

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

async fn recreate_table(engine: &Engine, table: &str) -> rusty_db::Result<()> {
    engine
        .connect()
        .await?
        .execute(&format!("DROP TABLE IF EXISTS {table}"), &[])
        .await?;
    engine
        .connect()
        .await?
        .execute(
            &format!("CREATE TABLE {table} (id BIGINT PRIMARY KEY, name TEXT NOT NULL)"),
            &[],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "audit_log_mysql_users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[tokio::test]
async fn insert_update_and_delete_are_each_recorded() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "audit_log_mysql_users").await?;
    engine
        .connect()
        .await?
        .execute("DROP TABLE IF EXISTS audit_log_mysql_history", &[])
        .await?;

    let mut session = engine
        .session()
        .with_audit_log_table("audit_log_mysql_history");

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    session.update(&User {
        id: 1,
        name: "ada lovelace".to_string(),
    });
    session.commit().await?;

    session.delete(&User {
        id: 1,
        name: "ada lovelace".to_string(),
    });
    session.commit().await?;

    let log = session.audit_log().await?;
    assert_eq!(log.len(), 3);
    assert_eq!(log[0].table, "audit_log_mysql_users");
    assert_eq!(log[0].operation, AuditOperation::Insert);
    assert_eq!(log[1].operation, AuditOperation::Update);
    assert_eq!(log[2].operation, AuditOperation::Delete);

    // audit_log() opened a transaction on its own connection (to read
    // through, same as get()/load_all()) that's still sitting open —
    // close it before the cleanup below tries to DROP these tables from
    // a different connection, or that would block forever on its lock.
    session.commit().await?;

    engine
        .connect()
        .await?
        .execute("DROP TABLE audit_log_mysql_users", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute("DROP TABLE audit_log_mysql_history", &[])
        .await?;

    Ok(())
}

#[tokio::test]
async fn audit_entries_share_atomicity_with_the_write_they_record() -> rusty_db::Result<()> {
    let Some(engine) = test_engine().await else {
        return Ok(());
    };
    recreate_table(&engine, "audit_log_mysql_atomicity_users").await?;
    engine
        .connect()
        .await?
        .execute(
            "DROP TABLE IF EXISTS audit_log_mysql_atomicity_history",
            &[],
        )
        .await?;

    #[derive(Debug, Clone, PartialEq, Mapped)]
    #[table(name = "audit_log_mysql_atomicity_users")]
    struct AtomicityUser {
        #[table(primary_key)]
        id: i64,
        name: String,
    }

    let mut session = engine
        .session()
        .with_audit_log_table("audit_log_mysql_atomicity_history");

    session.add(&AtomicityUser {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;
    assert_eq!(session.audit_log().await?.len(), 1);

    session.add(&AtomicityUser {
        id: 1,
        name: "duplicate".to_string(),
    });
    let outcome = session.commit().await;
    assert!(outcome.is_err(), "expected the duplicate insert to fail");
    session.rollback().await?;

    assert_eq!(session.audit_log().await?.len(), 1);
    // Same reason as the other test: close the transaction audit_log()
    // left open before dropping these tables from another connection.
    session.commit().await?;

    engine
        .connect()
        .await?
        .execute("DROP TABLE audit_log_mysql_atomicity_users", &[])
        .await?;
    engine
        .connect()
        .await?
        .execute("DROP TABLE audit_log_mysql_atomicity_history", &[])
        .await?;

    Ok(())
}
