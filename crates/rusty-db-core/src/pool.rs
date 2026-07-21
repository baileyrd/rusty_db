use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// Connection pool tuning knobs, passed to a driver's `connect_with`/
/// `engine_with` constructor instead of the plain `connect`/`engine` (which
/// use the underlying driver's defaults).
#[derive(Debug, Clone, Copy)]
pub struct PoolConfig {
    /// Maximum number of connections the pool will open at once.
    pub max_connections: u32,
    /// How long `Engine::connect()` (or anything that checks out a
    /// connection) waits for a free one before giving up, once the pool is
    /// at `max_connections`. `None` uses the underlying driver's default.
    pub acquire_timeout: Option<Duration>,
}

impl PoolConfig {
    /// A pool that opens at most `max_connections` connections.
    pub fn new(max_connections: u32) -> Self {
        PoolConfig {
            max_connections,
            acquire_timeout: None,
        }
    }

    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = Some(timeout);
        self
    }
}

/// A snapshot of a connection pool's state, from `Engine::pool_stats()` —
/// for monitoring saturation directly instead of inferring it from
/// `acquire_timeout` errors after the fact.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolStats {
    /// The pool's configured connection ceiling (`PoolConfig::max_connections`,
    /// or the underlying driver's default if none was given).
    pub max_connections: u32,
    /// Connections currently open, whether idle or checked out.
    pub active: u32,
    /// Of `active`, how many are sitting idle right now.
    pub idle: u32,
    /// Of `active`, how many are currently checked out and in use
    /// (`active - idle`).
    pub in_use: u32,
    /// Callers blocked right now waiting for a connection to free up
    /// (i.e. `in_use` was already at `max_connections` when they asked).
    pub waiters: u32,
    /// Total connections successfully acquired over this pool's lifetime
    /// (every `Engine::connect()`/query/session that got as far as a live
    /// connection), monotonically increasing.
    pub total_acquires: u64,
}

/// The bookkeeping behind `PoolStats::waiters`/`total_acquires` — the two
/// fields sqlx's own pool doesn't expose, so each driver keeps one of
/// these (behind an `Arc`, alongside its `sqlx::Pool`) and threads it
/// through `Driver::connect`. Every other `PoolStats` field is a
/// zero-cost read of the pool itself.
#[derive(Debug, Default)]
pub struct PoolMetrics {
    total_acquires: AtomicU64,
    waiters: AtomicU32,
}

impl PoolMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call before awaiting the underlying pool's own `acquire()`. Returns
    /// a guard that decrements the waiter count again when dropped —
    /// including on an early return from a failed or timed-out acquire —
    /// so a caller who never gets a connection doesn't linger as a phantom
    /// waiter.
    pub fn track_acquire(&self) -> AcquireGuard<'_> {
        self.waiters.fetch_add(1, Ordering::Relaxed);
        AcquireGuard { metrics: self }
    }

    pub fn waiters(&self) -> u32 {
        self.waiters.load(Ordering::Relaxed)
    }

    pub fn total_acquires(&self) -> u64 {
        self.total_acquires.load(Ordering::Relaxed)
    }
}

/// Returned by `PoolMetrics::track_acquire`; call `succeeded()` once the
/// acquire actually lands a connection, or just let it drop on failure.
pub struct AcquireGuard<'a> {
    metrics: &'a PoolMetrics,
}

impl AcquireGuard<'_> {
    /// Record a completed acquire and stop waiting.
    pub fn succeeded(self) {
        self.metrics.total_acquires.fetch_add(1, Ordering::Relaxed);
    }
}

impl Drop for AcquireGuard<'_> {
    fn drop(&mut self) {
        self.metrics.waiters.fetch_sub(1, Ordering::Relaxed);
    }
}
