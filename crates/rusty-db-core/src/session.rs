use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::engine::Engine;
use crate::error::Result;
use crate::mapping::{Entity, FromRow, Identifiable, Mapped};
use crate::query::{Select, Table, ToSql};
use crate::value::Value;

/// A unit of work: queues writes made through `add`/`update`/`delete` and
/// flushes them together, in a single transaction, on `commit`. Also holds
/// an identity map: `get`/`load_all` cache decoded rows by `(type, primary
/// key)`, so loading the same row twice returns the same shared instance
/// rather than two independently-decoded copies.
///
/// The identity map is why `Session` is not `Send`: it hands out `Rc`,
/// shared, single-threaded ownership, matching how SQLAlchemy's own
/// `Session` is documented as usable from one thread/task at a time. If you
/// need to read across tasks, use `Session::engine()` (or a separately held
/// `Engine`) directly instead — those reads bypass the identity map.
///
/// Reads made through `get`/`load_all` never see writes queued but not yet
/// committed on this session (there's no autoflush), and deleting an entity
/// does not evict it from the identity map — call `clear_identity_map()`
/// (or start a new `Session`) if you need a clean slate after deletes.
pub struct Session {
    engine: Engine,
    pending: Vec<Box<dyn ToSql + Send>>,
    identity_map: HashMap<(TypeId, String), Rc<dyn Any>>,
}

impl Session {
    pub fn new(engine: Engine) -> Self {
        Session {
            engine,
            pending: Vec::new(),
            identity_map: HashMap::new(),
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

    /// Fetch a `T` by primary key through the identity map: if this session
    /// has already loaded that `(type, primary key)`, returns the same
    /// cached handle without querying the database — including whatever
    /// in-memory changes were made to it since, even if they don't match
    /// what's actually in the database right now. Otherwise queries for it,
    /// caches the result, and returns it. Returns `Ok(None)` if no matching
    /// row exists.
    pub async fn get<T>(&mut self, primary_key: impl Into<Value>) -> Result<Option<Rc<RefCell<T>>>>
    where
        T: Mapped + FromRow + 'static,
    {
        let primary_key = primary_key.into();
        let key = (TypeId::of::<T>(), primary_key.to_string());

        if let Some(cached) = self.identity_map.get(&key) {
            return Ok(Some(downcast_cached::<T>(cached)));
        }

        let pk_column =
            T::PRIMARY_KEY.expect("Session::get requires T to have a #[table(primary_key)] field");
        let table = Table::new(T::TABLE_NAME);
        let query = Select::from(&table).filter(table.col(pk_column).eq(primary_key));

        let Some(row) = self.engine.fetch_optional(&query).await? else {
            return Ok(None);
        };
        let handle = Rc::new(RefCell::new(T::from_row(&row)?));
        self.identity_map.insert(key, handle.clone());
        Ok(Some(handle))
    }

    /// Run `query` and return each row through the identity map: a row
    /// whose `(type, primary key)` is already cached returns the existing
    /// handle (its in-memory state, not a fresh decode of this row);
    /// otherwise it's decoded, cached, and returned.
    pub async fn load_all<T>(&mut self, query: &dyn ToSql) -> Result<Vec<Rc<RefCell<T>>>>
    where
        T: Identifiable + FromRow + 'static,
    {
        let rows = self.engine.fetch_all(query).await?;
        let mut handles = Vec::with_capacity(rows.len());
        for row in &rows {
            let entity = T::from_row(row)?;
            let key = (TypeId::of::<T>(), entity.primary_key_value().to_string());

            let handle = match self.identity_map.get(&key) {
                Some(cached) => downcast_cached::<T>(cached),
                None => {
                    let handle = Rc::new(RefCell::new(entity));
                    self.identity_map.insert(key, handle.clone());
                    handle
                }
            };
            handles.push(handle);
        }
        Ok(handles)
    }

    /// How many distinct `(type, primary key)` entries are currently cached.
    pub fn identity_map_len(&self) -> usize {
        self.identity_map.len()
    }

    /// Drop every cached identity-mapped instance. Handles already handed
    /// out keep working (and keep the object alive); the next `get`/
    /// `load_all` for a given row re-fetches it as a fresh instance.
    pub fn clear_identity_map(&mut self) {
        self.identity_map.clear();
    }
}

fn downcast_cached<T: 'static>(cached: &Rc<dyn Any>) -> Rc<RefCell<T>> {
    cached
        .clone()
        .downcast::<RefCell<T>>()
        .expect("identity map key collision: TypeId matched but the cached value didn't")
}
