/// Connection metrics
#[derive(Debug, Clone)]
pub struct ConnectionMetrics {
    pub active_connections: usize,
    pub total_connections_ever: u64,
    pub total_messages: u64,
    pub active_users: usize,
    pub active_rooms: usize,
}

/// Disconnect signal reliability metrics
#[derive(Debug, Clone)]
pub struct DisconnectSignalMetrics {
    /// Number of disconnect signals currently pending retry
    pub pending_count: usize,
    /// Total number of disconnect signals dropped due to queue overflow or timeout
    pub dropped_count: u64,
    /// Total number of disconnect signals successfully retried
    pub retried_count: u64,
}

/// Outcome of awaiting a single background task during shutdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShutdownTaskOutcome {
    Completed,
    Cancelled,
    TimedOut,
    Failed(String),
}

/// Aggregated outcomes for all `ConnectionManager` background tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShutdownReport {
    pub ttl_refresh: Option<ShutdownTaskOutcome>,
    pub pending_retries: Option<ShutdownTaskOutcome>,
    pub disconnect_retry: Option<ShutdownTaskOutcome>,
}

impl ShutdownReport {
    pub(crate) const fn new() -> Self {
        Self {
            ttl_refresh: None,
            pending_retries: None,
            disconnect_retry: None,
        }
    }

    pub(crate) const fn all_clean(&self) -> bool {
        matches!(
            (
                self.ttl_refresh.as_ref(),
                self.pending_retries.as_ref(),
                self.disconnect_retry.as_ref(),
            ),
            (
                None | Some(ShutdownTaskOutcome::Completed | ShutdownTaskOutcome::Cancelled),
                None | Some(ShutdownTaskOutcome::Completed | ShutdownTaskOutcome::Cancelled),
                None | Some(ShutdownTaskOutcome::Completed | ShutdownTaskOutcome::Cancelled)
            )
        )
    }
}
