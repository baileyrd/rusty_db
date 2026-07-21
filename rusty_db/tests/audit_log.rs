#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Session`'s opt-in audit logging: every write a session
//! flushes gets recorded into an append-only audit table, atomically
//! with the write itself.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_audit_log_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn a_plain_session_never_creates_an_audit_table() -> rusty_db::Result<()> {
    let engine = file_engine("no_audit_by_default").await?;
    let mut session = engine.session();
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    assert_eq!(engine.list_tables().await?, vec!["users".to_string()]);

    Ok(())
}

#[tokio::test]
async fn insert_update_and_delete_are_each_recorded() -> rusty_db::Result<()> {
    let engine = file_engine("records_each_operation").await?;
    let mut session = engine.session().with_audit_log();

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

    assert_eq!(log[0].table, "users");
    assert_eq!(log[0].operation, AuditOperation::Insert);
    assert!(log[0].sql.to_uppercase().starts_with("INSERT"));
    assert!(log[0].params_text.contains("ada"));

    assert_eq!(log[1].operation, AuditOperation::Update);
    assert!(log[1].sql.to_uppercase().starts_with("UPDATE"));
    assert!(log[1].params_text.contains("ada lovelace"));

    assert_eq!(log[2].operation, AuditOperation::Delete);
    assert!(log[2].sql.to_uppercase().starts_with("DELETE"));

    Ok(())
}

#[tokio::test]
async fn audit_entries_share_atomicity_with_the_write_they_record() -> rusty_db::Result<()> {
    let engine = file_engine("shares_atomicity").await?;
    let mut session = engine.session().with_audit_log();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;
    assert_eq!(session.audit_log().await?.len(), 1);

    // A second insert with a duplicate primary key fails...
    session.add(&User {
        id: 1,
        name: "duplicate".to_string(),
    });
    let outcome = session.commit().await;
    assert!(outcome.is_err(), "expected the duplicate insert to fail");
    // ...discard it (same as any other failed write) rather than retrying
    // the same bad statement on the next flush.
    session.rollback().await?;

    // ...and its audit entry never took effect either — the failed
    // write's whole transaction (data and audit trail alike) rolled back.
    assert_eq!(session.audit_log().await?.len(), 1);

    Ok(())
}

#[tokio::test]
async fn audit_log_reflects_uncommitted_writes_from_the_same_session() -> rusty_db::Result<()> {
    let engine = file_engine("sees_own_writes").await?;
    let mut session = engine.session().with_audit_log();

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });

    // Not committed yet, but the session's own audit_log() autoflushes
    // and reads through its own transaction, same as get()/load_all().
    let log = session.audit_log().await?;
    assert_eq!(log.len(), 1);

    // A separate connection sees neither the row nor the (uncommitted,
    // still-transactional-DDL) audit table itself until commit().
    assert_eq!(engine.list_tables().await?, vec!["users".to_string()]);
    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(rows.is_empty(), "the insert hasn't committed yet");

    session.commit().await?;
    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert_eq!(rows.len(), 1);
    let mut tables = engine.list_tables().await?;
    tables.sort();
    assert_eq!(
        tables,
        vec!["_rusty_db_audit_log".to_string(), "users".to_string()]
    );

    Ok(())
}

#[tokio::test]
async fn a_custom_audit_table_name_is_honored() -> rusty_db::Result<()> {
    let engine = file_engine("custom_table_name").await?;
    let mut session = engine.session().with_audit_log_table("change_history");

    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    assert!(engine
        .list_tables()
        .await?
        .contains(&"change_history".to_string()));
    assert_eq!(session.audit_log().await?.len(), 1);

    Ok(())
}
