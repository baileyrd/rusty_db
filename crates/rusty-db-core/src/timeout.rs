//! A client-side timeout for any async database operation.

use std::future::Future;
use std::time::Duration;

use crate::error::{Error, Result};

/// Runs `operation` and cancels it if `duration` elapses before it
/// finishes, returning `Error::Timeout` instead of waiting any longer.
///
/// "Cancels" here means exactly what it means for any Rust future:
/// `operation` is dropped without being polled again. Whatever connection
/// it was using is returned to (or, if it was mid-query, discarded from)
/// the pool by the driver's own `Drop` handling — the same thing that
/// would happen if the caller had dropped the future manually — so a
/// cancelled operation never leaves the pool stuck; the next call just
/// gets a fresh connection if the old one couldn't be reused. Note that
/// the database server itself may not learn the query was abandoned
/// until the connection is actually closed — this only stops the client
/// from waiting on it.
///
/// Works with any `Engine`/`Transaction`/`Session`/`ReplicaSet` call,
/// since they all return `Result<T>`:
///
/// ```no_run
/// # use std::time::Duration;
/// # use rusty_db_core::{with_timeout, Engine, Select, Table};
/// # async fn example(engine: &Engine, users: &Table) -> rusty_db_core::Result<()> {
/// let rows = with_timeout(Duration::from_secs(5), engine.fetch_all(&Select::from(users))).await?;
/// # let _ = rows;
/// # Ok(())
/// # }
/// ```
pub async fn with_timeout<T>(
    duration: Duration,
    operation: impl Future<Output = Result<T>>,
) -> Result<T> {
    match tokio::time::timeout(duration, operation).await {
        Ok(result) => result,
        Err(_) => Err(Error::Timeout(duration)),
    }
}
