//! In-memory [`StreamRegistryTrait`] implementation for standalone mode without Redis.
//!
//! Provides the same semantics as the Redis-backed `StreamRegistry` using
//! `tokio::sync::Mutex<HashMap>` for thread-safe, single-node publisher tracking.

use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

use super::registry::PublisherInfo;
use super::registry_trait::{ActivePublisherEntry, PublisherRefreshOutcome, StreamRegistryTrait};
use crate::util::{
    validate_publisher_api_address, validate_stream_id_component, validate_stream_ids,
};

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
}

impl InMemoryRegistryState {
    fn remove_publisher(&mut self, key: &PublisherKey) -> Option<PublisherInfo> {
        self.publishers.remove(key)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct InMemoryStreamRegistry {
    state: Arc<Mutex<InMemoryRegistryState>>,
    next_epoch: Arc<AtomicU64>,
}

impl InMemoryStreamRegistry {
    #[must_use]
    pub(crate) fn new() -> Self {
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
        validate_publisher_api_address(api_address, node_id, room_id, media_id)?;
        let mut state = self.state.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        match state.publishers.entry(key) {
            Entry::Occupied(_) => Ok(false),
            Entry::Vacant(entry) => {
                let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
                entry.insert(PublisherInfo {
                    node_id: node_id.to_string(),
                    api_address: api_address.to_string(),
                    app_name: "live".to_string(),
                    user_id: user_id.to_string(),
                    started_at: synctv_core::SystemClock.now(),
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

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        validate_stream_id_component(room_id, "room_id")?;
        let state = self.state.lock().await;
        Ok(state
            .publishers
            .keys()
            .filter(|(publisher_room_id, _)| publisher_room_id == room_id)
            .map(|(_, media_id)| media_id.clone())
            .collect())
    }

    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let state = self.state.lock().await;
        Ok(state
            .publishers
            .iter()
            .filter(|(_, publisher)| publisher.user_id == user_id)
            .map(|((room_id, media_id), _)| (room_id.clone(), media_id.clone()))
            .collect())
    }

    async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>> {
        validate_stream_id_component(room_id, "room_id")?;
        let state = self.state.lock().await;
        Ok(state
            .publishers
            .iter()
            .filter(|((publisher_room_id, _), publisher)| {
                publisher_room_id == room_id && publisher.user_id == user_id
            })
            .map(|((room_id, media_id), _)| (room_id.clone(), media_id.clone()))
            .collect())
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
        state
            .publishers
            .retain(|_, publisher| publisher.node_id != node_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = anyhow::Result<()>;

    #[tokio::test]
    async fn in_memory_registry_registers_and_rejects_duplicates() -> TestResult {
        let registry = InMemoryStreamRegistry::new();

        let first = registry
            .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
            .await?;
        assert!(first);

        let second = registry
            .try_register_publisher("room1", "media1", "node2", "user2", "localhost:50052")
            .await?;
        assert!(!second);

        let publisher = registry
            .get_publisher("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?;
        assert_eq!(publisher.node_id, "node1");
        assert_eq!(publisher.user_id, "user1");
        assert_eq!(publisher.api_address, "localhost:50051");
        assert_eq!(publisher.epoch, 1);
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_registry_keeps_epoch_monotonic_after_unregister() -> TestResult {
        let registry = InMemoryStreamRegistry::new();

        registry
            .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
            .await?;
        let first_epoch = registry
            .get_publisher("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?
            .epoch;

        registry.unregister_publisher("room1", "media1").await?;
        registry
            .try_register_publisher("room1", "media1", "node2", "user2", "localhost:50052")
            .await?;

        let second_epoch = registry
            .get_publisher("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?
            .epoch;
        assert!(second_epoch > first_epoch);
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_registry_user_indexes_and_epoch_checks_work() -> TestResult {
        let registry = InMemoryStreamRegistry::new();

        registry
            .try_register_publisher("room1", "media1", "node1", "user1", "localhost:50051")
            .await?;
        registry
            .try_register_publisher("room2", "media2", "node1", "user1", "localhost:50051")
            .await?;

        let publisher = registry
            .get_publisher("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?;
        assert!(
            registry
                .validate_epoch("room1", "media1", publisher.epoch)
                .await?
        );
        assert!(!registry.validate_epoch("room1", "media1", 999).await?);

        let user_publishers = registry.get_user_publishers("user1").await?;
        assert_eq!(user_publishers.len(), 2);

        let wrong_node = registry
            .refresh_publisher_ttl("room1", "media1", "user1", "node2", publisher.epoch)
            .await?;
        assert_eq!(wrong_node, PublisherRefreshOutcome::OwnershipChanged);

        assert_eq!(registry.get_user_publishers("user1").await?.len(), 2);
        Ok(())
    }

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

    #[tokio::test]
    async fn in_memory_registry_rejects_empty_api_address() {
        let registry = InMemoryStreamRegistry::new();
        let error = registry
            .try_register_publisher("room1", "media1", "node1", "user1", " ")
            .await
            .expect_err("empty api_address must be rejected");

        assert!(error.to_string().contains("api_address"));
    }
}
