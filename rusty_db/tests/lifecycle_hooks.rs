#![cfg(all(feature = "sqlite", feature = "derive"))]

//! Exercises `Lifecycle`/`Session::add_mut`/`update_mut`/`delete_mut`:
//! before/after hooks and `validate()` run around a write, entirely
//! opt-in — `add`/`update`/`delete` themselves stay unhooked, infallible,
//! and unaffected by any of this.

use std::cell::RefCell;

use rusty_db::prelude::*;

thread_local! {
    static EVENTS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn record(event: impl Into<String>) {
    EVENTS.with(|events| events.borrow_mut().push(event.into()));
}

fn take_events() -> Vec<String> {
    EVENTS.with(|events| std::mem::take(&mut *events.borrow_mut()))
}

#[derive(Debug, Clone, PartialEq, Mapped)]
#[table(name = "documents")]
struct Document {
    #[table(primary_key)]
    id: i64,
    title: String,
    word_count: i64,
}

impl Lifecycle for Document {
    fn before_insert(&mut self) {
        self.title = self.title.trim().to_string();
        record(format!("before_insert({})", self.id));
    }

    fn after_insert(&self) {
        record(format!("after_insert({})", self.id));
    }

    fn before_update(&mut self) {
        record(format!("before_update({})", self.id));
    }

    fn after_update(&self) {
        record(format!("after_update({})", self.id));
    }

    fn before_delete(&mut self) {
        record(format!("before_delete({})", self.id));
    }

    fn after_delete(&self) {
        record(format!("after_delete({})", self.id));
    }

    fn validate(&self) -> rusty_db::Result<()> {
        if self.word_count < 0 {
            return Err(rusty_db::Error::QueryBuilder(
                "word_count cannot be negative".to_string(),
            ));
        }
        Ok(())
    }
}

async fn file_engine(name: &str) -> rusty_db::Result<Engine> {
    let path = std::env::temp_dir().join(format!(
        "rusty_db_lifecycle_hooks_{name}_{}.sqlite3",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let engine = SqliteDriver::engine(&url).await?;
    engine
        .connect()
        .await?
        .execute(
            "CREATE TABLE documents (id INTEGER PRIMARY KEY, title TEXT NOT NULL, \
             word_count INTEGER NOT NULL)",
            &[],
        )
        .await?;
    Ok(engine)
}

#[tokio::test]
async fn before_insert_can_mutate_the_entity_before_it_is_queued() -> rusty_db::Result<()> {
    take_events();
    let engine = file_engine("before_insert_mutates").await?;
    let mut session = engine.session();

    let mut doc = Document {
        id: 1,
        title: "  padded title  ".to_string(),
        word_count: 2,
    };
    session.add_mut(&mut doc)?;
    session.commit().await?;

    let rows: Vec<Document> = engine
        .fetch_all_as(&Select::from(&Document::table()))
        .await?;
    assert_eq!(rows[0].title, "padded title");

    Ok(())
}

#[tokio::test]
async fn validate_rejects_the_write_before_anything_is_queued() -> rusty_db::Result<()> {
    take_events();
    let engine = file_engine("validate_rejects").await?;
    let mut session = engine.session();

    let mut doc = Document {
        id: 1,
        title: "bad".to_string(),
        word_count: -5,
    };
    let outcome = session.add_mut(&mut doc);
    assert!(outcome.is_err());
    assert_eq!(
        session.pending_len(),
        0,
        "a failed validate() queues nothing"
    );

    Ok(())
}

#[tokio::test]
async fn after_insert_fires_only_once_the_write_actually_succeeds() -> rusty_db::Result<()> {
    take_events();
    let engine = file_engine("after_insert_timing").await?;
    let mut session = engine.session();

    let mut doc = Document {
        id: 1,
        title: "ok".to_string(),
        word_count: 1,
    };
    session.add_mut(&mut doc)?;
    // Queued but not yet flushed: only before_insert has run so far.
    assert_eq!(take_events(), vec!["before_insert(1)".to_string()]);

    session.commit().await?;
    assert_eq!(take_events(), vec!["after_insert(1)".to_string()]);

    Ok(())
}

#[tokio::test]
async fn after_insert_never_fires_for_a_write_that_never_flushes() -> rusty_db::Result<()> {
    take_events();
    let engine = file_engine("after_insert_rollback").await?;
    let mut session = engine.session();

    let mut doc = Document {
        id: 1,
        title: "ok".to_string(),
        word_count: 1,
    };
    session.add_mut(&mut doc)?;
    take_events(); // discard before_insert's record

    session.rollback().await?; // discards the queue without ever flushing it
    assert_eq!(
        take_events(),
        Vec::<String>::new(),
        "a rolled-back write never reaches flush, so after_insert never fires"
    );

    Ok(())
}

#[tokio::test]
async fn update_mut_runs_before_update_then_after_update_once_flushed() -> rusty_db::Result<()> {
    take_events();
    let engine = file_engine("update_mut_hooks").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        title: "draft".to_string(),
        word_count: 10,
    });
    session.commit().await?;
    take_events();

    let mut doc = Document {
        id: 1,
        title: "final".to_string(),
        word_count: 20,
    };
    session.update_mut(&mut doc)?;
    session.commit().await?;

    assert_eq!(
        take_events(),
        vec![
            "before_update(1)".to_string(),
            "after_update(1)".to_string()
        ]
    );

    let rows: Vec<Document> = engine
        .fetch_all_as(&Select::from(&Document::table()))
        .await?;
    assert_eq!(rows[0].word_count, 20);

    Ok(())
}

#[tokio::test]
async fn update_mut_also_rejects_via_validate() -> rusty_db::Result<()> {
    take_events();
    let engine = file_engine("update_mut_validate").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        title: "draft".to_string(),
        word_count: 10,
    });
    session.commit().await?;

    let mut doc = Document {
        id: 1,
        title: "final".to_string(),
        word_count: -1,
    };
    let outcome = session.update_mut(&mut doc);
    assert!(outcome.is_err());
    assert_eq!(session.pending_len(), 0);

    Ok(())
}

#[tokio::test]
async fn delete_mut_runs_before_delete_and_after_delete_but_never_validate() -> rusty_db::Result<()>
{
    take_events();
    let engine = file_engine("delete_mut_hooks").await?;
    let mut session = engine.session();
    session.add(&Document {
        id: 1,
        title: "temp".to_string(),
        word_count: -100, // would fail validate() if delete_mut ever called it
    });
    session.commit().await?;
    take_events();

    let mut doc = Document {
        id: 1,
        title: "temp".to_string(),
        word_count: -100,
    };
    session.delete_mut(&mut doc)?;
    session.commit().await?;

    assert_eq!(
        take_events(),
        vec![
            "before_delete(1)".to_string(),
            "after_delete(1)".to_string()
        ]
    );

    let rows: Vec<Document> = engine
        .fetch_all_as(&Select::from(&Document::table()))
        .await?;
    assert!(rows.is_empty());

    Ok(())
}

#[tokio::test]
async fn add_stays_completely_unhooked() -> rusty_db::Result<()> {
    take_events();
    let engine = file_engine("add_unaffected").await?;
    let mut session = engine.session();

    session.add(&Document {
        id: 1,
        title: "  untouched  ".to_string(),
        word_count: -999, // would fail validate() if add() ran Lifecycle at all
    });
    session.commit().await?;

    assert_eq!(
        take_events(),
        Vec::<String>::new(),
        "add() never touches Lifecycle"
    );

    let rows: Vec<Document> = engine
        .fetch_all_as(&Select::from(&Document::table()))
        .await?;
    assert_eq!(
        rows[0].title, "  untouched  ",
        "no before_insert ran to trim it"
    );

    Ok(())
}

#[tokio::test]
async fn after_insert_never_fires_when_a_later_item_in_the_same_flush_fails() -> rusty_db::Result<()>
{
    take_events();
    let engine = file_engine("after_insert_batch_failure").await?;
    let mut session = engine.session();

    // Commit a row up front so a later add() with the same id collides.
    session.add(&Document {
        id: 2,
        title: "existing".to_string(),
        word_count: 0,
    });
    session.commit().await?;
    take_events();

    let mut doc1 = Document {
        id: 1,
        title: "first".to_string(),
        word_count: 1,
    };
    session.add_mut(&mut doc1)?;
    take_events(); // discard before_insert(1); only after_insert(1) matters below

    session.add(&Document {
        id: 2,
        title: "duplicate".to_string(),
        word_count: 2,
    }); // primary key collision — this write fails at flush time

    let outcome = session.commit().await;
    assert!(outcome.is_err(), "the duplicate primary key should fail");

    assert_eq!(
        take_events(),
        Vec::<String>::new(),
        "doc1's insert ran inside the same transaction the duplicate-key failure rolled back — \
         after_insert must not fire for a write that ends up undone"
    );

    Ok(())
}
