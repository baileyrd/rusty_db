#![cfg(feature = "sqlite")]

//! Exercises `rusty_db::migration::cli::run_to`: the thin `up`/`down`/
//! `status` subcommand dispatcher meant to back a small `src/bin/migrate.rs`
//! in a caller's own crate. Uses `run_to` directly (not `run`) throughout,
//! since it takes the arguments and output writer explicitly rather than
//! reading real process argv/stdout, which is exactly what makes it
//! testable at all.

use rusty_db::migration::cli::run_to;
use rusty_db::prelude::*;

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "create_users",
        up: &["CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)"],
        down: &["DROP TABLE users"],
    },
    Migration {
        version: 2,
        name: "add_users_email",
        up: &["ALTER TABLE users ADD COLUMN email TEXT"],
        down: &["ALTER TABLE users DROP COLUMN email"],
    },
];

async fn run(engine: &Engine, args: &[&str]) -> rusty_db::Result<String> {
    let mut out = Vec::new();
    run_to(
        &mut out,
        engine,
        MIGRATIONS,
        args.iter().map(|s| s.to_string()),
    )
    .await?;
    Ok(String::from_utf8(out).expect("output is valid UTF-8"))
}

#[tokio::test]
async fn up_prints_each_applied_version_then_reports_up_to_date() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;

    let output = run(&engine, &["up"]).await?;
    assert!(output.contains("Applied migration 1"));
    assert!(output.contains("Applied migration 2"));

    // Both migrations actually ran.
    let users = Table::new("users");
    engine
        .execute(
            &Insert::into_table(&users)
                .value("id", 1_i64)
                .value("name", "ada")
                .value("email", "ada@example.com"),
        )
        .await?;

    let output_again = run(&engine, &["up"]).await?;
    assert!(output_again.contains("Already up to date."));

    Ok(())
}

#[tokio::test]
async fn down_prints_the_reverted_version_then_nothing_to_revert() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    run(&engine, &["up"]).await?;

    let output = run(&engine, &["down"]).await?;
    assert!(output.contains("Reverted migration 2"));

    let output_again_down = run(&engine, &["down"]).await?;
    assert!(output_again_down.contains("Reverted migration 1"));

    let output_nothing_left = run(&engine, &["down"]).await?;
    assert!(output_nothing_left.contains("Nothing to revert."));

    Ok(())
}

#[tokio::test]
async fn status_marks_each_migration_applied_or_not() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    // Only apply the first migration.
    engine.migrator().up(&MIGRATIONS[..1]).await?;

    let output = run(&engine, &["status"]).await?;
    assert!(output.contains("[x]") && output.contains("create_users"));
    assert!(output.contains("add_users_email"));
    // The unapplied migration's line has no "x" mark on it specifically —
    // check there's exactly one applied ("[x]") line, not two.
    assert_eq!(output.matches("[x]").count(), 1);

    Ok(())
}

#[tokio::test]
async fn an_unknown_command_is_a_migration_error() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let result = run(&engine, &["frobnicate"]).await;
    assert!(matches!(result, Err(rusty_db::Error::Migration(_))));
    Ok(())
}

#[tokio::test]
async fn no_command_at_all_is_a_migration_error() -> rusty_db::Result<()> {
    let engine = SqliteDriver::engine("sqlite::memory:").await?;
    let result = run(&engine, &[]).await;
    assert!(matches!(result, Err(rusty_db::Error::Migration(_))));
    Ok(())
}
