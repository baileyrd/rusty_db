#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Engine::backup`/`restore`: a logical (row-data) backup —
//! captured via `list_tables`/`table_schema` plus the query builder, not
//! any database-specific backup mechanism — and restoring it by
//! replaying deletes-then-inserts inside one transaction.

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
        "rusty_db_backup_restore_{name}_{}.sqlite3",
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

async fn all_users(engine: &Engine) -> rusty_db::Result<Vec<User>> {
    let mut rows: Vec<User> = engine.fetch_all_as(&Select::from(&User::table())).await?;
    rows.sort_by_key(|u| u.id);
    Ok(rows)
}

#[tokio::test]
async fn backup_captures_every_row_of_every_table() -> rusty_db::Result<()> {
    let engine = file_engine("captures_rows").await?;
    engine
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 1_i64)
                .value("name", "ada"),
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 2_i64)
                .value("name", "grace"),
        )
        .await?;

    let dump = engine.backup().await?;
    assert_eq!(dump.tables.len(), 1);
    let users_dump = &dump.tables[0];
    assert_eq!(users_dump.table, "users");
    assert_eq!(
        users_dump.columns,
        vec!["id".to_string(), "name".to_string()]
    );
    assert_eq!(users_dump.rows.len(), 2);

    Ok(())
}

#[tokio::test]
async fn restore_returns_the_database_to_its_backed_up_state() -> rusty_db::Result<()> {
    let engine = file_engine("round_trip").await?;
    engine
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 1_i64)
                .value("name", "ada"),
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 2_i64)
                .value("name", "grace"),
        )
        .await?;

    let dump = engine.backup().await?;

    // Mutate the database: delete one row, change another, add a new one.
    engine
        .execute(&Delete::from(&User::table()).filter(User::table().col("id").eq(1_i64)))
        .await?;
    engine
        .execute(
            &Update::table(&User::table())
                .set("name", "grace hopper")
                .filter(User::table().col("id").eq(2_i64)),
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 3_i64)
                .value("name", "linus"),
        )
        .await?;
    assert_eq!(
        all_users(&engine).await?,
        vec![
            User {
                id: 2,
                name: "grace hopper".to_string()
            },
            User {
                id: 3,
                name: "linus".to_string()
            },
        ]
    );

    engine.restore(&dump).await?;

    assert_eq!(
        all_users(&engine).await?,
        vec![
            User {
                id: 1,
                name: "ada".to_string()
            },
            User {
                id: 2,
                name: "grace".to_string()
            },
        ]
    );

    Ok(())
}

#[tokio::test]
async fn a_dump_can_be_restored_into_a_different_engine() -> rusty_db::Result<()> {
    let source = file_engine("cross_engine_source").await?;
    source
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 1_i64)
                .value("name", "ada"),
        )
        .await?;

    let dump = source.backup().await?;

    let destination = file_engine("cross_engine_destination").await?;
    destination.restore(&dump).await?;

    assert_eq!(
        all_users(&destination).await?,
        vec![User {
            id: 1,
            name: "ada".to_string()
        }]
    );

    Ok(())
}

#[tokio::test]
async fn a_failing_restore_rolls_back_completely() -> rusty_db::Result<()> {
    let engine = file_engine("failed_restore").await?;
    engine
        .execute(
            &Insert::into_table(&User::table())
                .value("id", 1_i64)
                .value("name", "ada"),
        )
        .await?;
    let dump = engine.backup().await?;

    // Corrupt the dump so the second row's insert violates the primary
    // key (duplicate id) partway through the restore.
    let mut broken_dump = dump.clone();
    broken_dump.tables[0]
        .rows
        .push(vec![1_i64.into(), "duplicate".into()]);

    let outcome = engine.restore(&broken_dump).await;
    assert!(outcome.is_err(), "expected the restore to fail");

    // The failed restore didn't leave the table empty (from the DELETE
    // that ran before the failing INSERT) — the whole transaction rolled
    // back, so the original row is still there untouched.
    assert_eq!(
        all_users(&engine).await?,
        vec![User {
            id: 1,
            name: "ada".to_string()
        }]
    );

    Ok(())
}

#[tokio::test]
async fn backing_up_an_empty_database_yields_an_empty_dump() -> rusty_db::Result<()> {
    let engine = file_engine("empty_table").await?;
    let dump = engine.backup().await?;
    assert_eq!(dump.tables.len(), 1);
    assert!(dump.tables[0].rows.is_empty());

    engine.restore(&dump).await?;
    assert_eq!(all_users(&engine).await?, Vec::new());

    Ok(())
}
