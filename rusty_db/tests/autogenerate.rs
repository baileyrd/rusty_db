#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Engine::autogenerate_migration`/`TableSpec`: diffing a
//! `#[derive(Mapped)]` type's expected shape against a live database and
//! generating the DDL needed to reconcile them. Every generated statement
//! is actually executed, then re-diffing confirms it converged — not just
//! checked as rendered SQL text.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "customers")]
struct Customer {
    #[table(primary_key)]
    id: i64,
    name: String,
    balance: f64,
    active: bool,
    nickname: Option<String>,
}

fn file_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "rusty_db_autogenerate_{name}_{}.sqlite3",
        std::process::id()
    ))
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = file_path(name);
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    SqliteDriver::engine(&url).await
}

/// A fresh `Engine` against the same on-disk file — see `AlterTable`'s own
/// docs for why this matters when a connection already had a table's
/// pre-`ALTER` shape "in view".
async fn reopen_engine(name: &str) -> rusty_db::Result<Engine> {
    let url = format!("sqlite://{}?mode=rw", file_path(name).display());
    SqliteDriver::engine(&url).await
}

#[tokio::test]
async fn a_missing_table_gets_a_single_create_table_statement() -> rusty_db::Result<()> {
    let engine = file_engine("missing_table").await?;

    let expected = vec![TableSpec::of::<Customer>()];
    let statements = engine
        .autogenerate_migration(&expected, &AutogenerateOptions::default())
        .await?;
    assert_eq!(statements.len(), 1);
    assert!(statements[0].starts_with("CREATE TABLE"));
    assert!(statements[0].contains("PRIMARY KEY"));

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }

    // The generated table is genuinely usable through ordinary derive-macro use.
    let mut session = engine.session();
    session.add(&Customer {
        id: 1,
        name: "ada".to_string(),
        balance: 42.5,
        active: true,
        nickname: None,
    });
    session.commit().await?;

    let rows: Vec<Customer> = engine
        .fetch_all_as(&Select::from(&Customer::table()))
        .await?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "ada");

    // Re-diffing against the now-created table finds nothing left to do.
    let statements_again = engine
        .autogenerate_migration(&expected, &AutogenerateOptions::default())
        .await?;
    assert_eq!(statements_again, Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn an_up_to_date_table_generates_nothing() -> rusty_db::Result<()> {
    let engine = file_engine("up_to_date").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             balance REAL NOT NULL, active BOOLEAN NOT NULL, nickname TEXT)",
            &[],
        )
        .await?;

    let statements = engine
        .autogenerate_migration(
            &[TableSpec::of::<Customer>()],
            &AutogenerateOptions::default(),
        )
        .await?;
    assert_eq!(statements, Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn a_field_added_to_the_struct_is_detected_and_can_be_applied() -> rusty_db::Result<()> {
    let engine = file_engine("added_field").await?;
    // Missing "nickname" compared to the Customer struct.
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             balance REAL NOT NULL, active BOOLEAN NOT NULL)",
            &[],
        )
        .await?;

    let expected = vec![TableSpec::of::<Customer>()];
    let statements = engine
        .autogenerate_migration(&expected, &AutogenerateOptions::default())
        .await?;
    assert_eq!(
        statements,
        vec![r#"ALTER TABLE "customers" ADD COLUMN "nickname" TEXT"#.to_string()]
    );

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }
    drop(engine);

    // See AlterTable's docs: verify through a fresh connection.
    let engine = reopen_engine("added_field").await?;
    let statements_again = engine
        .autogenerate_migration(&expected, &AutogenerateOptions::default())
        .await?;
    assert_eq!(statements_again, Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn a_field_removed_from_the_struct_is_detected_and_can_be_applied() -> rusty_db::Result<()> {
    let engine = file_engine("removed_field").await?;
    // An extra "legacy_notes" column the Customer struct doesn't declare.
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             balance REAL NOT NULL, active BOOLEAN NOT NULL, nickname TEXT, \
             legacy_notes TEXT)",
            &[],
        )
        .await?;

    let expected = vec![TableSpec::of::<Customer>()];
    let statements = engine
        .autogenerate_migration(&expected, &AutogenerateOptions::default())
        .await?;
    assert_eq!(
        statements,
        vec![r#"ALTER TABLE "customers" DROP COLUMN "legacy_notes""#.to_string()]
    );

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }
    drop(engine);

    let engine = reopen_engine("removed_field").await?;
    let statements_again = engine
        .autogenerate_migration(&expected, &AutogenerateOptions::default())
        .await?;
    assert_eq!(statements_again, Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn an_untracked_table_is_never_mentioned_in_the_generated_statements() -> rusty_db::Result<()>
{
    let engine = file_engine("untracked_table").await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             balance REAL NOT NULL, active BOOLEAN NOT NULL, nickname TEXT)",
            &[],
        )
        .await?;
    // An unrelated table not in `expected` at all — must never be touched.
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE _rusty_db_migrations (version BIGINT PRIMARY KEY)",
            &[],
        )
        .await?;

    let statements = engine
        .autogenerate_migration(
            &[TableSpec::of::<Customer>()],
            &AutogenerateOptions::default(),
        )
        .await?;
    assert_eq!(statements, Vec::<String>::new());

    // The untracked table is still there, completely untouched.
    let tables = engine.list_tables().await?;
    assert!(tables.iter().any(|t| t == "_rusty_db_migrations"));

    Ok(())
}

#[tokio::test]
async fn a_rename_hint_produces_a_data_preserving_rename_column() -> rusty_db::Result<()> {
    let engine = file_engine("rename_hint").await?;
    // "nickname" pre-dates the struct's own "display_name" field; a
    // straight diff (no hint) would see this as an unrelated drop+add,
    // losing whatever's stored in the old column.
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
             balance REAL NOT NULL, active BOOLEAN NOT NULL, nickname TEXT)",
            &[],
        )
        .await?;
    engine
        .execute(
            &Insert::into_table(&Table::new("customers"))
                .value("id", 1_i64)
                .value("name", "ada")
                .value("balance", 42.5_f64)
                .value("active", true)
                .value("nickname", "the-original-value"),
        )
        .await?;

    #[derive(Debug, Clone, PartialEq, Mapped)]
    #[table(name = "customers")]
    struct RenamedCustomer {
        #[table(primary_key)]
        id: i64,
        name: String,
        balance: f64,
        active: bool,
        display_name: Option<String>,
    }

    let expected = vec![TableSpec::of::<RenamedCustomer>()];
    let options = AutogenerateOptions {
        renamed_columns: vec![(
            "customers".to_string(),
            "nickname".to_string(),
            "display_name".to_string(),
        )],
        allow_drop_tables: Vec::new(),
        changed_column_types: Vec::new(),
    };
    let statements = engine.autogenerate_migration(&expected, &options).await?;
    assert_eq!(
        statements,
        vec![r#"ALTER TABLE "customers" RENAME COLUMN "nickname" TO "display_name""#.to_string()]
    );

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }
    drop(engine);

    // See AlterTable's docs: verify through a fresh connection.
    let engine = reopen_engine("rename_hint").await?;
    let rows: Vec<RenamedCustomer> = engine
        .fetch_all_as(&Select::from(&RenamedCustomer::table()))
        .await?;
    assert_eq!(rows.len(), 1);
    // The rename preserved the row's actual data, unlike a drop+add would have.
    assert_eq!(rows[0].display_name.as_deref(), Some("the-original-value"));

    let statements_again = engine.autogenerate_migration(&expected, &options).await?;
    assert_eq!(statements_again, Vec::<String>::new());

    Ok(())
}

#[tokio::test]
async fn an_allow_listed_table_absent_from_expected_gets_dropped() -> rusty_db::Result<()> {
    let engine = file_engine("allow_listed_drop").await?;
    engine
        .connect()
        .await?
        .execute("CREATE TABLE legacy_sessions (id INTEGER PRIMARY KEY)", &[])
        .await?;

    let options = AutogenerateOptions {
        renamed_columns: Vec::new(),
        allow_drop_tables: vec!["legacy_sessions".to_string()],
        changed_column_types: Vec::new(),
    };
    let statements = engine.autogenerate_migration(&[], &options).await?;
    assert_eq!(
        statements,
        vec![r#"DROP TABLE "legacy_sessions""#.to_string()]
    );

    for statement in &statements {
        engine.connect().await?.execute(statement, &[]).await?;
    }

    let tables = engine.list_tables().await?;
    assert!(!tables.iter().any(|t| t == "legacy_sessions"));

    Ok(())
}

#[tokio::test]
async fn a_live_table_not_allow_listed_is_never_proposed_for_dropping() -> rusty_db::Result<()> {
    let engine = file_engine("not_allow_listed").await?;
    engine
        .connect()
        .await?
        .execute("CREATE TABLE legacy_sessions (id INTEGER PRIMARY KEY)", &[])
        .await?;

    // Same shape as the previous test, but the table is never named in
    // `allow_drop_tables` — it must be left completely alone.
    let statements = engine
        .autogenerate_migration(&[], &AutogenerateOptions::default())
        .await?;
    assert_eq!(statements, Vec::<String>::new());

    let tables = engine.list_tables().await?;
    assert!(tables.iter().any(|t| t == "legacy_sessions"));

    Ok(())
}
