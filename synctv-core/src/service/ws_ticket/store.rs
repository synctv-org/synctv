use async_trait::async_trait;

use crate::models::RoomId;
use crate::Result;

use super::WsTicketData;

/// Backend storage for WebSocket tickets.
///
/// Implementations must provide atomic insert and get-and-delete operations
/// to ensure one-time-use semantics.
#[async_trait]
pub trait TicketStore: Send + Sync {
    /// Store a ticket with its associated data. The ticket must expire after `ttl_secs`.
    async fn store(&self, ticket: &str, data: &WsTicketData, ttl_secs: u64) -> Result<()>;

    /// Load a ticket scoped to the expected room without consuming it.
    ///
    /// Returns `None` if the ticket does not exist or has expired.
    async fn load(&self, ticket: &str, expected_room_id: &RoomId) -> Result<Option<WsTicketData>>;

    /// Try to claim a ticket after validation succeeds.
    ///
    /// The claim must only succeed if the stored ticket still matches the exact
    /// ticket data that was previously loaded and validated by the caller.
    /// This closes the `load -> validate -> consume` TOCTOU window by turning
    /// the final delete step into a compare-and-delete.
    ///
    /// Returns `true` if the ticket was successfully consumed by this caller,
    /// `false` if it had already expired, been consumed concurrently, or no
    /// longer matched the validated value.
    async fn claim(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        expected_ticket: &WsTicketData,
    ) -> Result<bool>;

    /// Atomically get and delete a ticket scoped to the expected room.
    /// Returns `None` if the ticket does not exist or has expired.
    async fn consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<Option<WsTicketData>>;

    /// Whether this store can safely validate and consume tickets across nodes.
    ///
    /// Clustered WebSocket authentication requires a shared backend because the
    /// node that validates a ticket may differ from the node that issued it.
    fn supports_cluster_runtime(&self) -> bool;
}
