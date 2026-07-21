//! Versioned schema migrations, tracked in a bookkeeping table.
//!
//! Migrations are plain SQL statements — the query builder doesn't cover
//! DDL, and DDL syntax diverges more across databases than DML does, so
//! there's no attempt to make a migration portable for you; write SQL your
//! target database(s) understand, same as with any other migration tool.

use std::collections::HashSet;

use crate::dialect::Dialect;
use crate::engine::{Engine, Transaction};
use crate::error::{Error, Result};
use crate::query::{Delete, Insert, Select, Table};

/// One schema change: a monotonically increasing `version`, a
/// human-readable `name`, the SQL statements that apply it (`up`), and the
/// statements that revert it (`down` — an empty slice means irreversible).
///
/// Each statement runs as its own `execute()` call, so if your database's
/// driver doesn't support multiple statements per call (as is common over
/// the extended/prepared query protocol), split a migration into multiple
/// entries in the `up`/`down` slices rather than joining them with `;`.
#[derive(Debug, Clone, Copy)]
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub up: &'static [&'static str],
    pub down: &'static [&'static str],
}

/// One row of the bookkeeping table: a migration that has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedMigration {
    pub version: i64,
    pub name: String,
}

/// Applies/reverts `Migration`s against an `Engine`, tracking which have
/// run in a bookkeeping table (`_rusty_db_migrations` by default). Each
/// migration runs in its own dedicated transaction (see `up`). To instead
/// fold migrations into a larger unit of work — sharing atomicity with
/// other reads/writes, and only taking effect when that unit of work
/// commits — use `Session::migrate` instead of `Migrator`.
pub struct Migrator<'a> {
    engine: &'a Engine,
    table: &'static str,
}

impl<'a> Migrator<'a> {
    pub fn new(engine: &'a Engine) -> Self {
        Migrator {
            engine,
            table: "_rusty_db_migrations",
        }
    }

    /// Use a different bookkeeping table name (e.g. to namespace migrations
    /// when this library is embedded inside a larger application).
    pub fn with_table(mut self, table: &'static str) -> Self {
        self.table = table;
        self
    }

    async fn ensure_table(&self) -> Result<()> {
        let quoted = self.engine.dialect().quote_ident(self.table);
        self.engine
            .connect()
            .await?
            .execute(&create_table_ddl(&quoted), &[])
            .await?;
        Ok(())
    }

    /// Migrations already recorded as applied, ordered by version.
    pub async fn applied(&self) -> Result<Vec<AppliedMigration>> {
        self.ensure_table().await?;
        let table = Table::new(self.table);
        let query = Select::from(&table).order_by(table.col("version").asc());
        self.engine
            .fetch_all(&query)
            .await?
            .iter()
            .map(|row| {
                Ok(AppliedMigration {
                    version: row.get_by_name("version")?,
                    name: row.get_by_name("name")?,
                })
            })
            .collect()
    }

    /// Apply every migration in `migrations` not already recorded as
    /// applied, in ascending version order, each in its own transaction.
    ///
    /// Returns the versions applied by this call. On failure, the failing
    /// migration's transaction rolls back and this returns the error;
    /// migrations before it in this call have already committed, so fixing
    /// the problem and calling `up` again resumes from there.
    pub async fn up(&self, migrations: &[Migration]) -> Result<Vec<i64>> {
        self.ensure_table().await?;

        let mut sorted: Vec<&Migration> = migrations.iter().collect();
        sorted.sort_by_key(|m| m.version);
        check_unique_versions(&sorted)?;

        let already: HashSet<i64> = self
            .applied()
            .await?
            .into_iter()
            .map(|m| m.version)
            .collect();

        let table = Table::new(self.table);
        let mut newly_applied = Vec::new();
        for migration in sorted {
            if already.contains(&migration.version) {
                continue;
            }

            let mut txn = self.engine.begin().await?;
            for statement in migration.up {
                txn.execute(statement, &[]).await?;
            }
            let record = Insert::into_table(&table)
                .value("version", migration.version)
                .value("name", migration.name);
            txn.execute_query(&record, self.engine.dialect()).await?;
            txn.commit().await?;

            newly_applied.push(migration.version);
        }

        Ok(newly_applied)
    }

    /// Revert the most-recently-applied migration among `migrations`,
    /// running its `down` statements and removing its bookkeeping row in
    /// one transaction. Returns the version reverted, or `None` if nothing
    /// in `migrations` is currently applied.
    pub async fn down(&self, migrations: &[Migration]) -> Result<Option<i64>> {
        self.ensure_table().await?;

        let applied = self.applied().await?;
        let Some(last) = applied.last() else {
            return Ok(None);
        };

        let migration = migrations
            .iter()
            .find(|m| m.version == last.version)
            .ok_or_else(|| {
                Error::Migration(format!(
                    "applied migration version {} is not in the provided migration list",
                    last.version
                ))
            })?;

        if migration.down.is_empty() {
            return Err(Error::Migration(format!(
                "migration {} ({:?}) has no down statements; it cannot be reverted",
                migration.version, migration.name
            )));
        }

        let table = Table::new(self.table);
        let mut txn = self.engine.begin().await?;
        for statement in migration.down {
            txn.execute(statement, &[]).await?;
        }
        let unrecord = Delete::from(&table).filter(table.col("version").eq(migration.version));
        txn.execute_query(&unrecord, self.engine.dialect()).await?;
        txn.commit().await?;

        Ok(Some(migration.version))
    }

    /// For each of `migrations`, whether it's currently applied — in the
    /// order `migrations` was given, for diagnostics/tests.
    pub async fn status(&self, migrations: &[Migration]) -> Result<Vec<(Migration, bool)>> {
        let already: HashSet<i64> = self
            .applied()
            .await?
            .into_iter()
            .map(|m| m.version)
            .collect();
        Ok(migrations
            .iter()
            .map(|m| (*m, already.contains(&m.version)))
            .collect())
    }
}

fn check_unique_versions(sorted: &[&Migration]) -> Result<()> {
    for pair in sorted.windows(2) {
        if pair[0].version == pair[1].version {
            return Err(Error::Migration(format!(
                "duplicate migration version {}: {:?} and {:?}",
                pair[0].version, pair[0].name, pair[1].name
            )));
        }
    }
    Ok(())
}

fn create_table_ddl(quoted_table: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {quoted_table} (\
            version INTEGER PRIMARY KEY, \
            name TEXT NOT NULL, \
            applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP\
        )"
    )
}

/// Applies every migration in `migrations` not already recorded as applied
/// (in ascending version order) through `txn`, without committing or
/// rolling it back — that's left to the caller. This is what
/// `Session::migrate` uses to fold migrations into a session's own
/// transaction; `Migrator::up` instead wraps each migration in its own
/// dedicated transaction via `Engine::begin`.
pub(crate) async fn apply_pending(
    txn: &mut Transaction,
    dialect: &dyn Dialect,
    table: &str,
    migrations: &[Migration],
) -> Result<Vec<i64>> {
    let quoted = dialect.quote_ident(table);
    txn.execute(&create_table_ddl(&quoted), &[]).await?;

    let mut sorted: Vec<&Migration> = migrations.iter().collect();
    sorted.sort_by_key(|m| m.version);
    check_unique_versions(&sorted)?;

    let table_ref = Table::new(table);
    let applied_query = Select::from(&table_ref).order_by(table_ref.col("version").asc());
    let already: HashSet<i64> = txn
        .fetch_all(&applied_query, dialect)
        .await?
        .iter()
        .map(|row| row.get_by_name::<i64>("version"))
        .collect::<Result<_>>()?;

    let mut newly_applied = Vec::new();
    for migration in sorted {
        if already.contains(&migration.version) {
            continue;
        }

        for statement in migration.up {
            txn.execute(statement, &[]).await?;
        }
        let record = Insert::into_table(&table_ref)
            .value("version", migration.version)
            .value("name", migration.name);
        txn.execute_query(&record, dialect).await?;

        newly_applied.push(migration.version);
    }

    Ok(newly_applied)
}
