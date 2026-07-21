use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use crate::engine::{Engine, Transaction};
use crate::error::Result;
use crate::mapping::{Entity, FromRow, Identifiable, Mapped};
use crate::migration::{self, Migration};
use crate::query::{Select, Table, ToSql};
use crate::value::Value;

/// A unit of work: queues writes made through `add`/`update`/`delete`, and
/// autoflushes them — sends them to the database, inside one ongoing
/// transaction, without committing — before every read (`get`/`load_all`),
/// so reads always see your own not-yet-committed writes. Also holds an
/// identity map: `get`/`load_all` cache decoded rows by `(type, primary
/// key)`, so loading the same row twice returns the same shared instance
/// rather than two independently-decoded copies.
///
/// The session's transaction begins lazily, on the first `add`/`update`/
/// `delete` that gets flushed or the first `get`/`load_all` call, and stays
/// open — so nothing is visible to *other* connections — until `commit()`
/// (COMMIT) or `rollback()` (ROLLBACK, undoing every flushed-but-uncommitted
/// write too). If a flush fails partway through a batch, the whole
/// transaction is rolled back (same all-or-nothing guarantee `commit()` has
/// always had) and the queue is left as it was, so fixing the problem and
/// calling `commit()`/flushing again retries from a fresh transaction.
///
/// The identity map is why `Session` is not `Send`: it hands out `Rc`,
/// shared, single-threaded ownership, matching how SQLAlchemy's own
/// `Session` is documented as usable from one thread/task at a time. If you
/// need to read across tasks, use `Session::engine()` (or a separately held
/// `Engine`) directly instead — those reads bypass both the transaction and
/// the identity map, so they only ever see already-committed data.
///
/// `delete` also evicts the entity from the identity map immediately (not
/// deferred to flush), so a subsequent `get`/`load_all` for that primary
/// key never hands back a stale cached instance — even before `commit()`
/// runs. Note this eviction isn't undone by `rollback()`: if the delete
/// itself is rolled back, the row still exists in the database but is no
/// longer cached, so the next `get`/`load_all` for it just re-fetches and
/// re-caches it fresh. Call `clear_identity_map()` (or start a new
/// `Session`) if you need a clean slate for any other reason.
pub struct Session {
    engine: Engine,
    txn: Option<Transaction>,
    pending: Vec<Box<dyn ToSql + Send>>,
    identity_map: HashMap<(TypeId, String), Rc<dyn Any>>,
}

impl Session {
    pub fn new(engine: Engine) -> Self {
        Session {
            engine,
            txn: None,
            pending: Vec::new(),
            identity_map: HashMap::new(),
        }
    }

    /// The `Engine` this session commits through, for reads or raw access
    /// that should bypass this session's transaction and identity map.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// How many writes are queued but not yet flushed.
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Queue an insert for `entity`; not sent to the database until the
    /// next flush (an explicit `flush()`/`commit()`, or the autoflush
    /// before a `get`/`load_all` call).
    pub fn add<T: Entity>(&mut self, entity: &T) {
        self.pending.push(Box::new(entity.insert()));
    }

    /// Queue an update for `entity`; not sent until the next flush.
    pub fn update<T: Identifiable>(&mut self, entity: &T) {
        self.pending.push(Box::new(entity.update()));
    }

    /// Queue a delete for `entity` (not sent until the next flush), and
    /// evict it from the identity map immediately, so `get`/`load_all`
    /// can't hand back a stale cached instance for its primary key.
    pub fn delete<T: Identifiable + 'static>(&mut self, entity: &T) {
        let key = (TypeId::of::<T>(), entity.primary_key_value().to_string());
        self.identity_map.remove(&key);
        self.pending.push(Box::new(entity.delete_query()));
    }

    /// Send every currently-queued write to the database, inside this
    /// session's ongoing transaction (beginning one if none is open yet),
    /// without committing it. `get`/`load_all` call this automatically
    /// (autoflush); call it directly if you want writes visible to a raw
    /// query run through `session.transaction()` without triggering a read
    /// method.
    ///
    /// On success, the queue is cleared. On failure, the whole transaction
    /// (including anything flushed earlier in its lifetime) is rolled back
    /// and the queue is left exactly as it was, so fixing the problem and
    /// flushing again starts a fresh transaction and retries the batch.
    pub async fn flush(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }

        let rendered: Vec<(String, Vec<Value>)> = {
            let dialect = self.engine.dialect();
            self.pending.iter().map(|op| op.to_sql(dialect)).collect()
        };

        if self.txn.is_none() {
            self.txn = Some(self.engine.begin().await?);
        }
        let txn = self.txn.as_mut().expect("just set");

        for (sql, params) in &rendered {
            if let Err(err) = txn.execute(sql, params).await {
                if let Some(txn) = self.txn.take() {
                    let _ = txn.rollback().await;
                }
                return Err(err);
            }
        }

        self.pending.clear();
        Ok(())
    }

    /// Apply every migration in `migrations` not already recorded as
    /// applied (bookkeeping table `_rusty_db_migrations`), the same as
    /// `Migrator::up`, but running them inside this session's own ongoing
    /// transaction — autoflushing queued writes first, beginning the
    /// transaction if none is open yet — rather than each migration in its
    /// own transaction. This means the migrations share atomicity with
    /// everything else in this unit of work: nothing they do is visible to
    /// another connection, and none of it takes effect at all, until this
    /// session's `commit()` runs.
    ///
    /// On failure, the whole transaction (including anything flushed or
    /// migrated earlier in its lifetime) is rolled back, same as `flush`.
    pub async fn migrate(&mut self, migrations: &[Migration]) -> Result<Vec<i64>> {
        self.migrate_with_table(migrations, "_rusty_db_migrations")
            .await
    }

    /// Like `migrate`, using a bookkeeping table name other than the
    /// default `_rusty_db_migrations` (matching `Migrator::with_table`).
    pub async fn migrate_with_table(
        &mut self,
        migrations: &[Migration],
        table: &str,
    ) -> Result<Vec<i64>> {
        self.flush().await?;

        if self.txn.is_none() {
            self.txn = Some(self.engine.begin().await?);
        }

        let dialect = self.engine.dialect();
        let txn = self.txn.as_mut().expect("just set");
        let result = migration::apply_pending(txn, dialect, table, migrations).await;

        if let Err(err) = result {
            if let Some(txn) = self.txn.take() {
                let _ = txn.rollback().await;
            }
            return Err(err);
        }

        result
    }

    /// Flush any remaining queued writes, then commit this session's
    /// transaction (a no-op if nothing was ever flushed or read, so no
    /// transaction is open).
    pub async fn commit(&mut self) -> Result<()> {
        self.flush().await?;
        if let Some(txn) = self.txn.take() {
            txn.commit().await?;
        }
        Ok(())
    }

    /// Discard queued writes and, if a transaction is open, roll it back —
    /// undoing any writes already flushed into it, not just the still-queued
    /// ones. The identity map is left as-is; call `clear_identity_map()` too
    /// if you need a clean slate.
    pub async fn rollback(&mut self) -> Result<()> {
        self.pending.clear();
        if let Some(txn) = self.txn.take() {
            txn.rollback().await?;
        }
        Ok(())
    }

    /// Fetch a `T` by primary key through the identity map: if this session
    /// has already loaded that `(type, primary key)`, returns the same
    /// cached handle without querying the database — including whatever
    /// in-memory changes were made to it since, even if they don't match
    /// what's actually in the database right now. Otherwise flushes queued
    /// writes (autoflush) and queries for it — through this session's
    /// transaction if one is open, so it sees those writes — caches the
    /// result, and returns it. Returns `Ok(None)` if no matching row exists.
    pub async fn get<T>(&mut self, primary_key: impl Into<Value>) -> Result<Option<Rc<RefCell<T>>>>
    where
        T: Mapped + FromRow + 'static,
    {
        let primary_key = primary_key.into();
        let key = (TypeId::of::<T>(), primary_key.to_string());

        if let Some(cached) = self.identity_map.get(&key) {
            return Ok(Some(downcast_cached::<T>(cached)));
        }

        self.flush().await?;

        let pk_column =
            T::PRIMARY_KEY.expect("Session::get requires T to have a #[table(primary_key)] field");
        let table = Table::new(T::TABLE_NAME);
        let query = Select::from(&table).filter(table.col(pk_column).eq(primary_key));

        let dialect = self.engine.dialect();
        let row_opt = match self.txn.as_mut() {
            Some(txn) => txn.fetch_optional(&query, dialect).await?,
            None => self.engine.fetch_optional(&query).await?,
        };

        let Some(row) = row_opt else {
            return Ok(None);
        };
        let handle = Rc::new(RefCell::new(T::from_row(&row)?));
        self.identity_map.insert(key, handle.clone());
        Ok(Some(handle))
    }

    /// Flush queued writes (autoflush), run `query` through this session's
    /// transaction if one is open (so it sees those writes), and return
    /// each row through the identity map: a row whose `(type, primary key)`
    /// is already cached returns the existing handle (its in-memory state,
    /// not a fresh decode of this row); otherwise it's decoded, cached, and
    /// returned.
    pub async fn load_all<T>(&mut self, query: &dyn ToSql) -> Result<Vec<Rc<RefCell<T>>>>
    where
        T: Identifiable + FromRow + 'static,
    {
        self.flush().await?;

        let dialect = self.engine.dialect();
        let rows = match self.txn.as_mut() {
            Some(txn) => txn.fetch_all(query, dialect).await?,
            None => self.engine.fetch_all(query).await?,
        };

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
