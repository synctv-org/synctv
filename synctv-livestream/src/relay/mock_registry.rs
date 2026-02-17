// Mock StreamRegistry for testing without Redis

use async_trait::async_trait;
use anyhow::Result;
use chrono::Utc;
use super::registry::PublisherInfo;
use super::registry_trait::StreamRegistryTrait;

/// Mock StreamRegistry for testing without Redis
#[derive(Debug, Clone)]
pub struct MockStreamRegistry {
    publishers: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<(String, String), PublisherInfo>>>,
    /// Epoch counter for each stream (room_id, media_id)
    epoch_counters: std::sync::Arc<tokio::sync::Mutex<std::collections::HashMap<(String, String), u64>>>,
}

impl MockStreamRegistry {
    pub fn new() -> Self {
        Self {
            publishers: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            epoch_counters: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
    }

    pub fn with_publishers(publishers: std::collections::HashMap<(String, String), PublisherInfo>) -> Self {
        Self {
            publishers: std::sync::Arc::new(tokio::sync::Mutex::new(publishers)),
            epoch_counters: std::sync::Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
        }
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
        &mut self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        app_name: &str,
    ) -> Result<bool> {
        let mut publishers = self.publishers.lock().await;
        let mut epoch_counters = self.epoch_counters.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        if publishers.contains_key(&key) {
            Ok(false)
        } else {
            // Increment epoch counter
            let epoch = epoch_counters.entry(key.clone()).or_insert(0);
            *epoch += 1;

            publishers.insert(key, PublisherInfo {
                node_id: node_id.to_string(),
                grpc_address: String::new(),
                app_name: app_name.to_string(),
                user_id: String::new(),
                started_at: Utc::now(),
                epoch: *epoch,
            });
            Ok(true)
        }
    }

    async fn try_register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        user_id: &str,
        grpc_address: &str,
    ) -> Result<bool> {
        let mut publishers = self.publishers.lock().await;
        let mut epoch_counters = self.epoch_counters.lock().await;
        let key = (room_id.to_string(), media_id.to_string());

        if publishers.contains_key(&key) {
            Ok(false)
        } else {
            // Increment epoch counter
            let epoch = epoch_counters.entry(key.clone()).or_insert(0);
            *epoch += 1;

            publishers.insert(key, PublisherInfo {
                node_id: node_id.to_string(),
                grpc_address: grpc_address.to_string(),
                app_name: "live".to_string(),
                user_id: user_id.to_string(),
                started_at: Utc::now(),
                epoch: *epoch,
            });
            Ok(true)
        }
    }

    async fn refresh_publisher_ttl(&self, _room_id: &str, _media_id: &str, _user_id: &str) -> Result<()> {
        // No-op for mock
        Ok(())
    }

    async fn unregister_publisher(&self, room_id: &str, media_id: &str) -> Result<()> {
        let mut publishers = self.publishers.lock().await;
        publishers.remove(&(room_id.to_string(), media_id.to_string()));
        Ok(())
    }

    async fn get_publisher(&self, room_id: &str, media_id: &str) -> Result<Option<PublisherInfo>> {
        let publishers = self.publishers.lock().await;
        Ok(publishers.get(&(room_id.to_string(), media_id.to_string())).cloned())
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

#[cfg(test)]
mod tests {
    use super::*;

    // Unit tests using MockStreamRegistry (no Redis required)
    #[tokio::test]
    async fn test_mock_register_publisher_success() {
        let mut registry = MockStreamRegistry::new();

        // First registration should succeed
        let registered = registry
            .register_publisher("room123", "media456", "node1", "live")
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
        let mut registry = MockStreamRegistry::new();

        // First registration should succeed
        let registered = registry
            .register_publisher("room123", "media456", "node1", "live")
            .await
            .unwrap();
        assert!(registered);

        // Second registration should fail (already exists)
        let registered = registry
            .register_publisher("room123", "media456", "node2", "live")
            .await
            .unwrap();
        assert!(!registered);
    }

    #[tokio::test]
    async fn test_mock_try_register_publisher() {
        let registry = MockStreamRegistry::new();

        // First try_register should succeed
        let result = registry.try_register_publisher("room123", "media456", "node1", "user1", "10.0.0.1:50051").await.unwrap();
        assert!(result);

        // Second try_register should return false (already exists)
        let result = registry.try_register_publisher("room123", "media456", "node2", "user2", "10.0.0.2:50051").await.unwrap();
        assert!(!result);
    }

    #[tokio::test]
    async fn test_mock_unregister_publisher() {
        let mut registry = MockStreamRegistry::new();

        // Register publisher
        registry
            .register_publisher("room123", "media456", "node1", "live")
            .await
            .unwrap();

        // Verify exists
        assert!(registry.is_stream_active("room123", "media456").await.unwrap());

        // Unregister
        registry.unregister_publisher("room123", "media456").await.unwrap();

        // Verify removed
        assert!(!registry.is_stream_active("room123", "media456").await.unwrap());
    }

    #[tokio::test]
    async fn test_mock_get_publisher_not_found() {
        let registry = MockStreamRegistry::new();

        // Non-existent publisher should return None
        let result = registry.get_publisher("nonexistent", "media").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_mock_list_active_streams() {
        let mut registry = MockStreamRegistry::new();

        // Register multiple publishers
        registry
            .register_publisher("room1", "media1", "node1", "live")
            .await
            .unwrap();
        registry
            .register_publisher("room2", "media2", "node1", "live")
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
                grpc_address: String::new(),
                app_name: "live".to_string(),
                user_id: String::new(),
                started_at: Utc::now(),
                epoch: 1,
            }
        );

        let registry = MockStreamRegistry::with_publishers(publishers);

        // Should find the pre-registered publisher
        let result = registry.get_publisher("room1", "media1").await.unwrap();
        assert!(result.is_some());
        assert_eq!(result.unwrap().node_id, "node1");
    }

    #[tokio::test]
    async fn test_mock_epoch_increments_on_register() {
        let mut registry = MockStreamRegistry::new();

        // First registration should have epoch 1
        registry.register_publisher("room1", "media1", "node1", "live").await.unwrap();
        let info = registry.get_publisher("room1", "media1").await.unwrap().unwrap();
        assert_eq!(info.epoch, 1);

        // Unregister
        registry.unregister_publisher("room1", "media1").await.unwrap();

        // Second registration should have epoch 2
        registry.register_publisher("room1", "media1", "node2", "live").await.unwrap();
        let info = registry.get_publisher("room1", "media1").await.unwrap().unwrap();
        assert_eq!(info.epoch, 2);
    }

    #[tokio::test]
    async fn test_mock_validate_epoch() {
        let mut registry = MockStreamRegistry::new();

        // Register publisher with epoch 1
        registry.register_publisher("room1", "media1", "node1", "live").await.unwrap();
        let info = registry.get_publisher("room1", "media1").await.unwrap().unwrap();

        // Validate with correct epoch
        let valid = registry.validate_epoch("room1", "media1", info.epoch).await.unwrap();
        assert!(valid);

        // Validate with incorrect epoch
        let valid = registry.validate_epoch("room1", "media1", 999).await.unwrap();
        assert!(!valid);

        // Validate for non-existent stream
        let valid = registry.validate_epoch("nonexistent", "media", 1).await.unwrap();
        assert!(!valid);
    }

    #[tokio::test]
    async fn test_mock_cleanup_all_publishers_for_node() {
        let mut registry = MockStreamRegistry::new();

        // Register publishers on different nodes
        registry.register_publisher("room1", "media1", "node1", "live").await.unwrap();
        registry.register_publisher("room1", "media2", "node1", "live").await.unwrap();
        registry.register_publisher("room2", "media1", "node2", "live").await.unwrap();

        // Verify all exist
        assert!(registry.is_stream_active("room1", "media1").await.unwrap());
        assert!(registry.is_stream_active("room1", "media2").await.unwrap());
        assert!(registry.is_stream_active("room2", "media1").await.unwrap());

        // Cleanup node1
        registry.cleanup_all_publishers_for_node("node1").await.unwrap();

        // Verify node1 publishers are removed, node2 remains
        assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
        assert!(!registry.is_stream_active("room1", "media2").await.unwrap());
        assert!(registry.is_stream_active("room2", "media1").await.unwrap());
    }
}
