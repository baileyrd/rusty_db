#![cfg(feature = "sqlite")]

//! `Engine::begin_two_phase`/`commit_prepared`/`rollback_prepared` need a
//! dialect that actually supports two-phase (prepared) commit — SQLite
//! doesn't have the concept (there's no notion of a prepared transaction
//! surviving independently of its connection), so this just confirms all
//! three report `Error::Unsupported` cleanly rather than attempting
//! something SQLite has no SQL for. Postgres and MySQL, which do support
//! it, are covered against real servers in
//! `two_phase_commit_postgres.rs`/`two_phase_commit_mysql.rs`.

use rusty_db::prelude::*;

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_two_phase_commit_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    SqliteDriver::engine(&url).await
}

#[tokio::test]
async fn begin_two_phase_reports_unsupported_on_sqlite() -> rusty_db::Result<()> {
    let engine = file_engine("begin").await?;
    let result = engine.begin_two_phase("some-gid").await;
    assert!(matches!(result, Err(rusty_db::Error::Unsupported(_))));
    Ok(())
}

#[tokio::test]
async fn commit_prepared_reports_unsupported_on_sqlite() -> rusty_db::Result<()> {
    let engine = file_engine("commit_prepared").await?;
    let result = engine.commit_prepared("some-gid").await;
    assert!(matches!(result, Err(rusty_db::Error::Unsupported(_))));
    Ok(())
}

#[tokio::test]
async fn rollback_prepared_reports_unsupported_on_sqlite() -> rusty_db::Result<()> {
    let engine = file_engine("rollback_prepared").await?;
    let result = engine.rollback_prepared("some-gid").await;
    assert!(matches!(result, Err(rusty_db::Error::Unsupported(_))));
    Ok(())
}
