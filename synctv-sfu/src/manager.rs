//! SFU Manager - Top-level orchestration for multi-room SFU management
//!
//! This module provides:
//! - Multi-room management with concurrent access
//! - Resource limit enforcement
//! - Automatic cleanup of empty rooms
//! - Global statistics collection
//! - Background maintenance tasks

use crate::config::SfuConfig;
use crate::room::{RoomMode, RoomStats, SfuRoom};
use crate::track::MediaTrack;
use crate::types::{PeerId, RoomId, TrackId};
use anyhow::{anyhow, Result};
use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tokio::task::JoinHandle;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

/// Global SFU manager statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ManagerStats {
    /// Number of active rooms
    pub active_rooms: usize,
    /// Number of rooms in SFU mode
    pub sfu_mode_rooms: usize,
    /// Number of rooms in P2P mode
    pub p2p_mode_rooms: usize,
    /// Total number of peers across all rooms
    pub total_peers: usize,
    /// Total number of audio tracks
    pub total_audio_tracks: usize,
    /// Total number of video tracks
    pub total_video_tracks: usize,
    /// Total bytes relayed through SFU
    pub total_bytes_relayed: u64,
    /// Total packets relayed through SFU
    pub total_packets_relayed: u64,
}

/// SFU Manager - manages multiple rooms and provides top-level orchestration
pub struct SfuManager {
    /// Configuration
    config: Arc<SfuConfig>,

    /// Active rooms (uses `DashMap` for lock-free concurrent access)
    rooms: DashMap<RoomId, Arc<SfuRoom>>,

    /// Global statistics
    stats: Arc<RwLock<ManagerStats>>,

    /// Cancellation token for background tasks
    cancel_token: CancellationToken,

    /// Atomic room counter for TOCTOU-safe limit enforcement
    room_count_atomic: AtomicUsize,

    /// Background task handles for cleanup and stats collection
    background_tasks: parking_lot::Mutex<Vec<JoinHandle<()>>>,
}

impl SfuManager {
    /// Create a new SFU manager
    pub fn new(config: SfuConfig) -> Arc<Self> {
        let cancel_token = CancellationToken::new();

        let manager = Arc::new(Self {
            config: Arc::new(config),
            rooms: DashMap::new(),
            stats: Arc::new(RwLock::new(ManagerStats::default())),
            cancel_token,
            room_count_atomic: AtomicUsize::new(0),
            background_tasks: parking_lot::Mutex::new(Vec::new()),
        });

        info!(
            sfu_threshold = manager.config.sfu_threshold,
            max_sfu_rooms = manager.config.max_sfu_rooms,
            max_peers_per_room = manager.config.max_peers_per_room,
            "SFU Manager initialized"
        );

        // Start background tasks with Weak references to avoid Arc cycle.
        // Tasks will exit gracefully when the manager is dropped.
        let cleanup_handle = {
            let weak = Arc::downgrade(&manager);
            let cancel = manager.cancel_token.clone();
            tokio::spawn(async move {
                Self::cleanup_task_weak(weak, cancel).await;
            })
        };

        let stats_handle = {
            let weak = Arc::downgrade(&manager);
            let cancel = manager.cancel_token.clone();
            tokio::spawn(async move {
                Self::stats_collection_task_weak(weak, cancel).await;
            })
        };

        manager.background_tasks.lock().extend([cleanup_handle, stats_handle]);

        manager
    }

    /// Get or create a room.
    ///
    /// Uses an `AtomicUsize` counter for TOCTOU-safe room limit enforcement:
    /// `fetch_add` first to reserve a slot, then insert; `fetch_sub` on failure.
    #[allow(clippy::unused_async)]
    pub async fn get_or_create_room(&self, room_id: RoomId) -> Result<Arc<SfuRoom>> {
        // Fast path: room already exists
        if let Some(room) = self.rooms.get(&room_id) {
            return Ok(Arc::clone(room.value()));
        }

        // Reserve a slot atomically before touching DashMap
        if self.config.max_sfu_rooms > 0 {
            let prev = self.room_count_atomic.fetch_add(1, Ordering::SeqCst);
            if prev >= self.config.max_sfu_rooms {
                self.room_count_atomic.fetch_sub(1, Ordering::SeqCst);
                warn!(
                    max_rooms = self.config.max_sfu_rooms,
                    "Room limit reached"
                );
                return Err(anyhow!("Maximum number of SFU rooms reached"));
            }
        }

        match self.rooms.entry(room_id.clone()) {
            Entry::Occupied(entry) => {
                // Room was created by concurrent task; release our reserved slot
                if self.config.max_sfu_rooms > 0 {
                    self.room_count_atomic.fetch_sub(1, Ordering::SeqCst);
                }
                debug!(room_id = %room_id, "Room already exists");
                Ok(Arc::clone(entry.get()))
            }
            Entry::Vacant(entry) => {
                let room = Arc::new(SfuRoom::new(room_id.clone(), Arc::clone(&self.config)));
                entry.insert(Arc::clone(&room));

                info!(
                    room_id = %room_id,
                    "Created new room"
                );

                Ok(room)
            }
        }
    }

    /// Add a peer to a room
    pub async fn add_peer_to_room(&self, room_id: RoomId, peer_id: PeerId) -> Result<()> {
        let room = self.get_or_create_room(room_id.clone()).await?;

        // Peer limit is enforced atomically inside add_peer to prevent TOCTOU races
        room.add_peer(peer_id.clone(), self.config.max_peers_per_room)
            .await?;

        info!(
            room_id = %room_id,
            peer_id = %peer_id,
            peer_count = room.peer_count().await,
            "Added peer to room"
        );

        Ok(())
    }

    /// Remove a peer from a room
    pub async fn remove_peer_from_room(&self, room_id: &RoomId, peer_id: &PeerId) -> Result<()> {
        if let Some(room_entry) = self.rooms.get(room_id) {
            let room = room_entry.value();
            room.remove_peer(peer_id).await?;

            info!(
                room_id = %room_id,
                peer_id = %peer_id,
                "Removed peer from room"
            );

            // If room is empty, it will be cleaned up by the cleanup task
        } else {
            debug!(room_id = %room_id, "Room not found when removing peer");
        }

        Ok(())
    }

    /// Publish a track in a room
    pub async fn publish_track(
        &self,
        room_id: &RoomId,
        peer_id: &PeerId,
        track_id: TrackId,
        track: Arc<MediaTrack>,
    ) -> Result<()> {
        if let Some(room_entry) = self.rooms.get(room_id) {
            let room = room_entry.value();
            room.add_published_track(peer_id, track_id, track).await?;
            Ok(())
        } else {
            Err(anyhow!("Room not found"))
        }
    }

    /// Unpublish a track from a room
    pub async fn unpublish_track(
        &self,
        room_id: &RoomId,
        peer_id: &PeerId,
        track_id: &TrackId,
    ) -> Result<()> {
        if let Some(room_entry) = self.rooms.get(room_id) {
            let room = room_entry.value();
            room.remove_published_track(peer_id, track_id).await?;
            Ok(())
        } else {
            Err(anyhow!("Room not found"))
        }
    }

    /// Subscribe to a track
    pub async fn subscribe_track(
        &self,
        room_id: &RoomId,
        subscriber_peer_id: &PeerId,
        track_id: &TrackId,
    ) -> Result<()> {
        if let Some(room_entry) = self.rooms.get(room_id) {
            let room = room_entry.value();
            room.subscribe_track(subscriber_peer_id, track_id).await?;
            Ok(())
        } else {
            Err(anyhow!("Room not found"))
        }
    }

    /// Unsubscribe from a track
    pub async fn unsubscribe_track(
        &self,
        room_id: &RoomId,
        subscriber_peer_id: &PeerId,
        track_id: &TrackId,
    ) -> Result<()> {
        if let Some(room_entry) = self.rooms.get(room_id) {
            let room = room_entry.value();
            room.unsubscribe_track(subscriber_peer_id, track_id).await?;
            Ok(())
        } else {
            Err(anyhow!("Room not found"))
        }
    }

    /// Get room statistics
    pub async fn get_room_stats(&self, room_id: &RoomId) -> Result<RoomStats> {
        if let Some(room_entry) = self.rooms.get(room_id) {
            let room = room_entry.value();
            Ok(room.get_stats().await)
        } else {
            Ok(RoomStats::default())
        }
    }

    /// Get global manager statistics
    pub async fn get_stats(&self) -> ManagerStats {
        self.stats.read().await.clone()
    }

    /// Get configuration
    #[must_use] 
    pub fn config(&self) -> &SfuConfig {
        &self.config
    }

    /// Get list of all active room IDs
    #[must_use] 
    pub fn get_room_ids(&self) -> Vec<RoomId> {
        self.rooms.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Get number of active rooms
    #[must_use] 
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Cleanup empty rooms
    ///
    /// Uses `DashMap::remove_if` for atomic check-and-remove to avoid a TOCTOU
    /// race where a peer joins between the emptiness check and the removal.
    ///
    /// Uses `SfuRoom::is_empty_sync()` (atomic counter) instead of directly
    /// accessing the `peers` DashMap, so the emptiness check goes through the
    /// room's public API and stays consistent with `add_peer`/`remove_peer`.
    pub async fn cleanup_empty_rooms(&self) {
        let mut removed_count = 0;

        // Collect candidate room IDs first (can't call remove_if while iterating)
        let candidate_ids: Vec<RoomId> = self
            .rooms
            .iter()
            .filter(|entry| entry.value().is_empty_sync())
            .map(|entry| entry.key().clone())
            .collect();

        // Atomically remove only if still empty
        for room_id in candidate_ids {
            if self
                .rooms
                .remove_if(&room_id, |_, room| room.is_empty_sync())
                .is_some()
            {
                if self.config.max_sfu_rooms > 0 {
                    self.room_count_atomic.fetch_sub(1, Ordering::SeqCst);
                }
                removed_count += 1;
                debug!(room_id = %room_id, "Removed empty room");
            }
        }

        if removed_count > 0 {
            info!(
                removed_count,
                remaining_rooms = self.rooms.len(),
                "Cleaned up empty rooms"
            );
        }
    }

    /// Background task for periodic cleanup using Weak reference.
    /// Exits when the manager is dropped or the cancel token is triggered.
    async fn cleanup_task_weak(weak: std::sync::Weak<Self>, cancel: CancellationToken) {
        let mut ticker = interval(Duration::from_mins(1));
        info!("Starting cleanup task (interval: 60s)");

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("Cleanup task cancelled");
                    break;
                }
                _ = ticker.tick() => {
                    if let Some(manager) = weak.upgrade() {
                        manager.cleanup_empty_rooms().await;
                    } else {
                        info!("Cleanup task exiting: manager dropped");
                        break;
                    }
                }
            }
        }
    }

    /// Background task for statistics collection using Weak reference.
    /// Exits when the manager is dropped or the cancel token is triggered.
    async fn stats_collection_task_weak(weak: std::sync::Weak<Self>, cancel: CancellationToken) {
        let mut ticker = interval(Duration::from_secs(5));
        info!("Starting statistics collection task (interval: 5s)");

        loop {
            tokio::select! {
                () = cancel.cancelled() => {
                    info!("Stats collection task cancelled");
                    break;
                }
                _ = ticker.tick() => {
                    if let Some(manager) = weak.upgrade() {
                        if let Err(e) = manager.update_global_stats().await {
                            error!(error = %e, "Failed to update global statistics");
                        }
                    } else {
                        info!("Stats collection task exiting: manager dropped");
                        break;
                    }
                }
            }
        }
    }

    /// Update global statistics by aggregating room stats
    async fn update_global_stats(&self) -> Result<()> {
        let mut stats = ManagerStats {
            active_rooms: self.rooms.len(),
            ..Default::default()
        };

        for entry in &self.rooms {
            let room = entry.value();
            let room_stats = room.get_stats().await;

            // Count peers
            stats.total_peers += room_stats.peer_count;

            // Count tracks
            stats.total_audio_tracks += room_stats.audio_tracks;
            stats.total_video_tracks += room_stats.video_tracks;

            // Aggregate bytes and packets
            stats.total_bytes_relayed += room_stats.bytes_relayed;
            stats.total_packets_relayed += room_stats.packets_relayed;

            // Count room modes
            let mode = *room.mode.read().await;
            match mode {
                RoomMode::SFU | RoomMode::Migrating => stats.sfu_mode_rooms += 1,
                RoomMode::P2P => stats.p2p_mode_rooms += 1,
            }
        }

        // Log before updating
        debug!(
            active_rooms = stats.active_rooms,
            total_peers = stats.total_peers,
            sfu_rooms = stats.sfu_mode_rooms,
            p2p_rooms = stats.p2p_mode_rooms,
            "Updated global statistics"
        );

        // Update shared stats
        *self.stats.write().await = stats;

        Ok(())
    }

    /// Gracefully shut down the SFU manager, closing all rooms and peers.
    pub async fn shutdown(&self) {
        info!("SFU Manager shutting down, closing {} rooms", self.rooms.len());

        // Cancel background tasks (cleanup + stats collection)
        self.cancel_token.cancel();

        // Drain and await background task handles so they finish before we return
        let handles: Vec<JoinHandle<()>> = self.background_tasks.lock().drain(..).collect();
        for handle in handles {
            let _ = handle.await;
        }

        // Collect room IDs and clear the rooms map
        let room_ids: Vec<RoomId> = self.rooms.iter().map(|entry| entry.key().clone()).collect();
        for room_id in &room_ids {
            if self.rooms.remove(room_id).is_some() && self.config.max_sfu_rooms > 0 {
                self.room_count_atomic.fetch_sub(1, Ordering::SeqCst);
            }
            debug!(room_id = %room_id, "Closed room during shutdown");
        }

        info!("SFU Manager shutdown complete");
    }

    /// Force a specific room into SFU or P2P mode (for testing/debugging)
    pub async fn set_room_mode(&self, room_id: &RoomId, mode: RoomMode) -> Result<()> {
        if let Some(room_entry) = self.rooms.get(room_id) {
            let room = room_entry.value();
            let mut current_mode = room.mode.write().await;

            if *current_mode == mode {
                drop(current_mode);
            } else {
                let old_mode = *current_mode;
                info!(
                    room_id = %room_id,
                    old_mode = ?old_mode,
                    new_mode = ?mode,
                    "Forcing room mode change"
                );
                *current_mode = mode;
                drop(current_mode);

                // Start or stop track forwarding to match the new mode
                match mode {
                    RoomMode::SFU | RoomMode::Migrating => room.switch_to_sfu().await?,
                    RoomMode::P2P => room.switch_to_p2p().await?,
                }
            }

            Ok(())
        } else {
            Err(anyhow!("Room not found"))
        }
    }

    /// Get network quality stats for all peers in a room
    pub fn get_room_network_quality(
        &self,
        room_id: &RoomId,
    ) -> Result<Vec<(String, crate::network_monitor::NetworkStats)>> {
        self.rooms.get(room_id).map_or_else(
            || Ok(Vec::new()),
            |room_entry| Ok(room_entry.value().get_network_quality_stats()),
        )
    }
}

impl Drop for SfuManager {
    fn drop(&mut self) {
        // Cancel all background tasks (cleanup + stats collection)
        self.cancel_token.cancel();

        // Abort background task handles to prevent leaked spawned tasks
        for handle in self.background_tasks.lock().drain(..) {
            handle.abort();
        }

        // Clear rooms (SfuRoom's Drop handles forwarding task cleanup)
        let room_count = self.rooms.len();
        self.rooms.clear();

        if room_count > 0 {
            info!(
                room_count,
                "SfuManager dropped, cleaned up all rooms"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_manager_creation() {
        let config = SfuConfig::default();
        let manager = SfuManager::new(config);
        assert_eq!(manager.room_count(), 0);
    }

    #[tokio::test]
    async fn test_room_lifecycle() {
        let config = SfuConfig::default();
        let manager = SfuManager::new(config);

        let room_id = RoomId::from("test-room");
        let room = manager.get_or_create_room(room_id.clone()).await.unwrap();
        assert_eq!(manager.room_count(), 1);

        // Getting the same room should return the existing one
        let room2 = manager.get_or_create_room(room_id.clone()).await.unwrap();
        assert_eq!(manager.room_count(), 1);
        assert!(Arc::ptr_eq(&room, &room2));
    }

    #[tokio::test]
    async fn test_peer_limit() {
        let config = SfuConfig { max_peers_per_room: 2, ..SfuConfig::default() };
        let manager = SfuManager::new(config);

        let room_id = RoomId::from("test-room");

        // Add peers up to limit
        manager.add_peer_to_room(room_id.clone(), PeerId::from("peer1")).await.unwrap();
        manager.add_peer_to_room(room_id.clone(), PeerId::from("peer2")).await.unwrap();

        // Adding one more should fail
        let result = manager.add_peer_to_room(room_id.clone(), PeerId::from("peer3")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_room_limit() {
        let config = SfuConfig { max_sfu_rooms: 2, ..SfuConfig::default() };
        let manager = SfuManager::new(config);

        // Create rooms up to limit
        manager.get_or_create_room(RoomId::from("room1")).await.unwrap();
        manager.get_or_create_room(RoomId::from("room2")).await.unwrap();

        // Creating one more should fail
        let result = manager.get_or_create_room(RoomId::from("room3")).await;
        assert!(result.is_err());
    }
}
