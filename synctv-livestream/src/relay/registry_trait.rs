// StreamRegistry trait for abstraction and testing
// This trait allows mocking StreamRegistry in tests without requiring Redis

use super::registry::{PublisherInfo, StreamRegistry};
use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherRefreshOutcome {
    Refreshed,
    Missing,
    OwnershipChanged,
}

#[derive(Debug, Clone)]
pub struct ActivePublisherEntry {
    pub room_id: String,
    pub media_id: String,
    pub publisher: PublisherInfo,
}

/// `StreamRegistry` trait for publisher registration
#[async_trait]
pub trait StreamRegistryTrait: Send + Sync {
    /// Register a publisher for a media in a room (atomic operation).
    /// Returns `true` if registered successfully, `false` if already exists.
    async fn register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        app_name: &str,
        api_address: &str,
    ) -> Result<bool>;

    /// Try to register as publisher (atomic operation)
    /// Returns true if registered successfully, false if already exists.
    /// `user_id` is stored for reverse-index lookups (pass "" if unknown).
    /// `api_address` is the advertised shared API address of this node for
    /// cross-node proxying.
    async fn try_register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        api_address: &str,
    ) -> Result<bool>;

    /// Refresh TTL for a publisher (called by heartbeat).
    /// `user_id` and `node_id` are used to also refresh reverse-index TTLs
    /// (pass "" to skip either index).
    async fn refresh_publisher_ttl(
        &self,
        room_id: &str,
        media_id: &str,
        user_id: &str,
        node_id: &str,
        expected_epoch: u64,
    ) -> Result<PublisherRefreshOutcome>;

    /// Unregister a publisher unconditionally.
    async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> Result<()>;

    /// Unregister a publisher only if the stored epoch matches `expected_epoch`.
    async fn unregister_publisher_if_epoch_matches(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: u64,
    ) -> Result<()>;

    /// Get publisher info for a media in a room
    async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>>;

    /// Check if a stream is active (has a publisher)
    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool>;

    /// List all active publishers with their current registry snapshot.
    async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>>;

    /// List all active streams (returns tuples of (`room_id`, `media_id`))
    async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
        Ok(self
            .list_active_publishers()
            .await?
            .into_iter()
            .map(|entry| (entry.room_id, entry.media_id))
            .collect())
    }

    /// List active streams for a specific room (returns `media_id` values).
    /// Default implementation filters `list_active_streams`; backends can
    /// override for a more efficient query.
    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        let all = self.list_active_streams().await?;
        Ok(all
            .into_iter()
            .filter(|(rid, _)| rid == room_id)
            .map(|(_, media_id)| media_id)
            .collect())
    }

    /// Get all active publishers for a user (via reverse index)
    /// Returns list of (`room_id`, `media_id`) pairs
    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>>;

    /// Remove all publisher entries for a user
    async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()>;

    /// Validate that the given epoch matches the current publisher's epoch.
    /// Returns Ok(true) if valid, Ok(false) if stale (split-brain detected).
    /// Used by pull streams to detect if publisher has changed.
    async fn validate_epoch(&self, room_id: &str, media_id: &str, epoch: u64) -> Result<bool>;

    /// Clean up all publisher registrations for a specific node.
    /// Used when a node restarts to remove stale entries from Redis.
    async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()>;
}

// Implement StreamRegistryTrait for StreamRegistry
#[async_trait]
impl StreamRegistryTrait for StreamRegistry {
    async fn register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        app_name: &str,
        api_address: &str,
    ) -> Result<bool> {
        Self::register_publisher(self, room_id, media_id, node_id, app_name, api_address).await
    }

    async fn try_register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        api_address: &str,
    ) -> Result<bool> {
        Self::try_register_publisher_with_user(
            self,
            room_id,
            media_id,
            node_id,
            user_id,
            api_address,
        )
        .await
    }

    async fn refresh_publisher_ttl(
        &self,
        room_id: &str,
        media_id: &str,
        user_id: &str,
        node_id: &str,
        expected_epoch: u64,
    ) -> Result<PublisherRefreshOutcome> {
        Self::refresh_publisher_ttl_with_owner(
            self,
            room_id,
            media_id,
            user_id,
            node_id,
            Some(expected_epoch),
        )
        .await
    }

    async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        Self::unregister_publisher_immut(self, room_id, media_id).await
    }

    async fn unregister_publisher_if_epoch_matches(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: u64,
    ) -> Result<()> {
        Self::unregister_publisher_with_epoch(self, room_id, media_id, Some(expected_epoch)).await
    }

    async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>> {
        Self::get_publisher_immut(self, room_id, media_id).await
    }

    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        Self::is_stream_active_immut(self, room_id, media_id).await
    }

    async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>> {
        Self::list_active_publishers_immut(self).await
    }

    async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
        Self::list_active_streams_immut(self).await
    }

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        Self::list_streams_for_room(self, room_id).await
    }

    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        Self::get_user_publishers(self, user_id).await
    }

    async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()> {
        Self::unregister_all_user_publishers(self, user_id).await
    }

    async fn validate_epoch(&self, room_id: &str, media_id: &str, epoch: u64) -> Result<bool> {
        Self::validate_epoch(self, room_id, media_id, epoch).await
    }

    async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()> {
        Self::cleanup_all_publishers_for_node(self, node_id).await
    }
}
