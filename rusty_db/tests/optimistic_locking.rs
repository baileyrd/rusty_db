#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `#[table(version)]`: optimistic locking on `Session::update`/
//! `delete`. A versioned entity's `update`/`delete_query` include the
//! version in their `WHERE` clause (and `update` increments it), so a
//! write built from a stale copy — one that doesn't reflect a change
//! already committed by someone else — matches no row, and `Session`
//! turns that into `Error::Conflict` instead of a silent no-op.

use rusty_db::prelude::*;
use rusty_db::Error;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "documents")]
struct Document {
    #[table(primary_key)]
    id: i64,
    #[table(version)]
    version: i64,
    title: String,
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
        "rusty_db_optimistic_locking_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE documents (id INTEGER PRIMARY KEY, version INTEGER NOT NULL, title TEXT NOT NULL)",
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

async fn get_document(engine: &Engine, id: i64) -> rusty_db::Result<Document> {
    let table = Document::table();
    engine
        .fetch_one_as(&Select::from(&table).filter(table.col("id").eq(id)))
        .await
}

#[tokio::test]
async fn updating_with_the_current_version_succeeds_and_increments_it() -> rusty_db::Result<()> {
    let engine = file_engine("update_succeeds").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    session.update(&Document {
        id: 1,
        version: 1,
        title: "final".to_string(),
    });
    session.commit().await?;

    let stored = get_document(&engine, 1).await?;
    assert_eq!(
        stored,
        Document {
            id: 1,
            version: 2,
            title: "final".to_string()
        }
    );

    Ok(())
}

#[tokio::test]
async fn updating_with_a_stale_version_fails_with_conflict() -> rusty_db::Result<()> {
    let engine = file_engine("update_conflict").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    // Someone else updates the document first, bumping its version to 2.
    session.update(&Document {
        id: 1,
        version: 1,
        title: "edited by someone else".to_string(),
    });
    session.commit().await?;

    // This session still has the stale version=1 copy from before that
    // edit, and tries to update based on it.
    session.update(&Document {
        id: 1,
        version: 1,
        title: "clobbering edit".to_string(),
    });
    let outcome = session.commit().await;
    assert!(
        matches!(outcome, Err(Error::Conflict(_))),
        "expected a conflict, got {outcome:?}"
    );

    // The failed update never took effect — the other edit is intact.
    let stored = get_document(&engine, 1).await?;
    assert_eq!(
        stored,
        Document {
            id: 1,
            version: 2,
            title: "edited by someone else".to_string()
        }
    );

    Ok(())
}

#[tokio::test]
async fn updating_a_row_deleted_by_someone_else_fails_with_conflict() -> rusty_db::Result<()> {
    let engine = file_engine("update_after_delete").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    engine
        .execute(&Delete::from(&Document::table()).filter(Document::table().col("id").eq(1_i64)))
        .await?;

    session.update(&Document {
        id: 1,
        version: 1,
        title: "too late".to_string(),
    });
    let outcome = session.commit().await;
    assert!(matches!(outcome, Err(Error::Conflict(_))));

    Ok(())
}

#[tokio::test]
async fn deleting_with_the_current_version_succeeds() -> rusty_db::Result<()> {
    let engine = file_engine("delete_succeeds").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    session.delete(&Document {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    let rows: Vec<Document> = engine
        .fetch_all_as(&Select::from(&Document::table()))
        .await?;
    assert!(rows.is_empty());

    Ok(())
}

#[tokio::test]
async fn deleting_with_a_stale_version_fails_with_conflict() -> rusty_db::Result<()> {
    let engine = file_engine("delete_conflict").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        version: 1,
        title: "draft".to_string(),
    });
    session.commit().await?;

    // Someone else edits it, bumping the version to 2.
    session.update(&Document {
        id: 1,
        version: 1,
        title: "edited".to_string(),
    });
    session.commit().await?;

    // This session tries to delete based on the stale version=1 copy.
    session.delete(&Document {
        id: 1,
        version: 1,
        title: "edited".to_string(),
    });
    let outcome = session.commit().await;
    assert!(matches!(outcome, Err(Error::Conflict(_))));

    // The row (with the other edit) is still there, untouched.
    let stored = get_document(&engine, 1).await?;
    assert_eq!(stored.version, 2);

    Ok(())
}

#[tokio::test]
async fn a_type_without_a_version_field_never_conflicts() -> rusty_db::Result<()> {
    let engine = file_engine("no_version_field").await?;
    let mut session = engine.session();
    session.add(&Note {
        id: 1,
        body: "hello".to_string(),
    });
    session.commit().await?;

    // Updating a row that doesn't even exist is a silent no-op for a
    // plain (non-versioned) `Identifiable` type — there's no version
    // column for `Session` to have noticed a mismatch on.
    session.update(&Note {
        id: 999,
        body: "does not exist".to_string(),
    });
    session.commit().await?; // does NOT error

    Ok(())
}
