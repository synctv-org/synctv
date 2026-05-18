// Mock StreamRegistry for testing without Redis

use super::registry::PublisherInfo;
use super::registry_trait::{ActivePublisherEntry, PublisherRefreshOutcome, StreamRegistryTrait};
use super::RedisOperationTimeout;
use anyhow::Result;
use async_trait::async_trait;
use chrono::Utc;

use crate::util::{validate_stream_id_component, validate_stream_ids};

/// Mock `StreamRegistry` for testing without Redis
#[derive(Debug, Clone)]
pub struct MockStreamRegistry {
    publishers: std::sync::Arc<
        tokio::sync::Mutex<std::collections::HashMap<(String, String), PublisherInfo>>,
    >,
    /// Epoch counter for each stream (`room_id`, `media_id`)
    epoch_counters:
        std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<(String, String), u64>>>,
    /// Counter for register calls (for testing task leaks)
    register_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Counter for TTL refresh calls (for heartbeat lifecycle tests)
    refresh_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Counter for epoch-fenced unregister calls.
    unregister_if_epoch_matches_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Counter for list_active_streams calls (for periodic sync lifecycle tests)
    list_active_streams_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// Counter for list_active_publishers calls (for periodic sync lifecycle tests)
    list_active_publishers_call_count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// When true, `get_publisher` returns an error (simulates Redis failure)
    fail_get_publisher: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When true, `refresh_publisher_ttl` returns a Redis-like connectivity error.
    fail_refresh_publisher_ttl: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When true, `refresh_publisher_ttl` returns the wrapped timeout error shape
    /// used by the real Redis-backed registry helper.
    fail_refresh_publisher_ttl_with_wrapped_timeout: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When true, `refresh_publisher_ttl` returns a persistent non-I/O registry error.
    fail_refresh_publisher_ttl_with_response_error: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Number of upcoming epoch-fenced unregister calls that should fail.
    fail_unregister_if_epoch_matches_times: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl MockStreamRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self {
            publishers: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            epoch_counters: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            register_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            unregister_if_epoch_matches_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            list_active_streams_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            list_active_publishers_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            fail_get_publisher: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fail_refresh_publisher_ttl: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            fail_refresh_publisher_ttl_with_wrapped_timeout: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            fail_refresh_publisher_ttl_with_response_error: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            fail_unregister_if_epoch_matches_times: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
        }
    }

    #[must_use]
    pub fn with_publishers(
        publishers: std::collections::HashMap<(String, String), PublisherInfo>,
    ) -> Self {
        Self {
            publishers: std::sync::Arc::new(tokio::sync::Mutex::new(publishers)),
            epoch_counters: std::sync::Arc::new(tokio::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            register_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            refresh_call_count: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            unregister_if_epoch_matches_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            list_active_streams_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            list_active_publishers_call_count: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
            fail_get_publisher: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            fail_refresh_publisher_ttl: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
                false,
            )),
            fail_refresh_publisher_ttl_with_wrapped_timeout: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            fail_refresh_publisher_ttl_with_response_error: std::sync::Arc::new(
                std::sync::atomic::AtomicBool::new(false),
            ),
            fail_unregister_if_epoch_matches_times: std::sync::Arc::new(
                std::sync::atomic::AtomicUsize::new(0),
            ),
        }
    }

    /// Get the count of `register_publisher` calls (for testing task leaks)
    #[must_use]
    pub fn register_call_count(&self) -> usize {
        self.register_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the count of `refresh_publisher_ttl` calls.
    #[must_use]
    pub fn refresh_call_count(&self) -> usize {
        self.refresh_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the count of `unregister_publisher_if_epoch_matches` calls.
    #[must_use]
    pub fn unregister_if_epoch_matches_call_count(&self) -> usize {
        self.unregister_if_epoch_matches_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the count of `list_active_streams` calls.
    #[must_use]
    pub fn list_active_streams_call_count(&self) -> usize {
        self.list_active_streams_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Get the count of `list_active_publishers` calls.
    #[must_use]
    pub fn list_active_publishers_call_count(&self) -> usize {
        self.list_active_publishers_call_count
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Set whether `get_publisher` should fail (simulates Redis failure)
    pub fn set_fail_get_publisher(&self, fail: bool) {
        self.fail_get_publisher
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    /// Set whether `refresh_publisher_ttl` should fail with a Redis I/O error.
    pub fn set_fail_refresh_publisher_ttl(&self, fail: bool) {
        self.fail_refresh_publisher_ttl
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    /// Set whether `refresh_publisher_ttl` should fail with the wrapped timeout
    /// error shape returned by `with_redis_timeout(...)`.
    pub fn set_fail_refresh_publisher_ttl_with_wrapped_timeout(&self, fail: bool) {
        self.fail_refresh_publisher_ttl_with_wrapped_timeout
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    /// Set whether `refresh_publisher_ttl` should fail with a non-I/O Redis error.
    pub fn set_fail_refresh_publisher_ttl_with_response_error(&self, fail: bool) {
        self.fail_refresh_publisher_ttl_with_response_error
            .store(fail, std::sync::atomic::Ordering::SeqCst);
    }

    /// Fail the next `times` epoch-fenced unregister calls.
    pub fn set_fail_unregister_if_epoch_matches_times(&self, times: usize) {
        self.fail_unregister_if_epoch_matches_times
            .store(times, std::sync::atomic::Ordering::SeqCst);
    }
}

impl Default for MockStreamRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl StreamRegistryTrait for MockStreamRegistry {
    async fn register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        app_name: &str,
        api_address: &str,
    ) -> Result<bool> {
        // Increment call counter for testing
        self.register_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        validate_stream_ids(room_id, media_id)?;
        let mut publishers = self.publishers.lock().await;
        let mut epoch_counters = self.epoch_counters.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        if let std::collections::hash_map::Entry::Vacant(entry) = publishers.entry(key.clone()) {
            // Increment epoch counter
            let epoch = epoch_counters.entry(key).or_insert(0);
            *epoch += 1;

            entry.insert(PublisherInfo {
                node_id: node_id.to_string(),
                api_address: api_address.to_string(),
                app_name: app_name.to_string(),
                user_id: String::new(),
                started_at: Utc::now(),
                epoch: *epoch,
            });
            Ok(true)
        } else {
            Ok(false)
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
        validate_stream_ids(room_id, media_id)?;
        let mut publishers = self.publishers.lock().await;
        let mut epoch_counters = self.epoch_counters.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        if let std::collections::hash_map::Entry::Vacant(entry) = publishers.entry(key.clone()) {
            // Increment epoch counter
            let epoch = epoch_counters.entry(key).or_insert(0);
            *epoch += 1;

            entry.insert(PublisherInfo {
                node_id: node_id.to_string(),
                api_address: api_address.to_string(),
                app_name: "live".to_string(),
                user_id: user_id.to_string(),
                started_at: Utc::now(),
                epoch: *epoch,
            });
            Ok(true)
        } else {
            Ok(false)
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
        self.refresh_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .fail_refresh_publisher_ttl
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let redis_error = redis::RedisError::from((
                redis::ErrorKind::Io,
                "simulated Redis failure in refresh_publisher_ttl",
            ));
            return Err(anyhow::Error::new(redis_error));
        }
        if self
            .fail_refresh_publisher_ttl_with_wrapped_timeout
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(RedisOperationTimeout::new(5).into());
        }
        if self
            .fail_refresh_publisher_ttl_with_response_error
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let redis_error = redis::RedisError::from((
                redis::ErrorKind::Client,
                "simulated Redis client error in refresh_publisher_ttl",
            ));
            return Err(anyhow::Error::new(redis_error));
        }
        let publishers = self.publishers.lock().await;
        Ok(
            match publishers.get(&(room_id.to_string(), media_id.to_string())) {
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
        let mut publishers = self.publishers.lock().await;
        publishers.remove(&(room_id.to_string(), media_id.to_string()));
        Ok(())
    }

    async fn unregister_publisher_if_epoch_matches(
        &self,
        room_id: &str,
        media_id: &str,
        expected_epoch: u64,
    ) -> Result<()> {
        validate_stream_ids(room_id, media_id)?;
        self.unregister_if_epoch_matches_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let remaining_failures = self
            .fail_unregister_if_epoch_matches_times
            .load(std::sync::atomic::Ordering::SeqCst);
        if remaining_failures > 0 {
            self.fail_unregister_if_epoch_matches_times
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            let redis_error = redis::RedisError::from((
                redis::ErrorKind::Io,
                "simulated Redis failure in unregister_publisher_if_epoch_matches",
            ));
            return Err(anyhow::Error::new(redis_error));
        }
        let mut publishers = self.publishers.lock().await;
        let key = (room_id.to_string(), media_id.to_string());
        if publishers
            .get(&key)
            .is_some_and(|publisher| publisher.epoch == expected_epoch)
        {
            publishers.remove(&key);
        }
        Ok(())
    }

    async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>> {
        validate_stream_ids(room_id, media_id)?;
        if self
            .fail_get_publisher
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow::anyhow!("Simulated Redis failure in get_publisher"));
        }
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .get(&(room_id.to_string(), media_id.to_string()))
            .cloned())
    }

    async fn is_stream_active(&self, room_id: &str, media_id: &str) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
        let publishers = self.publishers.lock().await;
        Ok(publishers.contains_key(&(room_id.to_string(), media_id.to_string())))
    }

    async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>> {
        self.list_active_publishers_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self
            .fail_get_publisher
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            return Err(anyhow::anyhow!(
                "Simulated Redis failure in list_active_publishers"
            ));
        }
        let publishers = self.publishers.lock().await;
        Ok(publishers
            .iter()
            .map(|((room_id, media_id), publisher)| ActivePublisherEntry {
                room_id: room_id.clone(),
                media_id: media_id.clone(),
                publisher: publisher.clone(),
            })
            .collect())
    }

    async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
        self.list_active_streams_call_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let publishers = self.publishers.lock().await;
        Ok(publishers.keys().cloned().collect())
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

    async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()> {
        let mut publishers = self.publishers.lock().await;
        publishers.retain(|_, info| info.user_id != user_id);
        Ok(())
    }

    async fn validate_epoch(&self, room_id: &str, media_id: &str, epoch: u64) -> Result<bool> {
        validate_stream_ids(room_id, media_id)?;
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

impl MockStreamRegistry {
    /// Test helper: manually set epoch for a publisher (to simulate stale epoch scenarios)
    #[cfg(test)]
    pub async fn set_epoch(&self, room_id: &str, media_id: &str, epoch: u64) {
        let mut publishers = self.publishers.lock().await;
        if let Some(info) = publishers.get_mut(&(room_id.to_string(), media_id.to_string())) {
            info.epoch = epoch;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Unit tests using MockStreamRegistry (no Redis required)
    #[tokio::test]
    async fn test_mock_register_publisher_success() {
        let registry = MockStreamRegistry::new();

        // First registration should succeed
        let registered = registry
            .register_publisher("room123", "media456", "node1", "live", "localhost:50051")
            .await
            .unwrap();
        assert!(registered);

        // Verify publisher exists
        let publisher = registry.get_publisher("room123", "media456").await.unwrap();
        assert!(publisher.is_some());

        let pub_info = publisher.unwrap();
        assert_eq!(pub_info.node_id, "node1");
        assert_eq!(pub_info.app_name, "live");
    }

    #[tokio::test]
    async fn test_mock_register_publisher_duplicate() {
        let registry = MockStreamRegistry::new();

        // First registration should succeed
        let registered = registry
            .register_publisher("room123", "media456", "node1", "live", "localhost:50051")
            .await
            .unwrap();
        assert!(registered);

        // Second registration should fail (already exists)
        let registered = registry
            .register_publisher("room123", "media456", "node2", "live", "localhost:50052")
            .await
            .unwrap();
        assert!(!registered);
    }

    #[tokio::test]
    async fn test_mock_try_register_publisher() {
        let registry = MockStreamRegistry::new();

        // First try_register should succeed
        let result = registry
            .try_register_publisher("room123", "media456", "node1", "user1", "10.0.0.1:50051")
            .await
            .unwrap();
        assert!(result);

        // Second try_register should return false (already exists)
        let result = registry
            .try_register_publisher("room123", "media456", "node2", "user2", "10.0.0.2:50051")
            .await
            .unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_mock_registry_rejects_ambiguous_stream_ids() {
        let registry = MockStreamRegistry::new();

        let error = registry
            .try_register_publisher("room:1", "media", "node1", "user1", "10.0.0.1:50051")
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
    async fn test_mock_unregister_publisher() {
        let registry = MockStreamRegistry::new();

        // Register publisher
        registry
            .register_publisher("room123", "media456", "node1", "live", "localhost:50051")
            .await
            .unwrap();

        // Verify exists
        assert!(registry
            .is_stream_active("room123", "media456")
            .await
            .unwrap());

        // Unregister
        registry
            .unregister_publisher("room123", "media456")
            .await
            .unwrap();

        // Verify removed
        assert!(!registry
            .is_stream_active("room123", "media456")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_mock_get_publisher_not_found() {
        let registry = MockStreamRegistry::new();

        // Non-existent publisher should return None
        let result = registry
            .get_publisher("nonexistent", "media")
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_mock_list_active_streams() {
        let registry = MockStreamRegistry::new();

        // Register multiple publishers
        registry
            .register_publisher("room1", "media1", "node1", "live", "localhost:50051")
            .await
            .unwrap();
        registry
            .register_publisher("room2", "media2", "node1", "live", "localhost:50051")
            .await
            .unwrap();

        // List active streams
        let streams = registry.list_active_streams().await.unwrap();
        assert_eq!(streams.len(), 2);
        assert!(streams.contains(&(String::from("room1"), String::from("media1"))));
        assert!(streams.contains(&(String::from("room2"), String::from("media2"))));
    }

    #[tokio::test]
    async fn test_mock_pre_initialized() {
        let mut publishers = std::collections::HashMap::new();
        publishers.insert(
            ("room1".to_string(), "media1".to_string()),
            PublisherInfo {
                node_id: "node1".to_string(),
                api_address: String::new(),
                app_name: "live".to_string(),
                user_id: String::new(),
                started_at: Utc::now(),
                epoch: 1,
            },
        );

        let registry = MockStreamRegistry::with_publishers(publishers);

        // Should find the pre-registered publisher
        let result = registry.get_publisher("room1", "media1").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().node_id, "node1");
    }

    #[tokio::test]
    async fn test_mock_epoch_increments_on_register() {
        let registry = MockStreamRegistry::new();

        // First registration should have epoch 1
        registry
            .register_publisher("room1", "media1", "node1", "live", "localhost:50051")
            .await
            .unwrap();
        let info = registry
            .get_publisher("room1", "media1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(info.epoch, 1);

        // Unregister
        registry
            .unregister_publisher("room1", "media1")
            .await
            .unwrap();

        // Second registration should have epoch 2
        registry
            .register_publisher("room1", "media1", "node2", "live", "localhost:50052")
            .await
            .unwrap();
        let info = registry
            .get_publisher("room1", "media1")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(info.epoch, 2);
    }

    #[tokio::test]
    async fn test_mock_validate_epoch() {
        let registry = MockStreamRegistry::new();

        // Register publisher with epoch 1
        registry
            .register_publisher("room1", "media1", "node1", "live", "localhost:50051")
            .await
            .unwrap();
        let info = registry
            .get_publisher("room1", "media1")
            .await
            .unwrap()
            .unwrap();

        // Validate with correct epoch
        let valid = registry
            .validate_epoch("room1", "media1", info.epoch)
            .await
            .unwrap();
        assert!(valid);

        // Validate with incorrect epoch
        let valid = registry
            .validate_epoch("room1", "media1", 999)
            .await
            .unwrap();
        assert!(!valid);

        // Validate for non-existent stream
        let valid = registry
            .validate_epoch("nonexistent", "media", 1)
            .await
            .unwrap();
        assert!(!valid);
    }

    #[tokio::test]
    async fn test_mock_cleanup_all_publishers_for_node() {
        let registry = MockStreamRegistry::new();

        // Register publishers on different nodes
        registry
            .register_publisher("room1", "media1", "node1", "live", "localhost:50051")
            .await
            .unwrap();
        registry
            .register_publisher("room1", "media2", "node1", "live", "localhost:50051")
            .await
            .unwrap();
        registry
            .register_publisher("room2", "media1", "node2", "live", "localhost:50052")
            .await
            .unwrap();

        // Verify all exist
        assert!(registry.is_stream_active("room1", "media1").await.unwrap());
        assert!(registry.is_stream_active("room1", "media2").await.unwrap());
        assert!(registry.is_stream_active("room2", "media1").await.unwrap());

        // Cleanup node1
        registry
            .cleanup_all_publishers_for_node("node1")
            .await
            .unwrap();

        // Verify node1 publishers are removed, node2 remains
        assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
        assert!(!registry.is_stream_active("room1", "media2").await.unwrap());
        assert!(registry.is_stream_active("room2", "media1").await.unwrap());
    }
}
