#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `#[table(soft_delete)]`: `Session::delete` marks a row
//! instead of removing it, `Session::get` treats a marked row as gone,
//! and `Mapped::not_deleted_filter`/`Session::load_active` let callers
//! query only still-active rows.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    #[table(soft_delete)]
    deleted: bool,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "notes")]
struct Note {
    #[table(primary_key)]
    id: i64,
    body: String,
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_soft_delete_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE users (id INTEGER PRIMARY KEY, deleted BOOLEAN NOT NULL, name TEXT NOT NULL)",
            &[],
        )
        .await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE notes (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

fn ada() -> User {
    User {
        id: 1,
        deleted: false,
        name: "ada".to_string(),
    }
}

#[tokio::test]
async fn deleting_marks_the_row_instead_of_removing_it() -> rusty_db::Result<()> {
    let engine = file_engine("marks_instead_of_removes").await?;
    let mut session = engine.session();
    session.add(&ada());
    session.commit().await?;

    session.delete(&ada());
    session.commit().await?;

    // The row is still physically there, just marked.
    let table = User::table();
    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&table)).await?;
    assert_eq!(
        rows,
        vec![User {
            id: 1,
            deleted: true,
            name: "ada".to_string()
        }]
    );

    Ok(())
}

#[tokio::test]
async fn get_treats_a_soft_deleted_row_as_not_found() -> rusty_db::Result<()> {
    let engine = file_engine("get_treats_as_missing").await?;
    let mut session = engine.session();
    session.add(&ada());
    session.commit().await?;

    assert!(session.get::<User>(1_i64).await?.is_some());

    session.delete(&ada());
    session.commit().await?;

    // A fresh session (no identity-map cache from before the delete) sees
    // the soft-deleted row as gone.
    let mut fresh_session = engine.session();
    assert!(fresh_session.get::<User>(1_i64).await?.is_none());

    Ok(())
}

#[tokio::test]
async fn not_deleted_filter_excludes_soft_deleted_rows() -> rusty_db::Result<()> {
    let engine = file_engine("not_deleted_filter").await?;
    let mut session = engine.session();
    session.add(&ada());
    session.add(&User {
        id: 2,
        deleted: false,
        name: "grace".to_string(),
    });
    session.commit().await?;

    session.delete(&ada());
    session.commit().await?;

    let table = User::table();
    let query = Select::from(&table).filter(User::not_deleted_filter().unwrap());
    let mut active: Vec<User> = engine.fetch_all_as(&query).await?;
    active.sort_by_key(|u| u.id);
    assert_eq!(
        active,
        vec![User {
            id: 2,
            deleted: false,
            name: "grace".to_string()
        }]
    );

    Ok(())
}

#[tokio::test]
async fn load_active_returns_only_still_active_rows() -> rusty_db::Result<()> {
    let engine = file_engine("load_active").await?;
    let mut session = engine.session();
    session.add(&ada());
    session.add(&User {
        id: 2,
        deleted: false,
        name: "grace".to_string(),
    });
    session.commit().await?;

    session.delete(&ada());
    session.commit().await?;

    let active = session.load_active::<User>().await?;
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].borrow().name, "grace");

    Ok(())
}

#[tokio::test]
async fn delete_query_is_still_a_real_hard_delete() -> rusty_db::Result<()> {
    let engine = file_engine("hard_delete_still_available").await?;
    let mut session = engine.session();
    session.add(&ada());
    session.commit().await?;

    // Calling the entity's own delete_query() directly (bypassing
    // Session::delete) still issues a real DELETE.
    engine.execute(&ada().delete_query()).await?;

    let rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    assert!(rows.is_empty(), "the row should be genuinely gone");

    Ok(())
}

#[tokio::test]
async fn a_type_without_a_soft_delete_column_deletes_normally() -> rusty_db::Result<()> {
    let engine = file_engine("no_soft_delete_column").await?;
    let mut session = engine.session();
    session.add(&Note {
        id: 1,
        body: "hello".to_string(),
    });
    session.commit().await?;

    session.delete(&Note {
        id: 1,
        body: "hello".to_string(),
    });
    session.commit().await?;

    let rows: Vec<Note> = engine.fetch_all_as(&Select::from(&Note::table())).await?;
    assert!(
        rows.is_empty(),
        "a plain delete should really remove the row"
    );

    Ok(())
}
