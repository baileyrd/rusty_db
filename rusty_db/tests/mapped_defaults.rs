#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `#[table(default = "...")]`: a mapping-level default applied
//! by `insert()` whenever a field's value equals `Default::default()` for
//! its type, distinct from any database-side column `DEFAULT`.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "tasks")]
struct Task {
    #[table(primary_key)]
    id: i64,
    #[table(default = "'pending'")]
    status: String,
    #[table(default = "42")]
    priority: i64,
    label: String,
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_mapped_defaults_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE tasks (id INTEGER PRIMARY KEY, status TEXT NOT NULL, \
             priority INTEGER NOT NULL, label TEXT NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

async fn all_tasks(engine: &Engine) -> rusty_db::Result<Vec<Task>> {
    let mut rows: Vec<Task> = engine.fetch_all_as(&Select::from(&Task::table())).await?;
    rows.sort_by_key(|t| t.id);
    Ok(rows)
}

#[tokio::test]
async fn a_field_left_at_its_type_default_gets_the_mapping_level_default() -> rusty_db::Result<()> {
    let engine = file_engine("left_at_default").await?;
    let mut session = engine.session();
    session.add(&Task {
        id: 1,
        status: String::new(), // String::default()
        priority: 0,           // i64::default()
        label: "write tests".to_string(),
    });
    session.commit().await?;

    assert_eq!(
        all_tasks(&engine).await?,
        vec![Task {
            id: 1,
            status: "pending".to_string(),
            priority: 42,
            label: "write tests".to_string(),
        }]
    );

    Ok(())
}

#[tokio::test]
async fn a_field_explicitly_set_to_a_non_default_value_is_preserved() -> rusty_db::Result<()> {
    let engine = file_engine("non_default_value").await?;
    let mut session = engine.session();
    session.add(&Task {
        id: 1,
        status: "active".to_string(),
        priority: 7,
        label: "ship it".to_string(),
    });
    session.commit().await?;

    assert_eq!(
        all_tasks(&engine).await?,
        vec![Task {
            id: 1,
            status: "active".to_string(),
            priority: 7,
            label: "ship it".to_string(),
        }]
    );

    Ok(())
}

/// Documents the feature's known limitation: since Rust has no "unset"
/// field state, a genuine value equal to the type's default is
/// indistinguishable from "left unset" and also gets the mapping default.
#[tokio::test]
async fn a_genuine_value_equal_to_the_type_default_also_gets_the_mapping_default(
) -> rusty_db::Result<()> {
    let engine = file_engine("ambiguous_default").await?;
    let mut session = engine.session();
    session.add(&Task {
        id: 1,
        status: String::new(),
        priority: 0, // a deliberate zero, not "left unset" — still overridden
        label: "edge case".to_string(),
    });
    session.commit().await?;

    let rows = all_tasks(&engine).await?;
    assert_eq!(
        rows[0].priority, 42,
        "the explicit 0 was indistinguishable from unset"
    );

    Ok(())
}

#[tokio::test]
async fn bulk_insert_handles_a_mix_of_defaulted_and_explicit_rows() -> rusty_db::Result<()> {
    let engine = file_engine("bulk_mixed_defaults").await?;

    let tasks = [
        Task {
            id: 1,
            status: String::new(),
            priority: 0,
            label: "defaulted".to_string(),
        },
        Task {
            id: 2,
            status: "urgent".to_string(),
            priority: 9,
            label: "explicit".to_string(),
        },
    ];

    let bulk = BulkInsert::combine(tasks.iter().map(Entity::insert))?
        .expect("non-empty input produces a statement");
    engine.execute(&bulk).await?;

    assert_eq!(
        all_tasks(&engine).await?,
        vec![
            Task {
                id: 1,
                status: "pending".to_string(),
                priority: 42,
                label: "defaulted".to_string(),
            },
            Task {
                id: 2,
                status: "urgent".to_string(),
                priority: 9,
                label: "explicit".to_string(),
            },
        ]
    );

    Ok(())
}
