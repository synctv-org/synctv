//! SFU Room management
//!
//! This module handles complete room functionality including:
//! - P2P ↔ SFU mode switching based on peer count
//! - Media track publishing and subscription
//! - RTP packet forwarding between peers
//! - Bandwidth estimation and adaptive quality
//! - Room statistics and monitoring

use crate::config::SfuConfig;
use crate::network_monitor::NetworkQualityMonitor;
use crate::peer::SfuPeer;
use crate::track::MediaTrack;
use crate::types::{PeerId, RoomId, TrackId};
use anyhow::{anyhow, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Room mode - P2P, Migrating, or SFU
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoomMode {
    /// Peer-to-peer mode (< threshold peers)
    P2P,
    /// Transitional state while existing P2P peers are being migrated to SFU.
    /// The room enters this state when the peer threshold is reached.
    /// During migration, both P2P and SFU media paths may be active.
    Migrating,
    /// SFU mode (>= threshold peers, all peers migrated)
    SFU,
}

/// Room statistics
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoomStats {
    /// Current number of peers in room
    pub peer_count: usize,
    /// Total peers that have joined (cumulative)
    pub total_peers_joined: u64,
    /// Number of mode switches
    pub mode_switches: u64,
    /// Number of audio tracks
    pub audio_tracks: usize,
    /// Number of video tracks
    pub video_tracks: usize,
    /// Total bytes relayed
    pub bytes_relayed: u64,
    /// Total packets relayed
    pub packets_relayed: u64,
}

/// SFU Room - manages peers and media routing
pub struct SfuRoom {
    /// Room ID
    pub id: RoomId,

    /// Current room mode
    pub mode: Arc<RwLock<RoomMode>>,

    /// Peers in the room (uses `DashMap` for concurrent access)
    pub peers: DashMap<PeerId, Arc<SfuPeer>>,

    /// Published tracks: `track_id` -> (`publisher_peer_id`, track)
    published_tracks: DashMap<TrackId, (PeerId, Arc<MediaTrack>)>,

    /// Track subscribers: `track_id` -> list of subscriber `peer_id`s
    track_subscribers: Arc<DashMap<TrackId, Vec<PeerId>>>,

    /// Forwarding tasks for each track
    forwarding_tasks: Arc<DashMap<TrackId, tokio::task::JoinHandle<()>>>,

    /// Configuration
    pub config: Arc<SfuConfig>,

    /// Statistics
    pub stats: Arc<RwLock<RoomStats>>,

    /// Atomic counters for hot-path stats (avoids write lock per packet)
    packets_relayed: Arc<AtomicU64>,
    bytes_relayed: Arc<AtomicU64>,

    /// Atomic peer counter for TOCTOU-safe capacity enforcement
    peer_count_atomic: Arc<AtomicUsize>,

    /// Network quality monitoring
    network_monitor: Arc<NetworkQualityMonitor>,
}

impl SfuRoom {
    /// Create a new SFU room
    pub fn new(id: RoomId, config: Arc<SfuConfig>) -> Self {
        info!(room_id = %id, "Creating new room");

        Self {
            id,
            mode: Arc::new(RwLock::new(RoomMode::P2P)),
            peers: DashMap::new(),
            published_tracks: DashMap::new(),
            track_subscribers: Arc::new(DashMap::new()),
            forwarding_tasks: Arc::new(DashMap::new()),
            config,
            stats: Arc::new(RwLock::new(RoomStats::default())),
            packets_relayed: Arc::new(AtomicU64::new(0)),
            bytes_relayed: Arc::new(AtomicU64::new(0)),
            peer_count_atomic: Arc::new(AtomicUsize::new(0)),
            network_monitor: Arc::new(NetworkQualityMonitor::new()),
        }
    }

    /// Add a peer to the room.
    ///
    /// Uses an `AtomicUsize` counter for TOCTOU-safe capacity enforcement:
    /// `fetch_add` first to reserve a slot, then insert; `fetch_sub` on failure.
    pub async fn add_peer(&self, peer_id: PeerId, max_peers: usize) -> Result<Arc<SfuPeer>> {
        use dashmap::mapref::entry::Entry;

        // Reserve a slot atomically before touching DashMap
        if max_peers > 0 {
            let prev = self.peer_count_atomic.fetch_add(1, Ordering::SeqCst);
            if prev >= max_peers {
                self.peer_count_atomic.fetch_sub(1, Ordering::SeqCst);
                return Err(anyhow!("Maximum number of peers reached for this room"));
            }
        }

        let peer = match self.peers.entry(peer_id.clone()) {
            Entry::Occupied(_) => {
                // Peer already exists, release reserved slot
                if max_peers > 0 {
                    self.peer_count_atomic.fetch_sub(1, Ordering::SeqCst);
                }
                return Err(anyhow!("Peer already exists in room"));
            }
            Entry::Vacant(entry) => {
                let p = Arc::new(SfuPeer::new(peer_id.clone()));
                entry.insert(p.clone());
                p
            }
        };

        // Update statistics
        {
            let mut stats = self.stats.write().await;
            stats.peer_count = self.peers.len();
            stats.total_peers_joined += 1;
        }

        info!(
            room_id = %self.id,
            peer_id = %peer_id,
            peer_count = self.peers.len(),
            "Added peer to room"
        );

        // Check if we need to switch modes
        self.check_mode_switch().await?;

        Ok(peer)
    }

    /// Remove a peer from the room
    pub async fn remove_peer(&self, peer_id: &PeerId) -> Result<()> {
        // Remove peer and decrement atomic counter
        if self.peers.remove(peer_id).is_some() {
            self.peer_count_atomic.fetch_sub(1, Ordering::SeqCst);
        }

        // Remove from network quality monitor
        self.network_monitor.remove_peer(peer_id);

        // Remove all tracks published by this peer
        let tracks_to_remove: Vec<TrackId> = self
            .published_tracks
            .iter()
            .filter(|entry| &entry.value().0 == peer_id)
            .map(|entry| entry.key().clone())
            .collect();

        for track_id in tracks_to_remove {
            if let Err(e) = self.remove_published_track(peer_id, &track_id).await {
                warn!(
                    room_id = %self.id,
                    peer_id = %peer_id,
                    track_id = %track_id,
                    error = %e,
                    "Failed to remove published track during peer cleanup, continuing"
                );
            }
        }

        // Remove this peer from all track subscriber lists
        for mut entry in self.track_subscribers.iter_mut() {
            entry.value_mut().retain(|id| id != peer_id);
        }

        // Update statistics
        {
            let mut stats = self.stats.write().await;
            stats.peer_count = self.peers.len();
        }

        info!(
            room_id = %self.id,
            peer_id = %peer_id,
            peer_count = self.peers.len(),
            "Removed peer from room"
        );

        // Check if we need to switch modes
        self.check_mode_switch().await?;

        Ok(())
    }

    /// Add a published track (holds peer reference to prevent TOCTOU)
    pub async fn add_published_track(
        &self,
        peer_id: &PeerId,
        track_id: TrackId,
        track: Arc<MediaTrack>,
    ) -> Result<()> {
        // Verify peer exists by holding a reference during insert
        let _peer_ref = self.peers
            .get(peer_id)
            .ok_or_else(|| anyhow!("Peer not found in room"))?;

        // **Track ID Conflict Detection (#21)**: Check for duplicate track_id before inserting.
        // If a track with this ID already exists (from any peer), reject the publish request.
        // This prevents ambiguity in track routing and subscription logic.
        if let Some(existing) = self.published_tracks.get(&track_id) {
            let (existing_peer_id, _existing_track) = existing.value();
            error!(
                room_id = %self.id,
                peer_id = %peer_id,
                track_id = %track_id,
                existing_peer_id = %existing_peer_id,
                "Track ID conflict: track_id already exists in room"
            );
            return Err(anyhow!(
                "Track ID conflict: track '{}' already published by peer '{}'. \
                 Use unique track IDs or implement composite IDs (peer_id:track_id).",
                track_id.as_str(),
                existing_peer_id.as_str()
            ));
        }

        // Store track (safe: peer reference still held, no conflicts)
        self.published_tracks
            .insert(track_id.clone(), (peer_id.clone(), track.clone()));

        info!(
            room_id = %self.id,
            peer_id = %peer_id,
            track_id = %track_id,
            track_kind = ?track.kind,
            "Track published"
        );

        // In SFU mode, start forwarding this track
        let mode = *self.mode.read().await;
        if mode == RoomMode::SFU {
            self.start_track_forwarding(track_id, track, peer_id.clone())
                .await?;
        }

        Ok(())
    }

    /// Remove a published track
    pub async fn remove_published_track(
        &self,
        peer_id: &PeerId,
        track_id: &TrackId,
    ) -> Result<()> {
        // Stop forwarding task if it exists
        if let Some((_, task)) = self.forwarding_tasks.remove(track_id) {
            task.abort();
            debug!(
                room_id = %self.id,
                track_id = %track_id,
                "Stopped track forwarding task"
            );
        }

        // Remove track
        if let Some((_, (publisher_id, track))) = self.published_tracks.remove(track_id) {
            if &publisher_id != peer_id {
                warn!(
                    room_id = %self.id,
                    track_id = %track_id,
                    expected_publisher = %peer_id,
                    actual_publisher = %publisher_id,
                    "Track publisher mismatch"
                );
            }

            info!(
                room_id = %self.id,
                peer_id = %peer_id,
                track_id = %track_id,
                track_kind = ?track.kind,
                "Track unpublished"
            );
        }

        // Remove all subscriptions to this track
        self.track_subscribers.remove(track_id);

        Ok(())
    }

    /// Subscribe to a track (holds references to prevent TOCTOU)
    pub async fn subscribe_track(
        &self,
        subscriber_peer_id: &PeerId,
        track_id: &TrackId,
    ) -> Result<()> {
        // Verify by holding references during insert
        let _peer_ref = self.peers
            .get(subscriber_peer_id)
            .ok_or_else(|| anyhow!("Subscriber peer not found in room"))?;
        let _track_ref = self.published_tracks
            .get(track_id)
            .ok_or_else(|| anyhow!("Track not found in room"))?;

        // Add subscription (safe: references still held)
        self.track_subscribers
            .entry(track_id.clone())
            .or_default()
            .push(subscriber_peer_id.clone());

        info!(
            room_id = %self.id,
            subscriber = %subscriber_peer_id,
            track_id = %track_id,
            "Subscribed to track"
        );

        Ok(())
    }

    /// Unsubscribe from a track
    pub async fn unsubscribe_track(
        &self,
        subscriber_peer_id: &PeerId,
        track_id: &TrackId,
    ) -> Result<()> {
        if let Some(mut subscribers) = self.track_subscribers.get_mut(track_id) {
            subscribers.retain(|id| id != subscriber_peer_id);
        }

        info!(
            room_id = %self.id,
            subscriber = %subscriber_peer_id,
            track_id = %track_id,
            "Unsubscribed from track"
        );

        Ok(())
    }

    /// Start forwarding a track to subscribers
    async fn start_track_forwarding(
        &self,
        track_id: TrackId,
        track: Arc<MediaTrack>,
        publisher_peer_id: PeerId,
    ) -> Result<()> {
        // Clone necessary data for the background task
        let track_id_clone = track_id.clone();
        let track_id_for_cleanup = track_id.clone();
        let room_id = self.id.clone();
        let peers = self.peers.clone();
        let track_subscribers = Arc::clone(&self.track_subscribers);
        let packets_relayed = Arc::clone(&self.packets_relayed);
        let bytes_relayed = Arc::clone(&self.bytes_relayed);
        let forwarding_tasks = Arc::clone(&self.forwarding_tasks);

        // Spawn forwarding task
        let task = tokio::spawn(async move {
            if let Err(e) = Self::forward_track_packets(
                room_id,
                track_id_clone,
                track,
                peers,
                track_subscribers,
                publisher_peer_id,
                packets_relayed,
                bytes_relayed,
            )
            .await
            {
                error!(error = %e, "Track forwarding task failed");
            }

            // Self-cleanup: remove our own entry from forwarding_tasks when the
            // track closes naturally (channel dropped) or on error. This prevents
            // stale entries from accumulating when tracks end without going through
            // remove_published_track (e.g., the publisher's media track closes).
            forwarding_tasks.remove(&track_id_for_cleanup);
        });

        self.forwarding_tasks.insert(track_id, task);

        Ok(())
    }

    /// Forward track packets to subscribers (background task)
    #[allow(clippy::too_many_arguments)]
    async fn forward_track_packets(
        room_id: RoomId,
        track_id: TrackId,
        track: Arc<MediaTrack>,
        peers: DashMap<PeerId, Arc<SfuPeer>>,
        track_subscribers: Arc<DashMap<TrackId, Vec<PeerId>>>,
        publisher_peer_id: PeerId,
        packets_relayed: Arc<AtomicU64>,
        bytes_relayed: Arc<AtomicU64>,
    ) -> Result<()> {
        // Start reading packets from the track (uses interior mutability)
        let mut packet_rx = track.start_reading().await?;

        debug!(
            room_id = %room_id,
            track_id = %track_id,
            "Started forwarding track packets"
        );

        // Forward packets to subscribers
        while let Some(packet) = packet_rx.recv().await {
            // O(subscribers_of_track) lookup instead of O(total_subscriptions)
            let subscribers: Vec<PeerId> = track_subscribers
                .get(&track_id)
                .map(|subs| {
                    subs.iter()
                        .filter(|id| *id != &publisher_peer_id)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();

            // Forward to each subscriber via their packet channel
            let packet_size = packet.data.len();
            for subscriber_id in &subscribers {
                if let Some(peer) = peers.get(subscriber_id) {
                    // Simulcast quality filtering: skip packets that don't match
                    // the subscriber's preferred quality layer
                    if let Some(packet_quality) = packet.quality_layer {
                        let preferred = peer.get_preferred_quality();
                        if packet_quality != preferred {
                            continue;
                        }
                    }

                    if peer.try_forward_packet(&packet) {
                        peer.record_sent_bytes(packet_size);
                        packets_relayed.fetch_add(1, Ordering::Relaxed);
                        bytes_relayed.fetch_add(packet_size as u64, Ordering::Relaxed);
                    }
                }
            }
        }

        info!(
            room_id = %room_id,
            track_id = %track_id,
            "Stopped forwarding track packets"
        );

        Ok(())
    }

    /// Check if mode switch is needed and perform it.
    ///
    /// Uses hysteresis to prevent rapid P2P/SFU switching: switch to SFU at
    /// `threshold`, but only switch back to P2P at `threshold - 2`. This
    /// avoids oscillation when peer count hovers around the threshold.
    async fn check_mode_switch(&self) -> Result<()> {
        let peer_count = self.peers.len();
        let threshold = self.config.sfu_threshold;
        // Hysteresis: switch back to P2P only when count drops to threshold - 2
        // (minimum of 1 to avoid underflow when threshold <= 2)
        let p2p_threshold = threshold.saturating_sub(2).max(1);
        let mut mode = self.mode.write().await;

        match *mode {
            RoomMode::P2P if peer_count >= threshold => {
                info!(
                    room_id = %self.id,
                    peer_count,
                    threshold,
                    "Switching from P2P to Migrating mode (threshold reached)"
                );
                *mode = RoomMode::Migrating;

                // Update statistics
                let mut stats = self.stats.write().await;
                stats.mode_switches += 1;
                drop(stats);
                drop(mode);

                // Start forwarding all published tracks.
                // During migration, both P2P and SFU paths may be active.
                // The API layer handles sending migration offers to existing peers.
                self.switch_to_sfu().await?;
            }
            RoomMode::Migrating if peer_count < p2p_threshold => {
                // Migration was in progress but peers left, drop back to P2P
                info!(
                    room_id = %self.id,
                    peer_count,
                    p2p_threshold,
                    "Switching from Migrating to P2P mode (peers left during migration)"
                );
                *mode = RoomMode::P2P;

                let mut stats = self.stats.write().await;
                stats.mode_switches += 1;
                drop(stats);
                drop(mode);

                self.switch_to_p2p().await?;
            }
            RoomMode::SFU if peer_count < p2p_threshold => {
                info!(
                    room_id = %self.id,
                    peer_count,
                    p2p_threshold,
                    "Switching from SFU to P2P mode (hysteresis)"
                );
                *mode = RoomMode::P2P;

                // Update statistics
                let mut stats = self.stats.write().await;
                stats.mode_switches += 1;
                drop(stats);
                drop(mode);

                // Stop forwarding all tracks
                self.switch_to_p2p().await?;
            }
            _ => {}
        }

        Ok(())
    }

    /// Mark the room as fully migrated to SFU mode.
    ///
    /// Called by the API layer after all existing P2P peers have completed
    /// migration (or timed out). Transitions from `Migrating` to `SFU`.
    pub async fn complete_migration(&self) -> Result<()> {
        let mut mode = self.mode.write().await;
        if *mode == RoomMode::Migrating {
            info!(
                room_id = %self.id,
                "Migration complete, switching to SFU mode"
            );
            *mode = RoomMode::SFU;
        }
        Ok(())
    }

    /// Switch to SFU mode - start forwarding all tracks.
    ///
    /// If any track fails to start forwarding, all previously started tasks
    /// for this switch are rolled back (aborted) to avoid inconsistent state.
    pub(crate) async fn switch_to_sfu(&self) -> Result<()> {
        // Collect all tracks to forward first (avoid holding DashMap refs across await)
        let tracks_to_forward: Vec<(TrackId, PeerId, Arc<MediaTrack>)> = self
            .published_tracks
            .iter()
            .map(|entry| {
                let track_id = entry.key().clone();
                let (publisher_peer_id, track) = entry.value().clone();
                (track_id, publisher_peer_id, track)
            })
            .collect();

        let mut started_track_ids: Vec<TrackId> = Vec::with_capacity(tracks_to_forward.len());

        for (track_id, publisher_peer_id, track) in tracks_to_forward {
            match self
                .start_track_forwarding(track_id.clone(), track, publisher_peer_id)
                .await
            {
                Ok(()) => {
                    started_track_ids.push(track_id);
                }
                Err(e) => {
                    // Rollback: abort all previously started forwarding tasks
                    error!(
                        room_id = %self.id,
                        failed_track = %track_id,
                        error = %e,
                        started_count = started_track_ids.len(),
                        "Failed to start track forwarding, rolling back"
                    );
                    for rollback_id in &started_track_ids {
                        if let Some((_, task)) = self.forwarding_tasks.remove(rollback_id) {
                            task.abort();
                        }
                    }
                    return Err(e);
                }
            }
        }

        info!(
            room_id = %self.id,
            track_count = started_track_ids.len(),
            "Started forwarding for all tracks"
        );

        Ok(())
    }

    /// Switch to P2P mode - stop forwarding all tracks
    pub(crate) async fn switch_to_p2p(&self) -> Result<()> {
        // Stop all forwarding tasks
        let track_ids: Vec<TrackId> = self
            .forwarding_tasks
            .iter()
            .map(|entry| entry.key().clone())
            .collect();

        for track_id in track_ids {
            if let Some((_, task)) = self.forwarding_tasks.remove(&track_id) {
                task.abort();
            }
        }

        info!(
            room_id = %self.id,
            "Stopped all track forwarding tasks"
        );

        Ok(())
    }

    /// Get current peer count
    pub async fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Check if room is empty
    pub async fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Get room statistics
    pub async fn get_stats(&self) -> RoomStats {
        let mut stats = self.stats.read().await.clone();

        // Update current peer count
        stats.peer_count = self.peers.len();

        // Read hot-path counters from atomics
        stats.packets_relayed = self.packets_relayed.load(Ordering::Relaxed);
        stats.bytes_relayed = self.bytes_relayed.load(Ordering::Relaxed);

        // Count tracks by type
        stats.audio_tracks = 0;
        stats.video_tracks = 0;

        for entry in &self.published_tracks {
            let (_, track) = entry.value();
            if track.is_audio() {
                stats.audio_tracks += 1;
            } else if track.is_video() {
                stats.video_tracks += 1;
            }
        }

        stats
    }

    /// Get current room mode
    pub async fn get_mode(&self) -> RoomMode {
        *self.mode.read().await
    }

    /// Get list of all peer IDs
    #[must_use] 
    pub fn get_peer_ids(&self) -> Vec<PeerId> {
        self.peers.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Get list of all published track IDs
    #[must_use]
    pub fn get_track_ids(&self) -> Vec<TrackId> {
        self.published_tracks
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Get network quality stats for all peers in the room
    #[must_use] 
    pub fn get_network_quality_stats(&self) -> Vec<(String, crate::network_monitor::NetworkStats)> {
        self.network_monitor.get_all_stats()
    }

    /// Get network quality monitor (for advanced use)
    #[must_use]
    pub const fn network_monitor(&self) -> &Arc<NetworkQualityMonitor> {
        &self.network_monitor
    }
}

impl Drop for SfuRoom {
    fn drop(&mut self) {
        let task_count = self.forwarding_tasks.len();
        let peer_count = self.peers.len();
        let track_count = self.published_tracks.len();
        let sub_count = self.track_subscribers.len();

        // Abort all forwarding tasks to prevent leaked spawned tasks
        for entry in self.forwarding_tasks.iter() {
            entry.value().abort();
        }

        // Deactivate all media tracks so their RTP reader tasks are cancelled
        for entry in &self.published_tracks {
            let (_, track) = entry.value();
            track.deactivate();
        }

        // Clear all state
        self.forwarding_tasks.clear();
        self.track_subscribers.clear();
        self.published_tracks.clear();
        self.peers.clear();

        if task_count > 0 || peer_count > 0 {
            info!(
                room_id = %self.id,
                task_count,
                peer_count,
                track_count,
                sub_count,
                "Room dropped, cleaned up all resources"
            );
        } else {
            debug!(
                room_id = %self.id,
                "Room dropped (was already empty)"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_room_creation() {
        let config = Arc::new(SfuConfig::default());
        let room = SfuRoom::new(RoomId::from("test-room"), config);

        assert_eq!(room.get_mode().await, RoomMode::P2P);
        assert!(room.is_empty().await);
    }

    #[tokio::test]
    async fn test_peer_lifecycle() {
        let config = Arc::new(SfuConfig::default());
        let room = SfuRoom::new(RoomId::from("test-room"), config);

        // Add peer
        let peer_id = PeerId::from("peer1");
        room.add_peer(peer_id.clone(), 0).await.unwrap();
        assert_eq!(room.peer_count().await, 1);
        assert!(!room.is_empty().await);

        // Remove peer
        room.remove_peer(&peer_id).await.unwrap();
        assert_eq!(room.peer_count().await, 0);
        assert!(room.is_empty().await);
    }

    #[tokio::test]
    async fn test_mode_switch_with_hysteresis() {
        let mut config = SfuConfig::default();
        config.sfu_threshold = 3;
        let config = Arc::new(config);

        let room = SfuRoom::new(RoomId::from("test-room"), config);

        // Start in P2P mode
        assert_eq!(room.get_mode().await, RoomMode::P2P);

        // Add peers up to threshold
        room.add_peer(PeerId::from("peer1"), 0).await.unwrap();
        room.add_peer(PeerId::from("peer2"), 0).await.unwrap();
        assert_eq!(room.get_mode().await, RoomMode::P2P);

        // Should switch to Migrating at threshold (3) - not SFU yet
        // (SFU state requires explicit migration completion by API layer)
        room.add_peer(PeerId::from("peer3"), 0).await.unwrap();
        assert_eq!(room.get_mode().await, RoomMode::Migrating);

        // Removing one peer (count=2) should NOT switch back due to hysteresis
        // (p2p_threshold = 3 - 2 = 1, so need count < 1 to switch back)
        room.remove_peer(&PeerId::from("peer3")).await.unwrap();
        assert_eq!(room.get_mode().await, RoomMode::Migrating);

        // Removing another peer (count=1) should still be Migrating (1 >= 1)
        room.remove_peer(&PeerId::from("peer2")).await.unwrap();
        assert_eq!(room.get_mode().await, RoomMode::Migrating);

        // Removing the last peer (count=0) should switch back to P2P (0 < 1)
        room.remove_peer(&PeerId::from("peer1")).await.unwrap();
        assert_eq!(room.get_mode().await, RoomMode::P2P);
    }

    #[tokio::test]
    async fn test_track_id_conflict_detection() {
        use std::sync::Arc;

        let config = Arc::new(SfuConfig::default());
        let room = SfuRoom::new(RoomId::from("test-room"), config);

        // Add two peers
        let peer1 = PeerId::from("peer1");
        let peer2 = PeerId::from("peer2");
        room.add_peer(peer1.clone(), 0).await.unwrap();
        room.add_peer(peer2.clone(), 0).await.unwrap();

        // Note: MediaTrack::new requires a real TrackRemote which is hard to mock,
        // so we'll test the conflict check indirectly by verifying the room prevents duplicate IDs.
        //
        // For now, verify that the published_tracks map correctly detects conflicts
        // by manually inserting a track (simulating what add_published_track does).

        // Since we can't easily construct a MediaTrack in tests without a real RTCTrack,
        // this test documents the intended behavior. Real integration tests would be needed
        // to fully test this path.

        // Document expected behavior in comments:
        // 1. peer1 publishes track with id "track123" -> success
        // 2. peer2 tries to publish track with same id "track123" -> fails with track ID conflict error
    }
}
