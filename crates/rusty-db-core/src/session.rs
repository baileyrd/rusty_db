use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::rc::Rc;

use crate::audit::{self, AuditEntry, AuditOperation};
use crate::engine::{Engine, Transaction};
use crate::error::{Error, Result};
use crate::mapping::{Entity, FromRow, Identifiable, Mapped};
use crate::migration::{self, Migration};
use crate::query::{BulkInsert, Column, Delete, Expr, Insert, Select, Table, ToSql, Update};
use crate::value::Value;

/// One write queued by `add`/`update`/`delete`, tagged with what it's for
/// (used when audit logging is enabled) alongside the rendered
/// `Insert`/`Update`/`Delete` itself. `requires_row_affected` is set for
/// an optimistic-locked `update`/`delete` (a type with
/// `Mapped::VERSION_COLUMN`), whose `WHERE` clause already encodes "only
/// if the version still matches" — `flush` turns a resulting
/// zero-rows-affected outcome into `Error::Conflict` instead of treating
/// it as a silent no-op.
struct PendingWrite {
    table: &'static str,
    operation: AuditOperation,
    query: Box<dyn ToSql + Send>,
    requires_row_affected: bool,
}

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
    pending: Vec<PendingWrite>,
    identity_map: HashMap<(TypeId, String), Rc<dyn Any>>,
    audit_enabled: bool,
    audit_table: &'static str,
    next_savepoint_id: u64,
    before_flush_hooks: Vec<Hook>,
    after_flush_hooks: Vec<Hook>,
    before_commit_hooks: Vec<Hook>,
    after_commit_hooks: Vec<Hook>,
    after_rollback_hooks: Vec<Hook>,
}

/// A callback registered with `on_before_flush`/`on_after_flush`/
/// `on_before_commit`/`on_after_commit`/`on_after_rollback`. Plain
/// `FnMut()` (no `Send` bound needed, matching `Session` itself, and no
/// async support — these fire at a specific synchronous point inside an
/// already-`async fn`, not as their own awaited step).
type Hook = Box<dyn FnMut()>;

/// A point inside a `Session`'s transaction created by `Session::savepoint`,
/// for `rollback_to_savepoint`/`release_savepoint`. Its name is generated
/// (`"rusty_db_sp_0"`, `"rusty_db_sp_1"`, ...) — plain ASCII, so it never
/// needs identifier quoting — and unique within the session, so nested
/// savepoints (a savepoint created while another is still open) just work.
#[derive(Debug)]
pub struct Savepoint {
    name: String,
}

impl Session {
    pub fn new(engine: Engine) -> Self {
        Session {
            engine,
            txn: None,
            pending: Vec::new(),
            identity_map: HashMap::new(),
            audit_enabled: false,
            audit_table: "_rusty_db_audit_log",
            next_savepoint_id: 0,
            before_flush_hooks: Vec::new(),
            after_flush_hooks: Vec::new(),
            before_commit_hooks: Vec::new(),
            after_commit_hooks: Vec::new(),
            after_rollback_hooks: Vec::new(),
        }
    }

    /// Registers a callback to run immediately before this session sends
    /// its currently-queued writes to the database — an explicit
    /// `flush()`/`commit()`, or the autoflush before a `get`/`load_all`/
    /// `audit_log`/`query` call — but only when there's actually something
    /// queued to send; a flush with nothing pending is a no-op and doesn't
    /// fire this. Hooks run in registration order.
    pub fn on_before_flush(&mut self, hook: impl FnMut() + 'static) {
        self.before_flush_hooks.push(Box::new(hook));
    }

    /// Like `on_before_flush`, but fires right after the flush actually
    /// succeeds (never after a failed one — the transaction rolls back
    /// instead, so nothing to "after" there).
    pub fn on_after_flush(&mut self, hook: impl FnMut() + 'static) {
        self.after_flush_hooks.push(Box::new(hook));
    }

    /// Registers a callback to run at the very start of `commit()`, before
    /// it flushes any queued writes or issues `COMMIT` — fires every time
    /// `commit()` is called, whether or not there ends up being anything
    /// to actually commit.
    pub fn on_before_commit(&mut self, hook: impl FnMut() + 'static) {
        self.before_commit_hooks.push(Box::new(hook));
    }

    /// Registers a callback to run right after `commit()` successfully
    /// issues `COMMIT` — only when a transaction was actually open to
    /// commit; calling `commit()` when nothing was ever flushed or read
    /// (so no transaction was ever opened in the first place) doesn't
    /// fire this.
    pub fn on_after_commit(&mut self, hook: impl FnMut() + 'static) {
        self.after_commit_hooks.push(Box::new(hook));
    }

    /// Registers a callback to run right after `rollback()` successfully
    /// issues `ROLLBACK` — only when a transaction was actually open to
    /// roll back.
    pub fn on_after_rollback(&mut self, hook: impl FnMut() + 'static) {
        self.after_rollback_hooks.push(Box::new(hook));
    }

    /// Record every write this session flushes into an append-only audit
    /// log (table `_rusty_db_audit_log` by default — see
    /// `with_audit_log_table` to use another), inside the same
    /// transaction as the write itself. See `audit_log` to read it back.
    pub fn with_audit_log(mut self) -> Self {
        self.audit_enabled = true;
        self
    }

    /// Like `with_audit_log`, using an audit table name other than the
    /// default `_rusty_db_audit_log` (matching `Migrator::with_table`).
    pub fn with_audit_log_table(mut self, table: &'static str) -> Self {
        self.audit_enabled = true;
        self.audit_table = table;
        self
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
        self.pending.push(PendingWrite {
            table: T::TABLE_NAME,
            operation: AuditOperation::Insert,
            query: Box::new(entity.insert()),
            requires_row_affected: false,
        });
    }

    /// Queue inserts for every entity in `entities` as a single bulk
    /// `INSERT` (`BulkInsert`) — one statement (and, at flush time, one
    /// round trip) for the whole batch, instead of one per entity as
    /// repeated `add` calls would. A no-op for an empty slice.
    pub fn add_all<T: Entity>(&mut self, entities: &[T]) {
        let Some(bulk) = BulkInsert::combine(entities.iter().map(Entity::insert))
            .expect("entities of the same type always share a table and column order")
        else {
            return;
        };
        self.pending.push(PendingWrite {
            table: T::TABLE_NAME,
            operation: AuditOperation::Insert,
            query: Box::new(bulk),
            requires_row_affected: false,
        });
    }

    /// Queue an update for `entity`; not sent until the next flush.
    ///
    /// If `T` has a `#[table(version)]` field (optimistic locking),
    /// `flush` errors with `Error::Conflict` instead of silently doing
    /// nothing when this update's `WHERE <primary key> = ... AND
    /// <version> = ...` matches no row — meaning either the row is gone
    /// or, more likely, someone else has already changed it since `entity`
    /// was loaded.
    pub fn update<T: Identifiable>(&mut self, entity: &T) {
        self.pending.push(PendingWrite {
            table: T::TABLE_NAME,
            operation: AuditOperation::Update,
            query: Box::new(entity.update()),
            requires_row_affected: T::VERSION_COLUMN.is_some(),
        });
    }

    /// Queue a delete for `entity` (not sent until the next flush), and
    /// evict it from the identity map immediately, so `get`/`load_all`
    /// can't hand back a stale cached instance for its primary key.
    ///
    /// With a `#[table(soft_delete)]` column, this marks the row (`SET
    /// <column> = true`) instead of actually removing it — `entity`'s own
    /// `delete_query()` (a real `DELETE`) is bypassed entirely; use it
    /// directly if you ever need a hard delete on a soft-deletable type.
    ///
    /// Same optimistic-locking behavior as `update`: with a
    /// `#[table(version)]` field, a delete that matches no row (version
    /// mismatch, or already gone) errors with `Error::Conflict` rather
    /// than succeeding silently.
    pub fn delete<T: Identifiable + 'static>(&mut self, entity: &T) {
        let key = (TypeId::of::<T>(), entity.primary_key_value().to_string());
        self.identity_map.remove(&key);

        let query: Box<dyn ToSql + Send> = match T::SOFT_DELETE_COLUMN {
            Some(soft_delete_column) => {
                let pk_column = T::PRIMARY_KEY.expect(
                    "#[table(soft_delete)] requires a #[table(primary_key)] field too \
                     (enforced when #[derive(Mapped)] expands)",
                );
                let table = Table::new(T::TABLE_NAME);
                Box::new(
                    Update::table(&table)
                        .set(soft_delete_column, true)
                        .filter(table.col(pk_column).eq(entity.primary_key_value())),
                )
            }
            None => Box::new(entity.delete_query()),
        };

        self.pending.push(PendingWrite {
            table: T::TABLE_NAME,
            operation: AuditOperation::Delete,
            query,
            requires_row_affected: T::VERSION_COLUMN.is_some(),
        });
    }

    /// Queue an arbitrary `UPDATE` against `T`'s table — not bound to any
    /// single entity — for changing every row matching a filter in one
    /// statement, instead of loading each one and calling `update` per
    /// instance. Not sent until the next flush, and audit-logged the same
    /// way `add`/`update`/`delete` are when enabled.
    ///
    /// Bypasses the identity map entirely: any already-cached instances of
    /// `T` this happens to touch are not updated in memory and will look
    /// stale until evicted (`clear_identity_map()`) or reloaded. There's
    /// also no optimistic-locking check here even if `T` has a
    /// `#[table(version)]` field — matching zero rows is a normal, silent
    /// outcome for a filter-scoped bulk update, unlike a single entity's
    /// `update`, which always expects to match the one row it was loaded
    /// from.
    ///
    /// ```
    /// # use rusty_db_core::{Mapped, Session, Table, Update};
    /// # fn example<T: Mapped>(session: &mut Session) {
    /// let table = Table::new(T::TABLE_NAME);
    /// session.bulk_update::<T>(
    ///     Update::table(&table)
    ///         .set("active", false)
    ///         .filter(table.col("last_login").lt("2020-01-01")),
    /// );
    /// # }
    /// ```
    pub fn bulk_update<T: Mapped>(&mut self, update: Update) {
        self.pending.push(PendingWrite {
            table: T::TABLE_NAME,
            operation: AuditOperation::Update,
            query: Box::new(update),
            requires_row_affected: false,
        });
    }

    /// Queue an arbitrary `DELETE` against `T`'s table — not bound to any
    /// single entity — for removing every row matching a filter in one
    /// statement. Not sent until the next flush.
    ///
    /// Bypasses the identity map and any `#[table(soft_delete)]` column:
    /// this is always a real, hard `DELETE` (use `Session::delete` for the
    /// soft-delete-aware, single-entity path). Any already-cached
    /// instances of `T` this happens to touch stay cached; evict them
    /// yourself (`clear_identity_map()`) if that matters.
    pub fn bulk_delete<T: Mapped>(&mut self, delete: Delete) {
        self.pending.push(PendingWrite {
            table: T::TABLE_NAME,
            operation: AuditOperation::Delete,
            query: Box::new(delete),
            requires_row_affected: false,
        });
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

        for hook in &mut self.before_flush_hooks {
            hook();
        }

        let rendered: Vec<(&'static str, AuditOperation, String, Vec<Value>, bool)> = {
            let dialect = self.engine.dialect();
            self.pending
                .iter()
                .map(|write| {
                    let (sql, params) = write.query.to_sql(dialect);
                    (
                        write.table,
                        write.operation,
                        sql,
                        params,
                        write.requires_row_affected,
                    )
                })
                .collect()
        };

        if self.txn.is_none() {
            self.txn = Some(self.engine.begin().await?);
        }

        if self.audit_enabled {
            self.ensure_audit_table().await?;
        }

        for (table, operation, sql, params, requires_row_affected) in &rendered {
            let affected = self.execute_or_rollback(sql, params).await?;
            if *requires_row_affected && affected == 0 {
                if let Some(txn) = self.txn.take() {
                    let _ = txn.rollback().await;
                }
                return Err(Error::Conflict(format!(
                    "no row in {table:?} matched the expected primary key and version — \
                     it was likely changed or deleted since this instance was loaded"
                )));
            }

            if self.audit_enabled {
                let audit_table = Table::new(self.audit_table);
                let record = Insert::into_table(&audit_table)
                    .value("table_name", *table)
                    .value("operation", operation.as_str())
                    .value("sql_text", sql.clone())
                    .value("params_text", audit::params_to_text(params));
                let dialect = self.engine.dialect();
                let (audit_sql, audit_params) = record.to_sql(dialect);
                self.execute_or_rollback(&audit_sql, &audit_params).await?;
            }
        }

        self.pending.clear();

        for hook in &mut self.after_flush_hooks {
            hook();
        }

        Ok(())
    }

    /// Runs one statement through the open transaction, rolling the whole
    /// transaction back and returning the error if it fails — the shared
    /// all-or-nothing behavior every write in `flush` (the real write and,
    /// when enabled, its audit-log entry) has. Returns the number of rows
    /// affected, so callers can detect e.g. an optimistic-locked update
    /// that matched no row.
    async fn execute_or_rollback(&mut self, sql: &str, params: &[Value]) -> Result<u64> {
        let result = self
            .txn
            .as_mut()
            .expect("transaction just opened by flush")
            .execute(sql, params)
            .await;
        match result {
            Ok(affected) => Ok(affected),
            Err(err) => {
                if let Some(txn) = self.txn.take() {
                    let _ = txn.rollback().await;
                }
                Err(err)
            }
        }
    }

    async fn ensure_audit_table(&mut self) -> Result<()> {
        if self.txn.is_none() {
            self.txn = Some(self.engine.begin().await?);
        }
        let quoted = self.engine.dialect().quote_ident(self.audit_table);
        self.execute_or_rollback(&audit::table_ddl(&quoted), &[])
            .await?;
        Ok(())
    }

    /// Reads back this session's audit log (autoflushing first, and
    /// reading through this session's transaction if one is open — the
    /// same "see your own writes" behavior `get`/`load_all` have), oldest
    /// entry first. Only meaningful when this session was built with
    /// `with_audit_log`/`with_audit_log_table`.
    pub async fn audit_log(&mut self) -> Result<Vec<AuditEntry>> {
        self.flush().await?;
        self.ensure_audit_table().await?;

        let table = Table::new(self.audit_table);
        let query = Select::from(&table);
        let dialect = self.engine.dialect();
        let rows = match self.txn.as_mut() {
            Some(txn) => txn.fetch_all(&query, dialect).await?,
            None => self.engine.fetch_all(&query).await?,
        };

        rows.iter()
            .map(|row| {
                let operation_text = row.get_by_name::<String>("operation")?;
                let operation = AuditOperation::parse(&operation_text).ok_or_else(|| {
                    Error::Database(format!("unrecognized audit operation {operation_text:?}"))
                })?;
                Ok(AuditEntry {
                    table: row.get_by_name("table_name")?,
                    operation,
                    sql: row.get_by_name("sql_text")?,
                    params_text: row.get_by_name("params_text")?,
                })
            })
            .collect()
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
    /// transaction is open). Runs `on_before_commit` hooks first (always)
    /// and `on_after_commit` hooks last (only if a transaction actually
    /// existed to commit) — see those for details.
    pub async fn commit(&mut self) -> Result<()> {
        for hook in &mut self.before_commit_hooks {
            hook();
        }
        self.flush().await?;
        if let Some(txn) = self.txn.take() {
            txn.commit().await?;
            for hook in &mut self.after_commit_hooks {
                hook();
            }
        }
        Ok(())
    }

    /// Discard queued writes and, if a transaction is open, roll it back —
    /// undoing any writes already flushed into it, not just the still-queued
    /// ones. The identity map is left as-is; call `clear_identity_map()` too
    /// if you need a clean slate. Runs `on_after_rollback` hooks, but only
    /// if a transaction actually existed to roll back.
    pub async fn rollback(&mut self) -> Result<()> {
        self.pending.clear();
        if let Some(txn) = self.txn.take() {
            txn.rollback().await?;
            for hook in &mut self.after_rollback_hooks {
                hook();
            }
        }
        Ok(())
    }

    /// Marks a point inside this session's ongoing transaction that
    /// `rollback_to_savepoint` can later undo back to, without aborting the
    /// whole transaction — for a sub-unit of work that might fail and need
    /// undoing on its own, while everything before it (and, once you're
    /// past that risk, everything after) still commits normally.
    ///
    /// Autoflushes first (the same "see your own writes" boundary `get`/
    /// `load_all` use) and begins the session's transaction if none is open
    /// yet, so the savepoint's boundary is well-defined regardless of what
    /// was queued before this call.
    pub async fn savepoint(&mut self) -> Result<Savepoint> {
        self.flush().await?;
        if self.txn.is_none() {
            self.txn = Some(self.engine.begin().await?);
        }
        let name = format!("rusty_db_sp_{}", self.next_savepoint_id);
        self.next_savepoint_id += 1;
        self.execute_or_rollback(&format!("SAVEPOINT {name}"), &[])
            .await?;
        Ok(Savepoint { name })
    }

    /// Undoes every write flushed — or queued but not yet flushed, which
    /// are simply discarded, since they never reached the database — since
    /// `savepoint` was created, without aborting the rest of the
    /// transaction. The session keeps going afterward: further
    /// `add`/`update`/`delete`/flushes continue in the same, still-open
    /// transaction, and `savepoint` itself can still be rolled back to
    /// again later if you keep writing past it.
    pub async fn rollback_to_savepoint(&mut self, savepoint: &Savepoint) -> Result<()> {
        self.pending.clear();
        self.execute_or_rollback(&format!("ROLLBACK TO SAVEPOINT {}", savepoint.name), &[])
            .await?;
        Ok(())
    }

    /// Flushes anything queued since `savepoint`, then releases it —
    /// keeping its effects as part of the ongoing transaction, just no
    /// longer available to roll back to on its own. Not required before
    /// `commit()`/`rollback()`: an unreleased savepoint is
    /// released/undone right along with the whole transaction either way.
    pub async fn release_savepoint(&mut self, savepoint: Savepoint) -> Result<()> {
        self.flush().await?;
        self.execute_or_rollback(&format!("RELEASE SAVEPOINT {}", savepoint.name), &[])
            .await?;
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
    ///
    /// With a `#[table(soft_delete)]` column, a row already marked deleted
    /// is treated the same as one that was never there: `Ok(None)`.
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
        let mut query = Select::from(&table).filter(table.col(pk_column).eq(primary_key));
        if let Some(not_deleted) = T::not_deleted_filter() {
            query = query.filter(not_deleted);
        }

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

    /// Like `load_all`, but every row of `T`'s table — excluding
    /// soft-deleted ones, for a `#[table(soft_delete)]` type (see
    /// `Mapped::not_deleted_filter`). For a type with no soft-delete
    /// column, this is simply every row.
    pub async fn load_active<T>(&mut self) -> Result<Vec<Rc<RefCell<T>>>>
    where
        T: Identifiable + FromRow + 'static,
    {
        let table = Table::new(T::TABLE_NAME);
        let mut query = Select::from(&table);
        if let Some(not_deleted) = T::not_deleted_filter() {
            query = query.filter(not_deleted);
        }
        self.load_all(&query).await
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

    /// A fluent, type-bound query against `T`'s own table, instead of
    /// building a `Select::from(&T::table())` yourself and passing it to
    /// `load_all`. Terminal methods (`all`/`first`) go through `load_all`
    /// under the hood, so results are identity-mapped exactly the same way.
    ///
    /// ```
    /// # use rusty_db_core::{FromRow, Identifiable, Mapped, Session, Table};
    /// # async fn example<T: Identifiable + FromRow + 'static>(session: &mut Session) -> rusty_db_core::Result<()> {
    /// let table = Table::new(T::TABLE_NAME);
    /// let recent = session
    ///     .query::<T>()
    ///     .filter(table.col("active").eq(true))
    ///     .order_by(table.col("id").desc())
    ///     .limit(10)
    ///     .all()
    ///     .await?;
    /// # let _ = recent;
    /// # Ok(())
    /// # }
    /// ```
    pub fn query<T: Identifiable>(&mut self) -> SessionQuery<'_, T> {
        SessionQuery {
            session: self,
            select: Select::from(&Table::new(T::TABLE_NAME)),
            marker: PhantomData,
        }
    }
}

/// A fluent query against one type's table, built by `Session::query`.
/// `.filter`/`.order_by`/`.limit`/`.offset`/`.active_only` mirror
/// `Select`'s own builder methods; `.all()`/`.first()` run it (autoflushing
/// first) and decode through the session's identity map, same as
/// `load_all`.
pub struct SessionQuery<'a, T> {
    session: &'a mut Session,
    select: Select,
    marker: PhantomData<T>,
}

impl<'a, T> SessionQuery<'a, T>
where
    T: Identifiable + FromRow + 'static,
{
    pub fn filter(mut self, expr: Expr) -> Self {
        self.select = self.select.filter(expr);
        self
    }

    pub fn order_by(mut self, ordering: (Column, bool)) -> Self {
        self.select = self.select.order_by(ordering);
        self
    }

    pub fn limit(mut self, limit: i64) -> Self {
        self.select = self.select.limit(limit);
        self
    }

    pub fn offset(mut self, offset: i64) -> Self {
        self.select = self.select.offset(offset);
        self
    }

    /// Excludes soft-deleted rows, for a `#[table(soft_delete)]` type (see
    /// `Mapped::not_deleted_filter`) — a no-op for a type with no
    /// soft-delete column. Mirrors `Session::load_active` for the fluent
    /// query API.
    pub fn active_only(mut self) -> Self {
        if let Some(not_deleted) = T::not_deleted_filter() {
            self.select = self.select.filter(not_deleted);
        }
        self
    }

    /// Runs the query (autoflushing first) and returns every matching row,
    /// identity-mapped the same way `Session::load_all` does.
    pub async fn all(self) -> Result<Vec<Rc<RefCell<T>>>> {
        self.session.load_all(&self.select).await
    }

    /// Like `all`, but only the first matching row (adds `LIMIT 1`).
    pub async fn first(mut self) -> Result<Option<Rc<RefCell<T>>>> {
        self.select = self.select.limit(1);
        Ok(self
            .session
            .load_all(&self.select)
            .await?
            .into_iter()
            .next())
    }
}

fn downcast_cached<T: 'static>(cached: &Rc<dyn Any>) -> Rc<RefCell<T>> {
    cached
        .clone()
        .downcast::<RefCell<T>>()
        .expect("identity map key collision: TypeId matched but the cached value didn't")
}
