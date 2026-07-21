#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Session::query`/`SessionQuery`: a fluent, type-bound query
//! against a mapped type's own table, instead of building a
//! `Select::from(&T::table())` yourself and passing it to `load_all`.

use std::rc::Rc;

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
        "rusty_db_session_query_{name}_{}.sqlite3",
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
async fn query_filter_and_order_by_narrow_the_result_set() -> rusty_db::Result<()> {
    let engine = file_engine("filter_order").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    let table = User::table();
    let active = session
        .query::<User>()
        .filter(table.col("active").eq(true))
        .order_by(table.col("name").desc())
        .all()
        .await?;

    assert_eq!(
        active
            .iter()
            .map(|u| u.borrow().name.clone())
            .collect::<Vec<_>>(),
        vec!["grace".to_string(), "ada".to_string()]
    );

    Ok(())
}

#[tokio::test]
async fn query_limit_and_offset_page_through_results() -> rusty_db::Result<()> {
    let engine = file_engine("limit_offset").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    let table = User::table();
    let page = session
        .query::<User>()
        .order_by(table.col("id").asc())
        .limit(1)
        .offset(1)
        .all()
        .await?;

    assert_eq!(page.len(), 1);
    assert_eq!(page[0].borrow().name, "grace");

    Ok(())
}

#[tokio::test]
async fn query_first_returns_only_the_first_matching_row() -> rusty_db::Result<()> {
    let engine = file_engine("first").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    let table = User::table();
    let first = session
        .query::<User>()
        .filter(table.col("active").eq(true))
        .order_by(table.col("id").asc())
        .first()
        .await?
        .expect("at least one active user");
    assert_eq!(first.borrow().name, "ada");

    let none = session
        .query::<User>()
        .filter(table.col("name").eq("nobody"))
        .first()
        .await?;
    assert!(none.is_none());

    Ok(())
}

#[tokio::test]
async fn query_results_go_through_the_identity_map() -> rusty_db::Result<()> {
    let engine = file_engine("identity_map").await?;
    let mut session = engine.session();
    session.add_all(&seed_users());
    session.commit().await?;

    let ada = session.get::<User>(1_i64).await?.expect("ada exists");
    ada.borrow_mut().name = "ADA".to_string();

    let table = User::table();
    let via_query = session
        .query::<User>()
        .filter(table.col("id").eq(1_i64))
        .first()
        .await?
        .expect("ada exists");

    assert!(
        Rc::ptr_eq(&ada, &via_query),
        "SessionQuery results should be the same identity-mapped handle"
    );
    assert_eq!(via_query.borrow().name, "ADA");

    Ok(())
}

#[tokio::test]
async fn query_active_only_excludes_soft_deleted_rows() -> rusty_db::Result<()> {
    let engine = file_engine("active_only").await?;
    let mut session = engine.session();
    session.add(&Note {
        id: 1,
        deleted: false,
        body: "keep".to_string(),
    });
    session.add(&Note {
        id: 2,
        deleted: false,
        body: "drop".to_string(),
    });
    session.commit().await?;

    let note_two = session.get::<Note>(2_i64).await?.expect("exists");
    session.delete(&*note_two.borrow());
    session.commit().await?;

    let active = session.query::<Note>().active_only().all().await?;
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].borrow().body, "keep");

    // Without `active_only`, the soft-deleted row is still there (just
    // marked), same as `Session::load_all` without a filter.
    let everything = session.query::<Note>().all().await?;
    assert_eq!(everything.len(), 2);

    Ok(())
}
