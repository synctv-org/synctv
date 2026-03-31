//! In-memory [`StreamRegistryTrait`] implementation for standalone mode without Redis.
//!
//! Provides the same semantics as the Redis-backed `StreamRegistry` using
//! `tokio::sync::Mutex<HashMap>` for thread-safe, single-node publisher tracking.

use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

use super::registry::PublisherInfo;
use super::registry_trait::{PublisherRefreshOutcome, StreamRegistryTrait};

/// In-memory stream registry for standalone mode without Redis.
///
/// Uses a `Mutex<HashMap>` for atomic publisher registration (single-publisher-per-media
/// enforcement). Epoch counters are tracked locally for split-brain detection parity
/// with the Redis-backed implementation.
///
/// Data is lost on process restart, which is acceptable for single-node deployments.

#[derive(Debug, Clone)]
pub struct InMemoryStreamRegistry {
    publishers: Arc<Mutex<HashMap<(String, String), PublisherInfo>>>,
    next_epoch: Arc<AtomicU64>,
}

impl InMemoryStreamRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            publishers: Arc::new(Mutex::new(HashMap::new())),
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
        let mut publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        match publishers.entry(key.clone()) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(vacant) => {
                let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);

                vacant.insert(PublisherInfo {
                    node_id: node_id.to_string(),
                    api_address: api_address.to_string(),
                    app_name: app_name.to_string(),
                    user_id: String::new(),
                    started_at: Utc::now(),
                    epoch,
                });
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
        let mut publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        match publishers.entry(key.clone()) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(vacant) => {
                let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);

                vacant.insert(PublisherInfo {
                    node_id: node_id.to_string(),
                    api_address: api_address.to_string(),
                    app_name: "live".to_string(),
                    user_id: user_id.to_string(),
                    started_at: Utc::now(),
                    epoch,
                });
                Ok(true)
            }
        }
    }

    async fn refresh_publisher_ttl(
        &self,
        room_id: &str,
        media_id: &str,
        _user_id: &str,
    ) -> Result<PublisherRefreshOutcome> {
        let publishers = self.publishers.lock().await;
        Ok(
            if publishers.contains_key(&(room_id.to_string(), media_id.to_string())) {
                PublisherRefreshOutcome::Refreshed
            } else {
                PublisherRefreshOutcome::Missing
            },
        )
    }

    async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        let mut publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        publishers.remove(&key);
        Ok(())
    }

    async fn unregister_publisher_if_epoch_matches(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: u64,
    ) -> Result<()> {
        let key = (room_id.to_string(), media_id.to_string());
        let mut publishers = self.publishers.lock().await;

        if publishers
            .get(&key)
            .is_some_and(|publisher| publisher.epoch == expected_epoch)
        {
            publishers.remove(&key);
        }

        Ok(())
    }

    async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>> {
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .get(&(room_id.to_string(), media_id.to_string()))
            .cloned())
    }

    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        let publishers = self.publishers.lock().await;
        Ok(publishers.contains_key(&(room_id.to_string(), media_id.to_string())))
    }

    async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
        let publishers = self.publishers.lock().await;
        Ok(publishers.keys().cloned().collect())
    }

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .keys()
            .filter(|(rid, _)| rid == room_id)
            .map(|(_, media_id)| media_id.clone())
            .collect())
    }

    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .iter()
            .filter(|(_, info)| info.user_id == user_id)
            .map(|((room_id, media_id), _)| (room_id.clone(), media_id.clone()))
            .collect())
    }

    async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()> {
        let mut publishers = self.publishers.lock().await;
        publishers.retain(|_, info| info.user_id != user_id);
        Ok(())
    }

    async fn validate_epoch(&self, room_id: &str, media_id: &str, epoch: u64) -> Result<bool> {
        let publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        match publishers.get(&key) {
            Some(info) => Ok(info.epoch == epoch),
            None => Ok(false),
        }
    }

    async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()> {
        let mut publishers = self.publishers.lock().await;
        publishers.retain(|_, info| info.node_id != node_id);
        Ok(())
    }
}
