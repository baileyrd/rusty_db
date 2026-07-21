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
