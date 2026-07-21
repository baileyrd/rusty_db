#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Session::bulk_update`/`bulk_delete`: an arbitrary,
//! filter-scoped `UPDATE`/`DELETE` against a table — not bound to any
//! single entity — queued and flushed the same way `add`/`update`/`delete`
//! are, instead of loading every matching row and writing it back one at
//! a time.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
    active: bool,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "notes")]
struct Note {
    #[table(primary_key)]
    id: i64,
    #[table(soft_delete)]
    deleted: bool,
    body: String,
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_bulk_update_delete_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, deleted BOOLEAN NOT NULL, body TEXT NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

async fn all_users(engine: &Engine) -> rusty_db::Result<Vec<User>> {
    let mut rows: Vec<User> = engine
        .fetch_all_as(&Select::from(&User::table()).order_by(User::table().col("id").asc()))
        .await?;
    rows.sort_by_key(|u| u.id);
    Ok(rows)
}

fn seed_users() -> Vec<User> {
    vec![
        User {
            id: 1,
            name: "ada".to_string(),
            active: true,
        },
        User {
            id: 2,
            name: "grace".to_string(),
            active: true,
        },
        User {
            id: 3,
            name: "linus".to_string(),
            active: false,
        },
    ]
}

#[tokio::test]
async fn bulk_update_changes_every_matching_row_in_one_statement() -> rusty_db::Result<()> {
    let engine = file_engine("bulk_update_basic").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    let table = User::table();
    session.bulk_update::<User>(
        Update::table(&table)
            .set("active", false)
            .filter(table.col("active").eq(true)),
    );
    assert_eq!(
        session.pending_len(),
        1,
        "one bulk statement, not one per matching row"
    );
    session.commit().await?;

    let users = all_users(&engine).await?;
    assert!(
        users.iter().all(|u| !u.active),
        "every row should now be inactive"
    );

    Ok(())
}

#[tokio::test]
async fn bulk_delete_removes_every_matching_row_in_one_statement() -> rusty_db::Result<()> {
    let engine = file_engine("bulk_delete_basic").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    let table = User::table();
    session.bulk_delete::<User>(Delete::from(&table).filter(table.col("active").eq(true)));
    assert_eq!(session.pending_len(), 1);
    session.commit().await?;

    let users = all_users(&engine).await?;
    assert_eq!(
        users,
        vec![User {
            id: 3,
            name: "linus".to_string(),
            active: false,
        }],
        "only the inactive row (not matching the filter) should remain"
    );

    Ok(())
}

#[tokio::test]
async fn bulk_update_bypasses_the_identity_map() -> rusty_db::Result<()> {
    let engine = file_engine("bulk_update_identity_map").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    // Cache ada's row through the identity map.
    let ada = session.get::<User>(1_i64).await?.expect("ada exists");
    assert!(ada.borrow().active);

    let table = User::table();
    session.bulk_update::<User>(
        Update::table(&table)
            .set("active", false)
            .filter(table.col("id").eq(1_i64)),
    );
    session.commit().await?;

    // The cached handle isn't touched by a bulk_update...
    assert!(
        ada.borrow().active,
        "bulk_update bypasses the identity map; the cached handle stays stale"
    );

    // ...even though the database itself was actually updated.
    let mut fresh_session = engine.session();
    let refetched = fresh_session
        .get::<User>(1_i64)
        .await?
        .expect("ada still exists");
    assert!(!refetched.borrow().active);

    Ok(())
}

#[tokio::test]
async fn bulk_delete_is_a_real_hard_delete_even_for_a_soft_deletable_type() -> rusty_db::Result<()>
{
    let engine = file_engine("bulk_delete_soft_delete_bypass").await?;
    let mut session = engine.session();
    session.add(&Note {
        id: 1,
        deleted: false,
        body: "hello".to_string(),
    });
    session.add(&Note {
        id: 2,
        deleted: false,
        body: "world".to_string(),
    });
    session.commit().await?;

    let table = Note::table();
    session.bulk_delete::<Note>(Delete::from(&table).filter(table.col("id").eq(1_i64)));
    session.commit().await?;

    // A real DELETE, not a soft-delete UPDATE: the row is genuinely gone,
    // not left behind with `deleted = true`.
    let remaining: Vec<Note> = engine.fetch_all_as(&Select::from(&table)).await?;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, 2);

    Ok(())
}

#[tokio::test]
async fn bulk_writes_are_audit_logged_when_enabled() -> rusty_db::Result<()> {
    let engine = file_engine("bulk_audit_log").await?;
    let mut session = engine.session().with_audit_log();
    session.add_all(&seed_users());
    session.commit().await?;

    let table = User::table();
    session.bulk_update::<User>(
        Update::table(&table)
            .set("active", false)
            .filter(table.col("active").eq(true)),
    );
    session.bulk_delete::<User>(Delete::from(&table).filter(table.col("id").eq(3_i64)));
    session.commit().await?;

    let entries = session.audit_log().await?;
    let bulk_entries: Vec<_> = entries.iter().filter(|e| e.sql.contains("WHERE")).collect();
    assert_eq!(
        bulk_entries.len(),
        2,
        "both the bulk update and bulk delete should be recorded: {entries:?}"
    );
    assert!(matches!(bulk_entries[0].operation, AuditOperation::Update));
    assert!(matches!(bulk_entries[1].operation, AuditOperation::Delete));

    Ok(())
}

#[tokio::test]
async fn a_failing_bulk_write_rolls_back_the_whole_transaction() -> rusty_db::Result<()> {
    let engine = file_engine("bulk_update_rollback").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    let mut new_session = engine.session();
    let table = User::table();
    new_session.bulk_update::<User>(
        Update::table(&table)
            .set("active", false)
            .filter(table.col("active").eq(true)),
    );
    // A bogus second write in the same flush batch, forcing a failure
    // after the bulk update already ran against this transaction.
    new_session.add(&User {
        id: 1, // duplicate primary key
        name: "duplicate".to_string(),
        active: true,
    });
    let outcome = new_session.commit().await;
    assert!(outcome.is_err(), "expected the batch to fail");

    // The bulk update should not have taken effect either, since it shared
    // the same all-or-nothing transaction as the failing insert.
    let users = all_users(&engine).await?;
    assert!(
        users.iter().any(|u| u.id == 1 && u.active),
        "the bulk update should have been rolled back along with the failing insert"
    );

    Ok(())
}
