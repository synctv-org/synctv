// StreamRegistry trait for abstraction and testing
// This trait allows mocking StreamRegistry in tests without requiring Redis

use super::registry::{StreamGeneration, StreamRegistry, WebRtcSessionOwner};
use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeaseRefreshOutcome {
    Refreshed,
    Missing,
    OwnershipChanged,
}

pub(crate) const PUBLISHER_REFRESH_BATCH_SIZE: usize = 128;

#[derive(Debug, Clone)]
pub struct LeaseRefreshRequest {
    pub room_id: String,
    pub media_id: String,
    pub generation_id: String,
    pub user_id: String,
    pub expected_lease_epoch: u64,
}

#[derive(Debug, Clone)]
pub struct ActiveStreamGeneration {
    pub room_id: String,
    pub media_id: String,
    pub generation: StreamGeneration,
}

#[derive(Debug, Clone, Copy)]
pub struct StreamGenerationRegistration<'a> {
    pub room_id: &'a str,
    pub media_id: &'a str,
    pub node_id: &'a str,
    pub user_id: &'a str,
    pub cluster_address: &'a str,
    pub generation_id: &'a str,
    pub supports_rtp: bool,
}

impl<'a> StreamGenerationRegistration<'a> {
    #[must_use]
    pub const fn new(
        room_id: &'a str,
        media_id: &'a str,
        node_id: &'a str,
        user_id: &'a str,
        cluster_address: &'a str,
        generation_id: &'a str,
    ) -> Self {
        Self {
            room_id,
            media_id,
            node_id,
            user_id,
            cluster_address,
            generation_id,
            supports_rtp: false,
        }
    }

    #[must_use]
    pub const fn with_rtp_support(mut self, supports_rtp: bool) -> Self {
        self.supports_rtp = supports_rtp;
        self
    }
}

/// `StreamRegistry` trait for publisher registration
#[async_trait]
pub trait StreamRegistryTrait: Send + Sync {
    /// Try to register as publisher (atomic operation)
    /// Returns true if registered successfully, false if already exists.
    /// `user_id` is stored for reverse-index lookups (pass "" if unknown).
    /// `cluster_address` is the advertised cluster listener address of this node
    /// for cross-node proxying.
    async fn try_activate_generation(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        cluster_address: &str,
        generation_id: &str,
    ) -> Result<bool>;

    async fn try_activate_generation_with_capabilities(
        &self,
        registration: StreamGenerationRegistration<'_>,
    ) -> Result<bool> {
        let StreamGenerationRegistration {
            room_id,
            media_id,
            node_id,
            user_id,
            cluster_address,
            generation_id,
            supports_rtp,
        } = registration;
        let registered = self
            .try_activate_generation(
                room_id,
                media_id,
                node_id,
                user_id,
                cluster_address,
                generation_id,
            )
            .await?;
        if !registered || !supports_rtp {
            return Ok(registered);
        }
        let generation = self
            .get_active_generation(room_id, media_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("registered generation disappeared"))?;
        anyhow::ensure!(
            self.set_generation_supports_rtp(
                room_id,
                media_id,
                generation_id,
                generation.lease_epoch,
                true,
            )
            .await?,
            "registered generation ownership changed before RTP capability update"
        );
        Ok(true)
    }

    async fn set_generation_supports_rtp(
        &self,
        _room_id: &str,
        _media_id: &str,
        _generation_id: &str,
        _expected_lease_epoch: u64,
        _supports_rtp: bool,
    ) -> Result<bool> {
        Ok(false)
    }

    async fn try_register_webrtc_session(
        &self,
        _session_id: &str,
        _owner: &WebRtcSessionOwner,
        _ttl: std::time::Duration,
    ) -> Result<bool> {
        Err(anyhow::anyhow!(
            "WebRTC session ownership is not supported by this registry"
        ))
    }

    async fn get_webrtc_session_owner(
        &self,
        _session_id: &str,
    ) -> Result<Option<WebRtcSessionOwner>> {
        Err(anyhow::anyhow!(
            "WebRTC session ownership is not supported by this registry"
        ))
    }

    async fn unregister_webrtc_session(
        &self,
        _session_id: &str,
        _expected_node_id: &str,
    ) -> Result<bool> {
        Err(anyhow::anyhow!(
            "WebRTC session ownership is not supported by this registry"
        ))
    }

    /// Commit a generation as playable after StreamHub admission.
    async fn mark_generation_ready(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool>;

    /// Refresh TTL for a publisher (called by heartbeat).
    /// `user_id` and `node_id` are used to also refresh reverse-index TTLs
    /// (pass "" to skip either index).
    async fn refresh_generation_lease(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        user_id: &str,
        node_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<LeaseRefreshOutcome>;

    /// Refresh a bounded batch of publisher leases, preserving input order.
    /// Redis-backed registries override this with a single pipelined request.
    async fn refresh_generation_leases(
        &self,
        node_id: &str,
        requests: &[LeaseRefreshRequest],
    ) -> Result<Vec<LeaseRefreshOutcome>> {
        anyhow::ensure!(
            requests.len() <= PUBLISHER_REFRESH_BATCH_SIZE,
            "publisher refresh batch contains {} entries; maximum is {}",
            requests.len(),
            PUBLISHER_REFRESH_BATCH_SIZE
        );

        let mut outcomes = Vec::with_capacity(requests.len());
        for request in requests {
            outcomes.push(
                self.refresh_generation_lease(
                    &request.room_id,
                    &request.media_id,
                    &request.generation_id,
                    &request.user_id,
                    node_id,
                    request.expected_lease_epoch,
                )
                .await?,
            );
        }
        Ok(outcomes)
    }

    /// Unregister a publisher unconditionally.
    async fn deactivate_current_generation(&self, room_id: &str, media_id: &str) -> Result<()>;

    /// Unregister a publisher only if the generation and lease still match.
    ///
    /// Returns `true` only when the active generation was actually deactivated.
    async fn deactivate_generation_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool>;

    /// Release the active publisher while retaining its route for the final HLS generation.
    async fn deactivate_generation_preserving_hls_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool> {
        self.deactivate_generation_if_lease_matches(
            room_id,
            media_id,
            generation_id,
            expected_lease_epoch,
        )
        .await
    }

    /// Get publisher info for a media in a room
    async fn get_active_generation(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<StreamGeneration>>;

    /// Get one exact active or retained stream generation.
    async fn get_generation(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
    ) -> Result<Option<StreamGeneration>>;

    /// Check if a stream is active (has a publisher)
    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool>;

    /// List all active publishers with their current registry snapshot.
    async fn list_active_generations(&self) -> Result<Vec<ActiveStreamGeneration>>;

    /// Release all active publishers owned by a node while retaining each
    /// generation long enough for its final HLS playlist to be served.
    async fn deactivate_all_generations_for_node_preserving_hls(
        &self,
        node_id: &str,
    ) -> Result<()> {
        let active_generations = self.list_active_generations().await?;
        for active in active_generations {
            if active.generation.node_id != node_id {
                continue;
            }
            self.deactivate_generation_preserving_hls_if_lease_matches(
                &active.room_id,
                &active.media_id,
                &active.generation.generation_id,
                active.generation.lease_epoch,
            )
            .await?;
        }
        Ok(())
    }

    /// List active streams for a specific room (returns `media_id` values).
    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>>;

    /// Get all active publishers for a user (via reverse index)
    /// Returns list of (`room_id`, `media_id`) pairs
    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>>;

    /// Get active publishers for a user in a specific room (via reverse index).
    async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>>;

    /// Validate that the given lease_epoch matches the current publisher's lease_epoch.
    /// Returns Ok(true) if valid, Ok(false) if stale (split-brain detected).
    /// Used by pull streams to detect if publisher has changed.
    async fn validate_lease(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        lease_epoch: u64,
    ) -> Result<bool> {
        Ok(self
            .get_active_generation(room_id, media_id)
            .await?
            .is_some_and(|generation| {
                generation.generation_id == generation_id && generation.lease_epoch == lease_epoch
            }))
    }

    /// Clean up all publisher registrations for a specific node.
    /// Used when a node restarts to remove stale entries from Redis.
    async fn cleanup_all_generations_for_node(&self, node_id: &str) -> Result<()>;
}

// Implement StreamRegistryTrait for StreamRegistry
#[async_trait]
impl StreamRegistryTrait for StreamRegistry {
    async fn try_activate_generation(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        cluster_address: &str,
        generation_id: &str,
    ) -> Result<bool> {
        Self::try_activate_generation_with_user(
            self,
            room_id,
            media_id,
            node_id,
            user_id,
            cluster_address,
            generation_id,
        )
        .await
    }

    async fn try_activate_generation_with_capabilities(
        &self,
        registration: StreamGenerationRegistration<'_>,
    ) -> Result<bool> {
        StreamRegistry::try_activate_generation_with_capabilities(self, registration).await
    }

    async fn set_generation_supports_rtp(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
        supports_rtp: bool,
    ) -> Result<bool> {
        StreamRegistry::set_generation_supports_rtp(
            self,
            room_id,
            media_id,
            generation_id,
            expected_lease_epoch,
            supports_rtp,
        )
        .await
    }

    async fn try_register_webrtc_session(
        &self,
        session_id: &str,
        owner: &WebRtcSessionOwner,
        ttl: std::time::Duration,
    ) -> Result<bool> {
        StreamRegistry::try_register_webrtc_session(self, session_id, owner, ttl).await
    }

    async fn get_webrtc_session_owner(
        &self,
        session_id: &str,
    ) -> Result<Option<WebRtcSessionOwner>> {
        StreamRegistry::get_webrtc_session_owner(self, session_id).await
    }

    async fn unregister_webrtc_session(
        &self,
        session_id: &str,
        expected_node_id: &str,
    ) -> Result<bool> {
        StreamRegistry::unregister_webrtc_session(self, session_id, expected_node_id).await
    }

    async fn refresh_generation_lease(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        user_id: &str,
        node_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<LeaseRefreshOutcome> {
        Self::refresh_generation_lease_with_owner(
            self,
            room_id,
            media_id,
            generation_id,
            user_id,
            node_id,
            Some(expected_lease_epoch),
        )
        .await
    }

    async fn mark_generation_ready(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool> {
        StreamRegistry::mark_generation_ready(
            self,
            room_id,
            media_id,
            generation_id,
            expected_lease_epoch,
        )
        .await
    }

    async fn refresh_generation_leases(
        &self,
        node_id: &str,
        requests: &[LeaseRefreshRequest],
    ) -> Result<Vec<LeaseRefreshOutcome>> {
        StreamRegistry::refresh_generation_leases(self, node_id, requests).await
    }

    async fn deactivate_current_generation(&self, room_id: &str, media_id: &str) -> Result<()> {
        StreamRegistry::deactivate_current_generation(self, room_id, media_id).await
    }

    async fn deactivate_generation_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool> {
        Self::deactivate_generation_with_lease(
            self,
            room_id,
            media_id,
            generation_id,
            expected_lease_epoch,
            false,
        )
        .await
    }

    async fn deactivate_generation_preserving_hls_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool> {
        Self::deactivate_generation_with_hls_grace(
            self,
            room_id,
            media_id,
            generation_id,
            expected_lease_epoch,
        )
        .await
    }

    async fn get_active_generation(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        StreamRegistry::get_active_generation(self, room_id, media_id).await
    }

    async fn get_generation(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        StreamRegistry::get_generation(self, room_id, media_id, generation_id).await
    }

    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        StreamRegistry::is_stream_active(self, room_id, media_id).await
    }

    async fn list_active_generations(&self) -> Result<Vec<ActiveStreamGeneration>> {
        StreamRegistry::list_active_generations(self).await
    }

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        StreamRegistry::list_streams_for_room(self, room_id).await
    }

    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        StreamRegistry::get_user_publishers(self, user_id).await
    }

    async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>> {
        StreamRegistry::get_user_publishers_for_room(self, room_id, user_id).await
    }

    async fn cleanup_all_generations_for_node(&self, node_id: &str) -> Result<()> {
        Self::cleanup_all_generations_for_node(self, node_id).await
    }
}
