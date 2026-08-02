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
use tokio::time::Instant;

use super::registry::{StreamGeneration, HLS_GENERATION_RETENTION};
use super::registry_trait::{ActiveStreamGeneration, LeaseRefreshOutcome, StreamRegistryTrait};
use crate::util::{
    validate_publisher_cluster_address, validate_stream_id_component, validate_stream_ids,
};

/// In-memory stream registry for standalone mode without Redis.
///
/// Uses a `Mutex<HashMap>` for atomic publisher registration (single-publisher-per-media
/// enforcement). Epoch counters are tracked locally for split-brain detection parity
/// with the Redis-backed implementation.
/// Data is lost on process restart, which is acceptable for single-node deployments.
type StreamKey = (String, String);
type GenerationKey = (String, String, String);

#[derive(Debug, Default)]
struct InMemoryRegistryState {
    active_generations: HashMap<StreamKey, String>,
    generations: HashMap<GenerationKey, GenerationRecord>,
}

#[derive(Debug)]
struct GenerationRecord {
    generation: StreamGeneration,
    expires_at: Option<Instant>,
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

    fn purge_expired(state: &mut InMemoryRegistryState) {
        let now = Instant::now();
        let expired: Vec<GenerationKey> = state
            .generations
            .iter()
            .filter(|(_, record)| {
                record
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= now)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for (room_id, media_id, generation_id) in expired {
            state
                .active_generations
                .remove(&(room_id.clone(), media_id.clone()));
            state
                .generations
                .remove(&(room_id, media_id, generation_id));
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
    async fn try_activate_generation(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        cluster_address: &str,
        generation_id: &str,
    ) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        crate::util::validate_stream_generation_id(generation_id)?;
        validate_publisher_cluster_address(cluster_address, node_id, room_id, media_id)?;
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        let key = (room_id.to_string(), media_id.to_string());

        if state.active_generations.contains_key(&key) {
            return Ok(false);
        }
        let lease_epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
        let generation = StreamGeneration {
            node_id: node_id.to_string(),
            cluster_address: cluster_address.to_string(),
            app_name: "live".to_string(),
            user_id: user_id.to_string(),
            started_at: synctv_core::SystemClock.now(),
            ended_at: None,
            lease_epoch,
            generation_id: generation_id.to_string(),
        };
        state
            .active_generations
            .insert(key.clone(), generation_id.to_string());
        state.generations.insert(
            (key.0, key.1, generation_id.to_string()),
            GenerationRecord {
                generation,
                expires_at: None,
            },
        );
        Ok(true)
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
        validate_stream_ids(room_id, media_id)?;
        crate::util::validate_stream_generation_id(generation_id)?;
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        let stream_key = (room_id.to_string(), media_id.to_string());
        let Some(active_generation_id) = state.active_generations.get(&stream_key) else {
            return Ok(LeaseRefreshOutcome::Missing);
        };
        if active_generation_id != generation_id {
            return Ok(LeaseRefreshOutcome::OwnershipChanged);
        }
        let generation_key = (stream_key.0, stream_key.1, generation_id.to_string());
        Ok(match state.generations.get(&generation_key) {
            Some(record)
                if (!user_id.is_empty() && record.generation.user_id != user_id)
                    || (!node_id.is_empty() && record.generation.node_id != node_id)
                    || record.generation.lease_epoch != expected_lease_epoch =>
            {
                LeaseRefreshOutcome::OwnershipChanged
            }
            Some(_) => LeaseRefreshOutcome::Refreshed,
            None => LeaseRefreshOutcome::Missing,
        })
    }

    async fn deactivate_current_generation(&self, room_id: &str, media_id: &str) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        let key = (room_id.to_string(), media_id.to_string());
        if let Some(generation_id) = state.active_generations.remove(&key) {
            state.generations.remove(&(key.0, key.1, generation_id));
        }
        Ok(())
    }

    async fn deactivate_generation_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        let key = (room_id.to_string(), media_id.to_string());
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);

        let generation_key = (
            room_id.to_string(),
            media_id.to_string(),
            generation_id.to_string(),
        );
        let owns_lease = state.active_generations.get(&key).is_some_and(|active| {
            active == generation_id
                && state
                    .generations
                    .get(&generation_key)
                    .is_some_and(|record| record.generation.lease_epoch == expected_lease_epoch)
        });
        if owns_lease {
            state.active_generations.remove(&key);
            state.generations.remove(&generation_key);
        }

        Ok(())
    }

    async fn deactivate_generation_preserving_hls_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        let key = (room_id.to_string(), media_id.to_string());
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        let generation_key = (
            room_id.to_string(),
            media_id.to_string(),
            generation_id.to_string(),
        );
        let owns_lease = state.active_generations.get(&key).is_some_and(|active| {
            active == generation_id
                && state
                    .generations
                    .get(&generation_key)
                    .is_some_and(|record| record.generation.lease_epoch == expected_lease_epoch)
        });
        if !owns_lease {
            return Ok(());
        }
        state.active_generations.remove(&key);
        let record = state
            .generations
            .get_mut(&generation_key)
            .expect("generation lease was checked while holding the registry lock");
        record.generation.ended_at = Some(synctv_core::SystemClock.now());
        record.expires_at = Some(Instant::now() + HLS_GENERATION_RETENTION);
        Ok(())
    }

    async fn get_active_generation(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        validate_stream_ids(room_id, media_id)?;
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        let key = (room_id.to_string(), media_id.to_string());
        Ok(state
            .active_generations
            .get(&key)
            .and_then(|generation_id| {
                state
                    .generations
                    .get(&(key.0, key.1, generation_id.clone()))
                    .map(|record| record.generation.clone())
            }))
    }

    async fn get_generation(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        validate_stream_ids(room_id, media_id)?;
        crate::util::validate_stream_generation_id(generation_id)?;
        let generation_key = (
            room_id.to_string(),
            media_id.to_string(),
            generation_id.to_string(),
        );
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        Ok(state
            .generations
            .get(&generation_key)
            .map(|record| record.generation.clone()))
    }

    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        Ok(state
            .active_generations
            .contains_key(&(room_id.to_string(), media_id.to_string())))
    }

    async fn list_active_generations(&self) -> Result<Vec<ActiveStreamGeneration>> {
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        Ok(state
            .active_generations
            .iter()
            .filter_map(|((room_id, media_id), generation_id)| {
                state
                    .generations
                    .get(&(room_id.clone(), media_id.clone(), generation_id.clone()))
                    .map(|record| ActiveStreamGeneration {
                        room_id: room_id.clone(),
                        media_id: media_id.clone(),
                        generation: record.generation.clone(),
                    })
            })
            .collect())
    }

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        validate_stream_id_component(room_id, "room_id")?;
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        Ok(state
            .active_generations
            .keys()
            .filter(|(publisher_room_id, _)| publisher_room_id == room_id)
            .map(|(_, media_id)| media_id.clone())
            .collect())
    }

    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        Ok(state
            .active_generations
            .iter()
            .filter(|((room_id, media_id), generation_id)| {
                state
                    .generations
                    .get(&(room_id.clone(), media_id.clone(), (*generation_id).clone()))
                    .is_some_and(|record| record.generation.user_id == user_id)
            })
            .map(|((room_id, media_id), _)| (room_id.clone(), media_id.clone()))
            .collect())
    }

    async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>> {
        validate_stream_id_component(room_id, "room_id")?;
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        Ok(state
            .active_generations
            .iter()
            .filter(|((publisher_room_id, media_id), generation_id)| {
                publisher_room_id == room_id
                    && state
                        .generations
                        .get(&(
                            publisher_room_id.clone(),
                            media_id.clone(),
                            (*generation_id).clone(),
                        ))
                        .is_some_and(|record| record.generation.user_id == user_id)
            })
            .map(|((room_id, media_id), _)| (room_id.clone(), media_id.clone()))
            .collect())
    }

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

    async fn cleanup_all_generations_for_node(&self, node_id: &str) -> Result<()> {
        let mut state = self.state.lock().await;
        Self::purge_expired(&mut state);
        let generation_ids = state
            .active_generations
            .iter()
            .filter_map(|(stream_key, generation_id)| {
                state
                    .generations
                    .get(&(
                        stream_key.0.clone(),
                        stream_key.1.clone(),
                        generation_id.clone(),
                    ))
                    .filter(|record| record.generation.node_id == node_id)
                    .map(|_| (stream_key.clone(), generation_id.clone()))
            })
            .collect::<Vec<_>>();
        for (stream_key, generation_id) in generation_ids {
            state.active_generations.remove(&stream_key);
            state
                .generations
                .remove(&(stream_key.0, stream_key.1, generation_id));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TEST_GENERATION_ID;
    use std::time::Duration;

    type TestResult = anyhow::Result<()>;

    #[tokio::test]
    async fn in_memory_registry_registers_and_rejects_duplicates() -> TestResult {
        let registry = InMemoryStreamRegistry::new();

        let first = registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(first);

        let second = registry
            .try_activate_generation(
                "room1",
                "media1",
                "node2",
                "user2",
                "localhost:50052",
                TEST_GENERATION_ID,
            )
            .await?;
        assert!(!second);

        let publisher = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?;
        assert_eq!(publisher.node_id, "node1");
        assert_eq!(publisher.user_id, "user1");
        assert_eq!(publisher.cluster_address, "localhost:50051");
        assert_eq!(publisher.lease_epoch, 1);
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_registry_purges_expired_retained_generations_on_access() -> TestResult {
        let registry = InMemoryStreamRegistry::new();
        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        let lease_epoch = registry
            .get_active_generation("room1", "media1")
            .await?
            .expect("active generation should exist")
            .lease_epoch;
        registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                "room1",
                "media1",
                TEST_GENERATION_ID,
                lease_epoch,
            )
            .await?;

        {
            let mut state = registry.state.lock().await;
            state
                .generations
                .get_mut(&(
                    "room1".to_string(),
                    "media1".to_string(),
                    TEST_GENERATION_ID.to_string(),
                ))
                .expect("retained generation should exist")
                .expires_at = Some(Instant::now() - Duration::from_secs(1));
        }

        assert!(registry
            .get_generation("room1", "media1", TEST_GENERATION_ID)
            .await?
            .is_none());
        assert!(registry.state.lock().await.generations.is_empty());
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_registry_keeps_epoch_monotonic_after_unregister() -> TestResult {
        let registry = InMemoryStreamRegistry::new();

        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        let first_epoch = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?
            .lease_epoch;

        registry
            .deactivate_current_generation("room1", "media1")
            .await?;
        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node2",
                "user2",
                "localhost:50052",
                TEST_GENERATION_ID,
            )
            .await?;

        let second_epoch = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?
            .lease_epoch;
        assert!(second_epoch > first_epoch);
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_registry_user_indexes_and_epoch_checks_work() -> TestResult {
        let registry = InMemoryStreamRegistry::new();

        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .try_activate_generation(
                "room2",
                "media2",
                "node1",
                "user1",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;

        let publisher = registry
            .get_active_generation("room1", "media1")
            .await?
            .ok_or_else(|| anyhow::anyhow!("publisher should exist"))?;
        assert!(
            registry
                .validate_lease("room1", "media1", TEST_GENERATION_ID, publisher.lease_epoch,)
                .await?
        );
        assert!(
            !registry
                .validate_lease("room1", "media1", TEST_GENERATION_ID, 999)
                .await?
        );

        let user_publishers = registry.get_user_publishers("user1").await?;
        assert_eq!(user_publishers.len(), 2);

        let wrong_node = registry
            .refresh_generation_lease(
                "room1",
                "media1",
                TEST_GENERATION_ID,
                "user1",
                "node2",
                publisher.lease_epoch,
            )
            .await?;
        assert_eq!(wrong_node, LeaseRefreshOutcome::OwnershipChanged);

        assert_eq!(registry.get_user_publishers("user1").await?.len(), 2);
        Ok(())
    }

    #[tokio::test]
    async fn in_memory_registry_rejects_ambiguous_stream_ids() {
        let registry = InMemoryStreamRegistry::new();
        let error = registry
            .try_activate_generation(
                "room:1",
                "media",
                "node1",
                "user1",
                "addr",
                TEST_GENERATION_ID,
            )
            .await
            .expect_err("ambiguous room id must be rejected");
        assert!(error.to_string().contains("room_id"));

        let error = registry
            .get_active_generation("room", "../media")
            .await
            .expect_err("path-like media id must be rejected");
        assert!(error.to_string().contains("media_id"));
    }

    #[tokio::test]
    async fn in_memory_registry_rejects_empty_cluster_address() {
        let registry = InMemoryStreamRegistry::new();
        let error = registry
            .try_activate_generation("room1", "media1", "node1", "user1", " ", TEST_GENERATION_ID)
            .await
            .expect_err("empty cluster_address must be rejected");

        assert!(error.to_string().contains("cluster_address"));
    }
}
