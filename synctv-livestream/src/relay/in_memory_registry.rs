//! In-memory [`StreamRegistryTrait`] implementation for standalone mode without Redis.
//!
//! Provides the same semantics as the Redis-backed `StreamRegistry` using
//! `tokio::sync::Mutex<HashMap>` for thread-safe, single-node publisher tracking.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

use super::registry::PublisherInfo;
use super::registry_trait::{ActivePublisherEntry, PublisherRefreshOutcome, StreamRegistryTrait};
use crate::util::{validate_stream_id_component, validate_stream_ids};

/// In-memory stream registry for standalone mode without Redis.
///
/// Uses a `Mutex<HashMap>` for atomic publisher registration (single-publisher-per-media
/// enforcement). Epoch counters are tracked locally for split-brain detection parity
/// with the Redis-backed implementation.
/// Data is lost on process restart, which is acceptable for single-node deployments.
type PublisherKey = (String, String);

#[derive(Debug, Default)]
struct InMemoryRegistryState {
    publishers: HashMap<PublisherKey, PublisherInfo>,
    room_streams: HashMap<String, HashSet<String>>,
    user_publishers: HashMap<String, HashSet<PublisherKey>>,
    node_publishers: HashMap<String, HashSet<PublisherKey>>,
}

impl InMemoryRegistryState {
    fn insert_publisher(&mut self, key: PublisherKey, publisher: PublisherInfo) {
        self.room_streams
            .entry(key.0.clone())
            .or_default()
            .insert(key.1.clone());

        if !publisher.user_id.is_empty() {
            self.user_publishers
                .entry(publisher.user_id.clone())
                .or_default()
                .insert(key.clone());
        }

        self.node_publishers
            .entry(publisher.node_id.clone())
            .or_default()
            .insert(key.clone());

        self.publishers.insert(key, publisher);
    }

    fn remove_publisher(&mut self, key: &PublisherKey) -> Option<PublisherInfo> {
        let publisher = self.publishers.remove(key)?;

        if let Some(room_streams) = self.room_streams.get_mut(&key.0) {
            room_streams.remove(&key.1);
            if room_streams.is_empty() {
                self.room_streams.remove(&key.0);
            }
        }

        if !publisher.user_id.is_empty() {
            if let Some(user_publishers) = self.user_publishers.get_mut(&publisher.user_id) {
                user_publishers.remove(key);
                if user_publishers.is_empty() {
                    self.user_publishers.remove(&publisher.user_id);
                }
            }
        }

        if let Some(node_publishers) = self.node_publishers.get_mut(&publisher.node_id) {
            node_publishers.remove(key);
            if node_publishers.is_empty() {
                self.node_publishers.remove(&publisher.node_id);
            }
        }

        Some(publisher)
    }
}

#[derive(Debug, Clone)]
pub struct InMemoryStreamRegistry {
    state: Arc<Mutex<InMemoryRegistryState>>,
    next_epoch: Arc<AtomicU64>,
}

impl InMemoryStreamRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(InMemoryRegistryState::default())),
            next_epoch: Arc::new(AtomicU64::new(1)),
        }
    }
}

impl Default for InMemoryStreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamRegistryTrait for InMemoryStreamRegistry {
    async fn register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        app_name: &str,
        api_address: &str,
    ) -> Result<bool> {
        use std::collections::hash_map::Entry;
        validate_stream_ids(room_id, media_id)?;
        let mut state = self.state.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        match state.publishers.entry(key.clone()) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(_) => {
                let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
                state.insert_publisher(
                    key,
                    PublisherInfo {
                        node_id: node_id.to_string(),
                        api_address: api_address.to_string(),
                        app_name: app_name.to_string(),
                        user_id: String::new(),
                        started_at: Utc::now(),
                        epoch,
                    },
                );
                Ok(true)
            }
        }
    }

    async fn try_register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        api_address: &str,
    ) -> Result<bool> {
        use std::collections::hash_map::Entry;
        validate_stream_ids(room_id, media_id)?;
        let mut state = self.state.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        match state.publishers.entry(key.clone()) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(_) => {
                let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
                state.insert_publisher(
                    key,
                    PublisherInfo {
                        node_id: node_id.to_string(),
                        api_address: api_address.to_string(),
                        app_name: "live".to_string(),
                        user_id: user_id.to_string(),
                        started_at: Utc::now(),
                        epoch,
                    },
                );
                Ok(true)
            }
        }
    }

    async fn refresh_publisher_ttl(
        &self,
        room_id: &str,
        media_id: &str,
        user_id: &str,
        node_id: &str,
        expected_epoch: u64,
    ) -> Result<PublisherRefreshOutcome> {
        validate_stream_ids(room_id, media_id)?;
        let state = self.state.lock().await;
        Ok(
            match state
                .publishers
                .get(&(room_id.to_string(), media_id.to_string()))
            {
                Some(publisher)
                    if (!user_id.is_empty() && publisher.user_id != user_id)
                        || (!node_id.is_empty() && publisher.node_id != node_id)
                        || publisher.epoch != expected_epoch =>
                {
                    PublisherRefreshOutcome::OwnershipChanged
                }
                Some(_) => PublisherRefreshOutcome::Refreshed,
                None => PublisherRefreshOutcome::Missing,
            },
        )
    }

    async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        let mut state = self.state.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        state.remove_publisher(&key);
        Ok(())
    }

    async fn unregister_publisher_if_epoch_matches(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: u64,
    ) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        let key = (room_id.to_string(), media_id.to_string());
        let mut state = self.state.lock().await;

        if state
            .publishers
            .get(&key)
            .is_some_and(|publisher| publisher.epoch == expected_epoch)
        {
            state.remove_publisher(&key);
        }

        Ok(())
    }

    async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>> {
        validate_stream_ids(room_id, media_id)?;
        let state = self.state.lock().await;
        Ok(state
            .publishers
            .get(&(room_id.to_string(), media_id.to_string()))
            .cloned())
    }

    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let state = self.state.lock().await;
        Ok(state
            .publishers
            .contains_key(&(room_id.to_string(), media_id.to_string())))
    }

    async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>> {
        let state = self.state.lock().await;
        Ok(state
            .publishers
            .iter()
            .map(|((room_id, media_id), publisher)| ActivePublisherEntry {
                room_id: room_id.clone(),
                media_id: media_id.clone(),
                publisher: publisher.clone(),
            })
            .collect())
    }

    async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
        let state = self.state.lock().await;
        Ok(state.publishers.keys().cloned().collect())
    }

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        validate_stream_id_component(room_id, "room_id")?;
        let state = self.state.lock().await;
        Ok(state
            .room_streams
            .get(room_id)
            .map(|streams| streams.iter().cloned().collect())
            .unwrap_or_default())
    }

    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let state = self.state.lock().await;
        Ok(state
            .user_publishers
            .get(user_id)
            .map(|publishers| publishers.iter().cloned().collect())
            .unwrap_or_default())
    }

    async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>> {
        validate_stream_id_component(room_id, "room_id")?;
        let state = self.state.lock().await;
        Ok(state
            .user_publishers
            .get(user_id)
            .map(|publishers| {
                publishers
                    .iter()
                    .filter(|(publisher_room_id, _)| publisher_room_id == room_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        let keys: Vec<_> = state
            .user_publishers
            .get(user_id)
            .map(|publishers| publishers.iter().cloned().collect())
            .unwrap_or_default();
        for key in keys {
            state.remove_publisher(&key);
        }
        Ok(())
    }

    async fn validate_epoch(&self, room_id: &str, media_id: &str, epoch: u64) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let state = self.state.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        match state.publishers.get(&key) {
            Some(info) => Ok(info.epoch == epoch),
            None => Ok(false),
        }
    }

    async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        let keys: Vec<_> = state
            .node_publishers
            .get(node_id)
            .map(|publishers| publishers.iter().cloned().collect())
            .unwrap_or_default();
        for key in keys {
            state.remove_publisher(&key);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_registry_rejects_ambiguous_stream_ids() {
        let registry = InMemoryStreamRegistry::new();
        let error = registry
            .try_register_publisher("room:1", "media", "node1", "user1", "addr")
            .await
            .expect_err("ambiguous room id must be rejected");
        assert!(error.to_string().contains("room_id"));

        let error = registry
            .get_publisher("room", "../media")
            .await
            .expect_err("path-like media id must be rejected");
        assert!(error.to_string().contains("media_id"));
    }
}
