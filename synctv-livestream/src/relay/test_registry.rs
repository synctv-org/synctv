use super::registry::{StreamGeneration, WebRtcSessionOwner};
use super::registry_trait::{
    ActiveStreamGeneration, LeaseRefreshOutcome, LeaseRefreshRequest, StreamGenerationRegistration,
    StreamRegistryTrait,
};
use anyhow::Result;
use async_trait::async_trait;

use crate::util::{validate_stream_id_component, validate_stream_ids};

type GenerationKey = (String, String, String);

#[derive(Debug, Clone)]
pub struct TestStreamRegistry {
    publishers: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<(String, String), StreamGeneration>>,
    >,
    generations: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<GenerationKey, StreamGeneration>>,
    >,
    webrtc_sessions:
        std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<String, WebRtcSessionOwner>>>,
    epoch_counters:
        std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<(String, String), u64>>>,
    register_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    refresh_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    refresh_batch_sizes: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
    block_batch_refresh: std::sync::Arc<std::sync::atomic::AtomicBool>,
    batch_refresh_started: std::sync::Arc<tokio::sync::Semaphore>,
    batch_refresh_release: std::sync::Arc<tokio::sync::Semaphore>,
    unregister_if_lease_matches_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    list_active_generations_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    fail_get_active_generation: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fail_mark_generation_ready: std::sync::Arc<std::sync::atomic::AtomicBool>,
    fail_refresh_generation_lease_with_response_error:
        std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl TestStreamRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            publishers: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            generations: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            webrtc_sessions: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            epoch_counters: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            register_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_batch_sizes: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            block_batch_refresh: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            batch_refresh_started: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            batch_refresh_release: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            unregister_if_lease_matches_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            list_active_generations_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            fail_get_active_generation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            fail_mark_generation_ready: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            fail_refresh_generation_lease_with_response_error: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
        }
    }

    #[must_use]
    pub fn with_publishers(
        publishers: std::collections::HashMap<(String, String), StreamGeneration>,
    ) -> Self {
        let generations = publishers
            .iter()
            .map(|((room_id, media_id), generation)| {
                (
                    (
                        room_id.clone(),
                        media_id.clone(),
                        generation.generation_id.clone(),
                    ),
                    generation.clone(),
                )
            })
            .collect();
        Self {
            publishers: std::sync::Arc::new(tokio::sync::Mutex::new(publishers)),
            generations: std::sync::Arc::new(tokio::sync::Mutex::new(generations)),
            webrtc_sessions: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            epoch_counters: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            register_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_batch_sizes: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            block_batch_refresh: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            batch_refresh_started: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            batch_refresh_release: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
            unregister_if_lease_matches_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            list_active_generations_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            fail_get_active_generation: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            fail_mark_generation_ready: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            fail_refresh_generation_lease_with_response_error: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
        }
    }

    /// Get the count of `register_publisher` calls (for testing task leaks)
    #[must_use]
    pub fn register_call_count(&self) -> usize {
        self.register_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the count of `refresh_generation_lease` calls.
    #[must_use]
    pub fn refresh_call_count(&self) -> usize {
        self.refresh_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    #[must_use]
    pub fn refresh_batch_sizes(&self) -> Vec<usize> {
        self.refresh_batch_sizes
            .lock()
            .expect("refresh batch size mutex should remain available")
            .clone()
    }

    pub fn set_block_batch_refresh(&self, block: bool) {
        self.block_batch_refresh
            .store(block, std::sync::atomic::Ordering::SeqCst);
    }

    pub async fn wait_for_batch_refresh(&self) {
        self.batch_refresh_started
            .acquire()
            .await
            .expect("batch refresh start semaphore should remain open")
            .forget();
    }

    pub fn release_batch_refresh(&self) {
        self.batch_refresh_release.add_permits(1);
    }

    /// Get the count of `deactivate_generation_if_lease_matches` calls.
    #[must_use]
    pub fn unregister_if_lease_matches_call_count(&self) -> usize {
        self.unregister_if_lease_matches_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the count of `list_active_generations` calls.
    #[must_use]
    pub fn list_active_generations_call_count(&self) -> usize {
        self.list_active_generations_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Set whether `get_active_generation` should fail (simulates Redis failure)
    pub fn set_fail_get_active_generation(&self, fail: bool) {
        self.fail_get_active_generation
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn set_fail_mark_generation_ready(&self, fail: bool) {
        self.fail_mark_generation_ready
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    /// Set whether `refresh_generation_lease` should fail with a non-I/O Redis error.
    pub fn set_fail_refresh_generation_lease_with_response_error(&self, fail: bool) {
        self.fail_refresh_generation_lease_with_response_error
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for TestStreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamRegistryTrait for TestStreamRegistry {
    async fn try_activate_generation(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        cluster_address: &str,
        generation_id: &str,
    ) -> Result<bool> {
        self.register_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        validate_stream_ids(room_id, media_id)?;
        crate::util::validate_stream_generation_id(generation_id)?;
        let mut publishers = self.publishers.lock().await;
        let mut epoch_counters = self.epoch_counters.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        if let std::collections::hash_map::Entry::Vacant(entry) = publishers.entry(key.clone()) {
            // Increment lease_epoch counter
            let lease_epoch = epoch_counters.entry(key.clone()).or_insert(0);
            *lease_epoch += 1;

            let generation = StreamGeneration {
                node_id: node_id.to_string(),
                cluster_address: cluster_address.to_string(),
                app_name: "live".to_string(),
                user_id: user_id.to_string(),
                started_at: synctv_core::SystemClock.now(),
                ready_at: None,
                ended_at: None,
                lease_epoch: *lease_epoch,
                generation_id: generation_id.to_string(),
                supports_rtp: false,
            };
            entry.insert(generation.clone());
            self.generations.lock().await.insert(
                (
                    room_id.to_string(),
                    media_id.to_string(),
                    generation_id.to_string(),
                ),
                generation,
            );
            Ok(true)
        } else {
            Ok(false)
        }
    }

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
            .ok_or_else(|| anyhow::anyhow!("test generation disappeared"))?;
        Ok(self
            .set_generation_supports_rtp(
                room_id,
                media_id,
                generation_id,
                generation.lease_epoch,
                true,
            )
            .await?)
    }

    async fn set_generation_supports_rtp(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
        supports_rtp: bool,
    ) -> Result<bool> {
        let key = (room_id.to_string(), media_id.to_string());
        let mut publishers = self.publishers.lock().await;
        let Some(publisher) = publishers.get_mut(&key) else {
            return Ok(false);
        };
        if publisher.generation_id != generation_id || publisher.lease_epoch != expected_lease_epoch
        {
            return Ok(false);
        }
        publisher.supports_rtp = supports_rtp;
        if let Some(generation) = self.generations.lock().await.get_mut(&(
            room_id.to_string(),
            media_id.to_string(),
            generation_id.to_string(),
        )) {
            generation.supports_rtp = supports_rtp;
        }
        Ok(true)
    }

    async fn try_register_webrtc_session(
        &self,
        session_id: &str,
        owner: &WebRtcSessionOwner,
        _ttl: std::time::Duration,
    ) -> Result<bool> {
        crate::util::validate_stream_generation_id(session_id)?;
        validate_stream_ids(&owner.room_id, &owner.media_id)?;
        let mut sessions = self.webrtc_sessions.lock().await;
        if sessions.contains_key(session_id) {
            return Ok(false);
        }
        sessions.insert(session_id.to_string(), owner.clone());
        Ok(true)
    }

    async fn get_webrtc_session_owner(
        &self,
        session_id: &str,
    ) -> Result<Option<WebRtcSessionOwner>> {
        crate::util::validate_stream_generation_id(session_id)?;
        Ok(self.webrtc_sessions.lock().await.get(session_id).cloned())
    }

    async fn unregister_webrtc_session(
        &self,
        session_id: &str,
        expected_node_id: &str,
    ) -> Result<bool> {
        crate::util::validate_stream_generation_id(session_id)?;
        let mut sessions = self.webrtc_sessions.lock().await;
        if sessions
            .get(session_id)
            .is_none_or(|owner| owner.node_id != expected_node_id)
        {
            return Ok(false);
        }
        sessions.remove(session_id);
        Ok(true)
    }

    async fn mark_generation_ready(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool> {
        if self
            .fail_mark_generation_ready
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow::anyhow!(
                "Simulated Redis failure in mark_generation_ready"
            ));
        }
        let key = (room_id.to_string(), media_id.to_string());
        let mut publishers = self.publishers.lock().await;
        let Some(publisher) = publishers.get_mut(&key) else {
            return Ok(false);
        };
        if publisher.generation_id != generation_id || publisher.lease_epoch != expected_lease_epoch
        {
            return Ok(false);
        }
        let ready_at = synctv_core::SystemClock.now();
        publisher.ready_at = Some(ready_at);
        if let Some(generation) = self.generations.lock().await.get_mut(&(
            room_id.to_string(),
            media_id.to_string(),
            generation_id.to_string(),
        )) {
            generation.ready_at = Some(ready_at);
        }
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
        self.refresh_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .fail_refresh_generation_lease_with_response_error
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let redis_error = redis::RedisError::from((
                redis::ErrorKind::Client,
                "simulated Redis client error in refresh_generation_lease",
            ));
            return Err(anyhow::Error::new(redis_error));
        }
        let publishers = self.publishers.lock().await;
        Ok(
            match publishers.get(&(room_id.to_string(), media_id.to_string())) {
                Some(publisher)
                    if publisher.generation_id != generation_id
                        || (!user_id.is_empty() && publisher.user_id != user_id)
                        || (!node_id.is_empty() && publisher.node_id != node_id)
                        || publisher.lease_epoch != expected_lease_epoch =>
                {
                    LeaseRefreshOutcome::OwnershipChanged
                }
                Some(_) => LeaseRefreshOutcome::Refreshed,
                None => LeaseRefreshOutcome::Missing,
            },
        )
    }

    async fn refresh_generation_leases(
        &self,
        node_id: &str,
        requests: &[LeaseRefreshRequest],
    ) -> Result<Vec<LeaseRefreshOutcome>> {
        if self
            .block_batch_refresh
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            self.batch_refresh_started.add_permits(1);
            self.batch_refresh_release
                .acquire()
                .await
                .map_err(anyhow::Error::from)?
                .forget();
        }
        self.refresh_batch_sizes
            .lock()
            .expect("refresh batch size mutex should remain available")
            .push(requests.len());

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

    async fn deactivate_current_generation(&self, room_id: &str, media_id: &str) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        let mut publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        if let Some(generation) = publishers.remove(&key) {
            self.generations
                .lock()
                .await
                .remove(&(key.0, key.1, generation.generation_id));
        }
        Ok(())
    }

    async fn deactivate_generation_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        self.unregister_if_lease_matches_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        let owns_lease = publishers.get(&key).is_some_and(|publisher| {
            publisher.generation_id == generation_id
                && publisher.lease_epoch == expected_lease_epoch
        });
        if owns_lease {
            publishers.remove(&key);
            self.generations
                .lock()
                .await
                .remove(&(key.0, key.1, generation_id.to_string()));
        }
        Ok(owns_lease)
    }

    async fn deactivate_generation_preserving_hls_if_lease_matches(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
        expected_lease_epoch: u64,
    ) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        self.unregister_if_lease_matches_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        let owns_lease = publishers.get(&key).is_some_and(|publisher| {
            publisher.generation_id == generation_id
                && publisher.lease_epoch == expected_lease_epoch
        });
        if owns_lease {
            if let Some(mut generation) = publishers.remove(&key) {
                generation.ended_at = Some(synctv_core::SystemClock.now());
                self.generations
                    .lock()
                    .await
                    .insert((key.0, key.1, generation_id.to_string()), generation);
            }
        }
        Ok(owns_lease)
    }

    async fn get_active_generation(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        validate_stream_ids(room_id, media_id)?;
        if self
            .fail_get_active_generation
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow::anyhow!(
                "Simulated Redis failure in get_active_generation"
            ));
        }
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .get(&(room_id.to_string(), media_id.to_string()))
            .cloned())
    }

    async fn get_generation(
        &self,
        room_id: &str,
        media_id: &str,
        generation_id: &str,
    ) -> Result<Option<StreamGeneration>> {
        validate_stream_ids(room_id, media_id)?;
        crate::util::validate_stream_generation_id(generation_id)?;
        Ok(self
            .generations
            .lock()
            .await
            .get(&(
                room_id.to_string(),
                media_id.to_string(),
                generation_id.to_string(),
            ))
            .cloned())
    }

    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let publishers = self.publishers.lock().await;
        Ok(publishers.contains_key(&(room_id.to_string(), media_id.to_string())))
    }

    async fn list_active_generations(&self) -> Result<Vec<ActiveStreamGeneration>> {
        self.list_active_generations_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .fail_get_active_generation
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow::anyhow!(
                "Simulated Redis failure in list_active_generations"
            ));
        }
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .iter()
            .map(|((room_id, media_id), publisher)| ActiveStreamGeneration {
                room_id: room_id.clone(),
                media_id: media_id.clone(),
                generation: publisher.clone(),
            })
            .collect())
    }

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        validate_stream_id_component(room_id, "room_id")?;
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

    async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>> {
        validate_stream_id_component(room_id, "room_id")?;
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .iter()
            .filter(|((publisher_room_id, _), info)| {
                publisher_room_id == room_id && info.user_id == user_id
            })
            .map(|((room_id, media_id), _)| (room_id.clone(), media_id.clone()))
            .collect())
    }

    async fn cleanup_all_generations_for_node(&self, node_id: &str) -> Result<()> {
        let mut publishers = self.publishers.lock().await;
        let removed = publishers
            .extract_if(|_, info| info.node_id == node_id)
            .map(|((room_id, media_id), info)| (room_id, media_id, info.generation_id))
            .collect::<Vec<_>>();
        let mut generations = self.generations.lock().await;
        for key in removed {
            generations.remove(&key);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::TEST_GENERATION_ID;

    type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn require_publisher(publisher: Option<StreamGeneration>) -> Result<StreamGeneration> {
        publisher.ok_or_else(|| anyhow::anyhow!("publisher should exist"))
    }

    #[tokio::test]
    async fn test_registry_rejects_ambiguous_stream_ids() {
        let registry = TestStreamRegistry::new();

        let error = registry
            .try_activate_generation(
                "room:1",
                "media",
                "node1",
                "user1",
                "10.0.0.1:50051",
                TEST_GENERATION_ID,
            )
            .await
            .expect_err("ambiguous room id must be rejected");
        assert!(error.to_string().contains("room_id"));

        let error = registry
            .is_stream_active("room1", "../media")
            .await
            .expect_err("path-like media id must be rejected");
        assert!(error.to_string().contains("media_id"));
    }

    #[tokio::test]
    async fn test_registry_pre_initialized() -> TestResult {
        let mut publishers = std::collections::HashMap::new();
        publishers.insert(
            ("room1".to_string(), "media1".to_string()),
            StreamGeneration {
                node_id: "node1".to_string(),
                cluster_address: String::new(),
                app_name: "live".to_string(),
                user_id: String::new(),
                started_at: synctv_core::SystemClock.now(),
                ready_at: None,
                ended_at: None,
                lease_epoch: 1,
                generation_id: TEST_GENERATION_ID.to_string(),
                supports_rtp: false,
            },
        );

        let registry = TestStreamRegistry::with_publishers(publishers);

        let result = registry.get_active_generation("room1", "media1").await?;
        assert_eq!(require_publisher(result)?.node_id, "node1");
        Ok(())
    }

    #[tokio::test]
    async fn test_registry_epoch_increments_on_register() -> TestResult {
        let registry = TestStreamRegistry::new();

        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        let info = require_publisher(registry.get_active_generation("room1", "media1").await?)?;
        assert_eq!(info.lease_epoch, 1);

        registry
            .deactivate_current_generation("room1", "media1")
            .await?;

        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node2",
                "",
                "localhost:50052",
                TEST_GENERATION_ID,
            )
            .await?;
        let info = require_publisher(registry.get_active_generation("room1", "media1").await?)?;
        assert_eq!(info.lease_epoch, 2);
        Ok(())
    }

    #[tokio::test]
    async fn generation_readiness_is_fenced_by_generation_and_epoch() -> TestResult {
        let registry = TestStreamRegistry::new();
        registry
            .try_activate_generation(
                "room-ready",
                "media-ready",
                "node-ready",
                "user-ready",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        let generation = require_publisher(
            registry
                .get_active_generation("room-ready", "media-ready")
                .await?,
        )?;
        assert!(generation.ready_at.is_none());

        assert!(
            !registry
                .mark_generation_ready(
                    "room-ready",
                    "media-ready",
                    "00000000-0000-0000-0000-000000000002",
                    generation.lease_epoch,
                )
                .await?
        );
        assert!(
            !registry
                .mark_generation_ready(
                    "room-ready",
                    "media-ready",
                    TEST_GENERATION_ID,
                    generation.lease_epoch + 1,
                )
                .await?
        );
        assert!(
            registry
                .mark_generation_ready(
                    "room-ready",
                    "media-ready",
                    TEST_GENERATION_ID,
                    generation.lease_epoch,
                )
                .await?
        );
        assert!(require_publisher(
            registry
                .get_active_generation("room-ready", "media-ready")
                .await?,
        )?
        .ready_at
        .is_some());
        Ok(())
    }

    #[tokio::test]
    async fn preserving_deactivation_keeps_exact_generation() -> TestResult {
        let registry = TestStreamRegistry::new();
        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        let first = require_publisher(registry.get_active_generation("room1", "media1").await?)?;

        registry
            .deactivate_generation_preserving_hls_if_lease_matches(
                "room1",
                "media1",
                TEST_GENERATION_ID,
                first.lease_epoch,
            )
            .await?;
        assert!(registry
            .get_active_generation("room1", "media1")
            .await?
            .is_none());
        assert_eq!(
            require_publisher(
                registry
                    .get_generation("room1", "media1", TEST_GENERATION_ID)
                    .await?,
            )?
            .lease_epoch,
            first.lease_epoch
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_registry_validate_lease() -> TestResult {
        let registry = TestStreamRegistry::new();

        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        let info = require_publisher(registry.get_active_generation("room1", "media1").await?)?;

        let valid = registry
            .validate_lease("room1", "media1", TEST_GENERATION_ID, info.lease_epoch)
            .await?;
        assert!(valid);

        let valid = registry
            .validate_lease("room1", "media1", TEST_GENERATION_ID, 999)
            .await?;
        assert!(!valid);

        let valid = registry
            .validate_lease("nonexistent", "media", TEST_GENERATION_ID, 1)
            .await?;
        assert!(!valid);
        Ok(())
    }

    #[tokio::test]
    async fn test_registry_cleanup_all_generations_for_node() -> TestResult {
        let registry = TestStreamRegistry::new();

        registry
            .try_activate_generation(
                "room1",
                "media1",
                "node1",
                "",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .try_activate_generation(
                "room1",
                "media2",
                "node1",
                "",
                "localhost:50051",
                TEST_GENERATION_ID,
            )
            .await?;
        registry
            .try_activate_generation(
                "room2",
                "media1",
                "node2",
                "",
                "localhost:50052",
                TEST_GENERATION_ID,
            )
            .await?;

        assert!(registry.is_stream_active("room1", "media1").await?);
        assert!(registry.is_stream_active("room1", "media2").await?);
        assert!(registry.is_stream_active("room2", "media1").await?);

        registry.cleanup_all_generations_for_node("node1").await?;

        assert!(!registry.is_stream_active("room1", "media1").await?);
        assert!(!registry.is_stream_active("room1", "media2").await?);
        assert!(registry.is_stream_active("room2", "media1").await?);
        Ok(())
    }
}
