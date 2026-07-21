#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `BulkInsert`/`Session::add_all`: inserting many rows as one
//! multi-row `INSERT` statement instead of one statement per row.

use rusty_db::prelude::*;

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "users")]
struct User {
    #[table(primary_key)]
    id: i64,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "orders")]
struct Order {
    #[table(primary_key)]
    id: i64,
    amount: i64,
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_bulk_insert_{name}_{}.sqlite3",
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
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE orders (id INTEGER PRIMARY KEY, amount INTEGER NOT NULL)",
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
async fn bulk_insert_combines_rows_into_one_statement() -> rusty_db::Result<()> {
    let users = [
        User {
            id: 1,
            name: "ada".to_string(),
        },
        User {
            id: 2,
            name: "grace".to_string(),
        },
        User {
            id: 3,
            name: "linus".to_string(),
        },
    ];

    let bulk = BulkInsert::combine(users.iter().map(Entity::insert))?
        .expect("non-empty input produces a statement");
    assert_eq!(bulk.row_count(), 3);

    let (sql, params) = bulk.to_sql(&rusty_db::dialect::QuestionMarkDialect);
    // One INSERT, three parenthesized value groups.
    assert_eq!(sql.matches("INSERT INTO").count(), 1);
    assert_eq!(sql.matches('(').count(), 4); // column list + 3 row groups
    assert_eq!(params.len(), 6); // 2 columns * 3 rows

    Ok(())
}

#[tokio::test]
async fn combining_zero_inserts_yields_none() -> rusty_db::Result<()> {
    let inserts: Vec<Insert> = Vec::new();
    assert!(BulkInsert::combine(inserts)?.is_none());
    Ok(())
}

#[tokio::test]
async fn combining_inserts_from_different_tables_errors() {
    let user = User {
        id: 1,
        name: "ada".to_string(),
    }
    .insert();
    let order = Order { id: 1, amount: 100 }.insert();

    let outcome = BulkInsert::combine(vec![user, order]);
    assert!(
        outcome.is_err(),
        "expected combining different tables to be rejected"
    );
}

#[tokio::test]
async fn session_add_all_inserts_every_row_in_one_round_trip() -> rusty_db::Result<()> {
    let engine = file_engine("session_add_all").await?;
    let mut session = engine.session();

    let users = vec![
        User {
            id: 1,
            name: "ada".to_string(),
        },
        User {
            id: 2,
            name: "grace".to_string(),
        },
        User {
            id: 3,
            name: "linus".to_string(),
        },
    ];
    session.add_all(&users);
    assert_eq!(
        session.pending_len(),
        1,
        "one bulk statement, not one per row"
    );

    session.commit().await?;

    assert_eq!(all_users(&engine).await?, users);

    Ok(())
}

#[tokio::test]
async fn session_add_all_with_an_empty_slice_is_a_no_op() -> rusty_db::Result<()> {
    let engine = file_engine("empty_add_all").await?;
    let mut session = engine.session();

    session.add_all::<User>(&[]);
    assert_eq!(session.pending_len(), 0);
    session.commit().await?;

    assert_eq!(all_users(&engine).await?, Vec::new());

    Ok(())
}

#[tokio::test]
async fn a_failing_bulk_insert_rolls_back_the_whole_batch() -> rusty_db::Result<()> {
    let engine = file_engine("bulk_insert_conflict").await?;
    let mut session = engine.session();
    session.add(&User {
        id: 1,
        name: "ada".to_string(),
    });
    session.commit().await?;

    // The second row's id collides with the one already committed above.
    let mut new_session = engine.session();
    new_session.add_all(&[
        User {
            id: 2,
            name: "grace".to_string(),
        },
        User {
            id: 1,
            name: "duplicate".to_string(),
        },
    ]);
    let outcome = new_session.commit().await;
    assert!(outcome.is_err(), "expected the bulk insert to fail");

    // Neither row landed — not even the non-conflicting one — since it's
    // one statement, one transaction.
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
async fn bulk_inserted_rows_are_visible_through_the_query_builder() -> rusty_db::Result<()> {
    let engine = file_engine("visible_via_query_builder").await?;
    let bulk = BulkInsert::combine(
        [
            User {
                id: 1,
                name: "ada".to_string(),
            },
            User {
                id: 2,
                name: "grace".to_string(),
            },
        ]
        .iter()
        .map(Entity::insert),
    )?
    .unwrap();

    engine.execute(&bulk).await?;

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
