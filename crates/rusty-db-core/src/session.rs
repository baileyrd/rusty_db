use crate::engine::Engine;
use crate::error::Result;
use crate::mapping::{Entity, Identifiable};
use crate::query::ToSql;
use crate::value::Value;

/// A unit of work: queues writes made through `add`/`update`/`delete` and
/// flushes them together, in a single transaction, on `commit`.
///
/// This is intentionally not a full ORM session — there is no identity map
/// and no autoflush. Reads always go straight through `Session::engine()`
/// (or a separately held `Engine`) and never observe writes queued but not
/// yet committed.
pub struct Session {
    engine: Engine,
    pending: Vec<Box<dyn ToSql + Send>>,
}

impl Session {
    pub fn new(engine: Engine) -> Self {
        Session {
            engine,
            pending: Vec::new(),
        }
    }

    /// The `Engine` this session commits through, for reads or raw access.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// How many writes are queued but not yet committed.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Queue an insert for `entity`; not executed until `commit()`.
    pub fn add<T: Entity>(&mut self, entity: &T) {
        self.pending.push(Box::new(entity.insert()));
    }

    /// Queue an update for `entity`; not executed until `commit()`.
    pub fn update<T: Identifiable>(&mut self, entity: &T) {
        self.pending.push(Box::new(entity.update()));
    }

    /// Queue a delete for `entity`; not executed until `commit()`.
    pub fn delete<T: Identifiable>(&mut self, entity: &T) {
        self.pending.push(Box::new(entity.delete_query()));
    }

    /// Run every queued write in a single transaction.
    ///
    /// On success, the queue is cleared. On failure, the transaction is
    /// rolled back and the queue is left exactly as it was, so the caller
    /// can fix the issue and retry `commit()`.
    pub async fn commit(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }

        let rendered: Vec<(String, Vec<Value>)> = {
            let dialect = self.engine.dialect();
            self.pending.iter().map(|op| op.to_sql(dialect)).collect()
        };

        let mut txn = self.engine.begin().await?;
        let mut failure = None;
        for (sql, params) in &rendered {
            if let Err(err) = txn.execute(sql, params).await {
                failure = Some(err);
                break;
            }
        }

        match failure {
            None => {
                txn.commit().await?;
                self.pending.clear();
                Ok(())
            }
            Some(err) => {
                let _ = txn.rollback().await;
                Err(err)
            }
        }
    }

    /// Discard queued writes without touching the database.
    pub fn rollback(&mut self) {
        self.pending.clear();
    }
}
