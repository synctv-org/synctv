use super::super::TestStreamRegistry;
use super::*;
use crate::relay::{ActivePublisherEntry, PublisherInfo};
use anyhow::Result;
use chrono::Utc;

/// Returns the manager and the corresponding receiver so tests can inspect
/// events sent on heartbeat failure.
fn test_manager(
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: &str,
) -> (
    Arc<PublisherManager>,
    synctv_xiu::streamhub::define::StreamHubEventReceiver,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    (
        Arc::new(PublisherManager::new(registry, node_id.to_string(), tx)),
        rx,
    )
}

#[tokio::test]
async fn test_publisher_manager_creation() {
    let registry = Arc::new(TestStreamRegistry::new());

    let (manager, _rx) = test_manager(registry, "test-node-1");
    assert_eq!(manager.local_node_id, "test-node-1");
    assert!(manager.active_publishers.is_empty());
}

#[tokio::test]
async fn test_active_publishers_map() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry, "test-node");

    assert!(manager.active_publishers.is_empty());
}

#[tokio::test]
async fn test_handle_publish_success() {
    let registry = Arc::new(TestStreamRegistry::new());
    // Pre-register publisher so handle_publish can look up the entry
    registry
        .try_register_publisher("room123", "media456", "test-node-1", "", "")
        .await
        .unwrap();
    let (manager, _rx) = test_manager(registry, "test-node-1");

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };

    // Handle publish event
    let result = manager.handle_publish(identifier).await;
    assert!(result.is_ok());

    // Verify publisher was tracked with composite key
    assert!(manager.active_publishers.contains_key("room123:media456"));
}

#[tokio::test]
async fn test_handle_unpublish_success() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node-1");

    // First, register a publisher
    registry
        .try_register_publisher("room123", "media456", "test-node-1", "user1", "addr1")
        .await
        .unwrap();
    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };
    let _ = manager.handle_publish(identifier.clone()).await;

    // Then unpublish
    let result = manager.handle_unpublish(identifier).await;
    assert!(result.is_ok());

    // Verify publisher was removed from tracking (composite key)
    assert!(!manager.active_publishers.contains_key("room123:media456"));
    assert!(
        registry
            .get_publisher("room123", "media456")
            .await
            .unwrap()
            .is_none(),
        "matching unpublish should remove registry entry"
    );
    assert_eq!(
        registry.unregister_if_epoch_matches_call_count(),
        1,
        "broadcast unpublish must use epoch-fenced unregister"
    );
}

#[tokio::test]
async fn test_handle_unpublish_does_not_delete_replacement_epoch() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node-1");

    registry
        .try_register_publisher("room-fast", "media-fast", "test-node-1", "user1", "addr1")
        .await
        .unwrap();
    let original = registry
        .get_publisher("room-fast", "media-fast")
        .await
        .unwrap()
        .expect("original publisher should exist");

    registry
        .unregister_publisher("room-fast", "media-fast")
        .await
        .unwrap();
    registry
        .try_register_publisher("room-fast", "media-fast", "test-node-1", "user2", "addr2")
        .await
        .unwrap();
    let replacement = registry
        .get_publisher("room-fast", "media-fast")
        .await
        .unwrap()
        .expect("replacement publisher should exist");

    manager.active_publishers.insert(
        "room-fast:media-fast".to_string(),
        Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            original.epoch,
        )),
    );

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room-fast".to_string(),
        stream_name: "media-fast".to_string(),
    };
    manager
        .handle_unpublish(identifier)
        .await
        .expect("stale unpublish should be handled");

    let current = registry
        .get_publisher("room-fast", "media-fast")
        .await
        .unwrap()
        .expect("replacement publisher must not be removed by stale unpublish");
    assert_eq!(current.epoch, replacement.epoch);
    assert_eq!(current.user_id, "user2");
    assert!(
        !manager
            .active_publishers
            .contains_key("room-fast:media-fast"),
        "stale local entry should still be removed"
    );
}

#[tokio::test]
async fn test_handle_publish_tracks_any_stream() {
    let registry = Arc::new(TestStreamRegistry::new());
    // Pre-register publisher so handle_publish can look up the entry
    registry
        .try_register_publisher("room123", "media456", "test-node-1", "", "")
        .await
        .unwrap();
    let (manager, _rx) = test_manager(registry, "test-node-1");

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };

    // PublisherManager just tracks publishers, doesn't validate format
    let result = manager.handle_publish(identifier).await;
    assert!(result.is_ok());

    // Verify tracking uses composite key
    assert!(manager.active_publishers.contains_key("room123:media456"));
}

/// Helper to insert a publisher entry into the `active_publishers` map.
fn insert_entry(manager: &PublisherManager, key: &str) {
    manager
        .active_publishers
        .insert(key.to_string(), Arc::new(PublisherEntry::new()));
}

async fn insert_registered_entry(
    manager: &PublisherManager,
    registry: &dyn StreamRegistryTrait,
    room_id: &str,
    media_id: &str,
) {
    let info = registry
        .get_publisher(room_id, media_id)
        .await
        .expect("registry lookup should succeed")
        .expect("publisher should exist in registry");
    manager.active_publishers.insert(
        publisher_key(room_id, media_id).expect("valid test stream id"),
        Arc::new(PublisherEntry::with_registration(info.user_id, info.epoch)),
    );
}

struct RecreateOnReregisterRegistry {
    publisher: tokio::sync::Mutex<Option<PublisherInfo>>,
    next_epoch: AtomicU64,
    expire_before_next_try_register: AtomicBool,
    replace_before_next_try_register: AtomicBool,
    expire_before_next_refresh: AtomicBool,
}

impl RecreateOnReregisterRegistry {
    fn new() -> Self {
        Self {
            publisher: tokio::sync::Mutex::new(Some(PublisherInfo {
                node_id: "test-node".to_string(),
                api_address: "addr1".to_string(),
                app_name: "live".to_string(),
                user_id: "user1".to_string(),
                started_at: Utc::now(),
                epoch: 1,
            })),
            next_epoch: AtomicU64::new(2),
            expire_before_next_try_register: AtomicBool::new(false),
            replace_before_next_try_register: AtomicBool::new(false),
            expire_before_next_refresh: AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl StreamRegistryTrait for RecreateOnReregisterRegistry {
    async fn register_publisher(
        &self,
        room_id: &str,
        media_id: &str,
        node_id: &str,
        _app_name: &str,
        api_address: &str,
    ) -> Result<bool> {
        self.try_register_publisher(room_id, media_id, node_id, "", api_address)
            .await
    }

    async fn try_register_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
        node_id: &str,
        user_id: &str,
        api_address: &str,
    ) -> Result<bool> {
        let mut publisher = self.publisher.lock().await;
        if self
            .expire_before_next_try_register
            .swap(false, Ordering::AcqRel)
        {
            publisher.take();
        }
        if self
            .replace_before_next_try_register
            .swap(false, Ordering::AcqRel)
        {
            let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
            *publisher = Some(PublisherInfo {
                node_id: node_id.to_string(),
                api_address: api_address.to_string(),
                app_name: "live".to_string(),
                user_id: user_id.to_string(),
                started_at: Utc::now(),
                epoch,
            });
            return Ok(false);
        }

        if publisher.is_some() {
            return Ok(false);
        }

        let epoch = self.next_epoch.fetch_add(1, Ordering::AcqRel);
        *publisher = Some(PublisherInfo {
            node_id: node_id.to_string(),
            api_address: api_address.to_string(),
            app_name: "live".to_string(),
            user_id: user_id.to_string(),
            started_at: Utc::now(),
            epoch,
        });
        Ok(true)
    }

    async fn refresh_publisher_ttl(
        &self,
        _room_id: &str,
        _media_id: &str,
        _user_id: &str,
        _node_id: &str,
        _expected_epoch: u64,
    ) -> Result<PublisherRefreshOutcome> {
        let mut publisher = self.publisher.lock().await;
        if self
            .expire_before_next_refresh
            .swap(false, Ordering::AcqRel)
        {
            publisher.take();
        }
        Ok(if publisher.is_some() {
            PublisherRefreshOutcome::Refreshed
        } else {
            PublisherRefreshOutcome::Missing
        })
    }

    async fn unregister_publisher(&self, _room_id: &str, _media_id: &str) -> Result<()> {
        let mut publisher = self.publisher.lock().await;
        publisher.take();
        Ok(())
    }

    async fn unregister_publisher_if_epoch_matches(
        &self,
        _room_id: &str,
        _media_id: &str,
        expected_epoch: u64,
    ) -> Result<()> {
        let mut publisher = self.publisher.lock().await;
        if publisher
            .as_ref()
            .is_some_and(|current| current.epoch == expected_epoch)
        {
            publisher.take();
        }
        Ok(())
    }

    async fn get_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
    ) -> Result<Option<PublisherInfo>> {
        let publisher = self.publisher.lock().await;
        if publisher.is_some() {
            self.expire_before_next_try_register
                .store(true, Ordering::Release);
        }
        Ok(publisher.clone())
    }

    async fn is_stream_active(&self, _room_id: &str, _media_id: &str) -> Result<bool> {
        Ok(self.publisher.lock().await.is_some())
    }

    async fn list_active_publishers(&self) -> Result<Vec<ActivePublisherEntry>> {
        Ok(self
            .publisher
            .lock()
            .await
            .clone()
            .into_iter()
            .map(|publisher| ActivePublisherEntry {
                room_id: "room-reregister".to_string(),
                media_id: "media-reregister".to_string(),
                publisher,
            })
            .collect())
    }

    async fn list_active_streams(&self) -> Result<Vec<(String, String)>> {
        Ok(if self.publisher.lock().await.is_some() {
            vec![(
                "room-reregister".to_string(),
                "media-reregister".to_string(),
            )]
        } else {
            Vec::new()
        })
    }

    async fn get_user_publishers(&self, user_id: &str) -> Result<Vec<(String, String)>> {
        let publisher = self.publisher.lock().await;
        Ok(
            if publisher
                .as_ref()
                .is_some_and(|current| current.user_id == user_id)
            {
                vec![(
                    "room-reregister".to_string(),
                    "media-reregister".to_string(),
                )]
            } else {
                Vec::new()
            },
        )
    }

    async fn unregister_all_user_publishers(&self, user_id: &str) -> Result<()> {
        let mut publisher = self.publisher.lock().await;
        if publisher
            .as_ref()
            .is_some_and(|current| current.user_id == user_id)
        {
            publisher.take();
        }
        Ok(())
    }

    async fn validate_epoch(&self, _room_id: &str, _media_id: &str, epoch: u64) -> Result<bool> {
        Ok(self
            .publisher
            .lock()
            .await
            .as_ref()
            .is_some_and(|current| current.epoch == epoch))
    }

    async fn cleanup_all_publishers_for_node(&self, node_id: &str) -> Result<()> {
        let mut publisher = self.publisher.lock().await;
        if publisher
            .as_ref()
            .is_some_and(|current| current.node_id == node_id)
        {
            publisher.take();
        }
        Ok(())
    }
}

#[tokio::test]
async fn test_reconcile_removes_stale_entries() {
    // Registry has room1:media1 on our node, but NOT room2:media2
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "test-node", "", "")
        .await
        .unwrap();

    let (manager, _rx) = test_manager(registry, "test-node");

    // Simulate local tracking of two publishers
    insert_entry(&manager, "room1:media1");
    insert_entry(&manager, "room2:media2");
    assert_eq!(manager.active_publishers.len(), 2);

    // Reconcile should remove room2:media2 (not in registry)
    manager.reconcile_with_registry().await;

    assert_eq!(manager.active_publishers.len(), 1);
    assert!(manager.active_publishers.contains_key("room1:media1"));
    assert!(!manager.active_publishers.contains_key("room2:media2"));
}

#[tokio::test]
async fn test_reconcile_removes_entries_moved_to_other_node() {
    // Registry has room1:media1 but on a DIFFERENT node
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "other-node", "", "")
        .await
        .unwrap();

    let (manager, _rx) = test_manager(registry, "test-node");

    // Local tracking thinks we own it
    insert_entry(&manager, "room1:media1");
    assert_eq!(manager.active_publishers.len(), 1);

    // Reconcile should remove it (owned by other-node)
    manager.reconcile_with_registry().await;

    assert!(manager.active_publishers.is_empty());
}

#[tokio::test]
async fn test_reconcile_keeps_valid_entries() {
    // Registry has both publishers on our node
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "test-node", "", "")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media2", "test-node", "", "")
        .await
        .unwrap();

    let (manager, _rx) = test_manager(registry, "test-node");

    insert_entry(&manager, "room1:media1");
    insert_entry(&manager, "room2:media2");

    manager.reconcile_with_registry().await;

    // Both should still be present
    assert_eq!(manager.active_publishers.len(), 2);
}

#[tokio::test]
async fn test_reconcile_with_empty_map() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry, "test-node");

    // Should not panic with empty active_publishers
    manager.reconcile_with_registry().await;
    assert!(manager.active_publishers.is_empty());
}

#[tokio::test]
async fn test_reregister_refreshes_local_epoch_after_registry_recreate() {
    let registry = Arc::new(RecreateOnReregisterRegistry::new());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx)
        .with_api_address("addr1".to_string());
    manager.active_publishers.insert(
        "room-reregister:media-reregister".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    manager.reregister_all_publishers().await;

    let active_entry = manager
        .active_publishers
        .get("room-reregister:media-reregister")
        .expect("publisher should still be tracked after re-registration");
    assert_eq!(
        active_entry.epoch, 2,
        "successful re-registration should refresh the locally tracked epoch"
    );
    drop(active_entry);

    manager
        .cleanup_publisher("room-reregister", "media-reregister", 2, "test cleanup")
        .await;

    assert!(
        registry
            .get_publisher("room-reregister", "media-reregister")
            .await
            .unwrap()
            .is_none(),
        "cleanup should remove the recreated registry entry using the refreshed epoch"
    );
}

#[tokio::test]
async fn test_reregister_refreshes_local_epoch_after_ttl_only_recovery() {
    let registry = Arc::new(RecreateOnReregisterRegistry::new());
    registry
        .replace_before_next_try_register
        .store(true, Ordering::Release);
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx)
        .with_api_address("addr1".to_string());
    manager.active_publishers.insert(
        "room-reregister:media-reregister".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    manager.reregister_all_publishers().await;

    let active_entry = manager
        .active_publishers
        .get("room-reregister:media-reregister")
        .expect("publisher should still be tracked after TTL-only re-registration");
    assert_eq!(
        active_entry.epoch, 2,
        "TTL-only recovery should still refresh the locally tracked epoch"
    );
    drop(active_entry);

    manager
        .cleanup_publisher("room-reregister", "media-reregister", 2, "test cleanup")
        .await;

    assert!(
        registry
            .get_publisher("room-reregister", "media-reregister")
            .await
            .unwrap()
            .is_none(),
        "cleanup should remove the live registry entry after TTL-only recovery"
    );
}

#[tokio::test]
async fn test_reregister_recreates_entry_when_ttl_refresh_reports_missing() {
    let registry = Arc::new(RecreateOnReregisterRegistry::new());
    registry
        .expire_before_next_refresh
        .store(true, Ordering::Release);
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx)
        .with_api_address("addr1".to_string());
    manager.active_publishers.insert(
        "room-reregister:media-reregister".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    manager.reregister_all_publishers().await;

    let recreated = registry
        .get_publisher("room-reregister", "media-reregister")
        .await
        .unwrap()
        .expect("restart recovery should recreate the missing registry entry");
    assert_eq!(recreated.node_id, "test-node");
    assert_eq!(recreated.epoch, 2);

    let active_entry = manager
        .active_publishers
        .get("room-reregister:media-reregister")
        .expect("publisher should still be tracked after recovery");
    assert_eq!(
        active_entry.epoch, 2,
        "recovery should refresh the tracked epoch after recreating a missing entry"
    );
}

#[tokio::test]
async fn test_lag_event_count() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry, "test-node");

    assert_eq!(manager.lag_event_count(), 0);
}

#[tokio::test]
async fn test_record_publisher_activity() {
    let registry = Arc::new(TestStreamRegistry::new());
    // Pre-register publisher so handle_publish can look up the entry
    registry
        .try_register_publisher("room1", "media1", "test-node", "", "")
        .await
        .unwrap();
    let (manager, _rx) = test_manager(registry, "test-node");

    // Insert publisher
    let identifier = StreamIdentifier::Rtmp {
        app_name: "room1".to_string(),
        stream_name: "media1".to_string(),
    };
    manager.handle_publish(identifier).await.unwrap();

    // Record activity and verify the entry was touched
    let before = manager
        .active_publishers
        .get("room1:media1")
        .unwrap()
        .idle_secs();
    assert!(before <= 1); // just created

    manager.record_publisher_activity("room1", "media1");

    let after = manager
        .active_publishers
        .get("room1:media1")
        .unwrap()
        .idle_secs();
    assert!(after <= 1); // just touched
}

/// Verify that handle_publish returns an error when Redis fails,
/// ensuring the stream is rejected (fail-closed behavior).
#[tokio::test]
async fn test_handle_publish_fails_closed_on_redis_failure() {
    let registry = Arc::new(TestStreamRegistry::new());
    // Pre-register publisher so it exists
    registry
        .try_register_publisher("room123", "media456", "test-node-1", "user1", "")
        .await
        .unwrap();

    // Enable Redis failure simulation
    registry.set_fail_get_publisher(true);

    let (manager, _rx) = test_manager(registry, "test-node-1");

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };

    // handle_publish should return Err when Redis fails
    let result = manager.handle_publish(identifier).await;
    assert!(
        result.is_err(),
        "Stream should be rejected on Redis failure"
    );

    // Verify publisher was NOT tracked (fail-closed: no untracked publisher)
    assert!(
        !manager.active_publishers.contains_key("room123:media456"),
        "Publisher should not be tracked when Redis fails"
    );
}

/// Verify that the broadcast event handler propagates the error from
/// handle_publish when Redis fails, so the stream hub can reject the connection.
#[tokio::test]
async fn test_broadcast_event_propagates_redis_failure() {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "")
        .await
        .unwrap();

    // Enable Redis failure simulation
    registry.set_fail_get_publisher(true);

    let (manager, _rx) = test_manager(registry, "test-node");

    let event = synctv_xiu::streamhub::define::BroadcastEvent::Publish {
        identifier: StreamIdentifier::Rtmp {
            app_name: "room1".to_string(),
            stream_name: "media1".to_string(),
        },
        pub_type: synctv_xiu::streamhub::define::PublishType::RtmpPush,
    };

    let result = manager.handle_broadcast_event(event).await;
    assert!(
        result.is_err(),
        "Broadcast event handler should propagate Redis failure"
    );
}

#[tokio::test]
async fn test_record_activity_nonexistent_publisher() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry, "test-node");

    // Should not panic when recording activity for a publisher that doesn't exist
    manager.record_publisher_activity("nonexistent", "publisher");
}

#[tokio::test]
async fn test_cleanup_publisher_waits_for_unpublish_backpressure() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let manager = PublisherManager::new(registry.clone(), "test-node".to_string(), tx);

    registry
        .try_register_publisher(
            "room-backpressure",
            "media-backpressure",
            "test-node",
            "user1",
            "",
        )
        .await
        .unwrap();
    manager.active_publishers.insert(
        "room-backpressure:media-backpressure".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    manager
        .hub_event_sender
        .try_send(StreamHubEvent::UnPublish {
            identifier: StreamIdentifier::Rtmp {
                app_name: "occupied".to_string(),
                stream_name: "occupied".to_string(),
            },
        })
        .expect("fill channel to create backpressure");

    let cleanup = manager.cleanup_publisher("room-backpressure", "media-backpressure", 1, "test");
    let delayed_recv = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = rx.recv().await;
        rx.recv().await
    };

    let ((), received) = tokio::join!(cleanup, delayed_recv);
    let Some(StreamHubEvent::UnPublish { identifier }) = received else {
        panic!("expected an UnPublish event after backpressure clears");
    };

    assert_eq!(
        identifier,
        StreamIdentifier::Rtmp {
            app_name: "room-backpressure".to_string(),
            stream_name: "media-backpressure".to_string(),
        }
    );
}

#[tokio::test]
async fn test_cleanup_publisher_uses_epoch_fenced_unregister() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, mut rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher("room-fence", "media-fence", "test-node", "user1", "addr1")
        .await
        .unwrap();
    let original = registry
        .get_publisher("room-fence", "media-fence")
        .await
        .unwrap()
        .unwrap();

    manager.active_publishers.insert(
        "room-fence:media-fence".to_string(),
        Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            original.epoch,
        )),
    );

    registry
        .unregister_publisher("room-fence", "media-fence")
        .await
        .unwrap();
    registry
        .try_register_publisher("room-fence", "media-fence", "other-node", "user2", "addr2")
        .await
        .unwrap();
    let replacement = registry
        .get_publisher("room-fence", "media-fence")
        .await
        .unwrap()
        .expect("replacement owner should exist");
    manager.active_publishers.insert(
        "room-fence:media-fence".to_string(),
        Arc::new(PublisherEntry::with_registration(
            "user2".to_string(),
            replacement.epoch,
        )),
    );

    manager
        .cleanup_publisher(
            "room-fence",
            "media-fence",
            original.epoch,
            "stale local owner",
        )
        .await;

    let current = registry
        .get_publisher("room-fence", "media-fence")
        .await
        .unwrap()
        .expect("new owner should still exist");
    assert_eq!(current.node_id, "other-node");
    assert_eq!(current.epoch, replacement.epoch);
    let active_entry = manager
        .active_publishers
        .get("room-fence:media-fence")
        .expect("replacement publisher should still be tracked");
    assert_eq!(active_entry.epoch, replacement.epoch);

    assert!(
        rx.try_recv().is_err(),
        "cleanup must not unpublish a replacement publisher"
    );
}

#[tokio::test(start_paused = true)]
async fn test_cleanup_publisher_retries_epoch_fenced_unregister() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, mut rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher("room-retry", "media-retry", "test-node", "user1", "addr1")
        .await
        .unwrap();
    let original = registry
        .get_publisher("room-retry", "media-retry")
        .await
        .unwrap()
        .unwrap();
    manager.active_publishers.insert(
        "room-retry:media-retry".to_string(),
        Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            original.epoch,
        )),
    );
    registry.set_fail_unregister_if_epoch_matches_times(2);

    manager
        .cleanup_publisher("room-retry", "media-retry", original.epoch, "retry cleanup")
        .await;

    assert!(
        !manager
            .active_publishers
            .contains_key("room-retry:media-retry"),
        "cleanup should remove the local publisher entry"
    );

    let event = rx.recv().await.expect("cleanup should emit unpublish");
    let StreamHubEvent::UnPublish { identifier } = event else {
        panic!("expected unpublish event");
    };
    assert_eq!(
        identifier,
        StreamIdentifier::Rtmp {
            app_name: "room-retry".to_string(),
            stream_name: "media-retry".to_string(),
        }
    );

    assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
    assert!(
        registry
            .get_publisher("room-retry", "media-retry")
            .await
            .unwrap()
            .is_none(),
        "cleanup should clear the stale registry entry after retrying"
    );
}

#[tokio::test(start_paused = true)]
async fn test_cleanup_publisher_survives_brief_extended_redis_outage() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, mut rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher(
            "room-recover",
            "media-recover",
            "test-node",
            "user1",
            "addr1",
        )
        .await
        .unwrap();
    let original = registry
        .get_publisher("room-recover", "media-recover")
        .await
        .unwrap()
        .unwrap();
    manager.active_publishers.insert(
        "room-recover:media-recover".to_string(),
        Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            original.epoch,
        )),
    );
    registry.set_fail_unregister_if_epoch_matches_times(2);

    manager
        .cleanup_publisher(
            "room-recover",
            "media-recover",
            original.epoch,
            "temporary redis outage",
        )
        .await;

    let event = rx.recv().await.expect("cleanup should emit unpublish");
    let StreamHubEvent::UnPublish { identifier } = event else {
        panic!("expected unpublish event");
    };
    assert_eq!(
        identifier,
        StreamIdentifier::Rtmp {
            app_name: "room-recover".to_string(),
            stream_name: "media-recover".to_string(),
        }
    );

    assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
    assert!(
        registry
            .get_publisher("room-recover", "media-recover")
            .await
            .unwrap()
            .is_none(),
        "cleanup should eventually clear the stale registry entry after Redis recovers"
    );
}

#[tokio::test(start_paused = true)]
async fn test_heartbeat_cleanup_runs_in_background_without_blocking_cycle() {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room-bg", "media-bg", "test-node", "user1", "addr1")
        .await
        .unwrap();
    let current = registry
        .get_publisher("room-bg", "media-bg")
        .await
        .unwrap()
        .unwrap();

    let (manager, mut rx) = test_manager(registry.clone(), "test-node");
    let entry = Arc::new(PublisherEntry::with_registration(
        "user1".to_string(),
        current.epoch,
    ));
    entry
        .consecutive_heartbeat_failures
        .store(MAX_CONSECUTIVE_HEARTBEAT_FAILURES - 1, Ordering::Release);
    manager
        .active_publishers
        .insert("room-bg:media-bg".to_string(), Arc::clone(&entry));

    registry.set_fail_refresh_publisher_ttl_with_response_error(true);
    registry.set_fail_unregister_if_epoch_matches_times(2);

    let heartbeat = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move {
            manager.run_heartbeat_cycle().await;
        }
    });
    let observe = async {
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(HEARTBEAT_RETRY_BASE_DELAY_MS + 1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(
            (HEARTBEAT_RETRY_BASE_DELAY_MS * 2) + 1,
        ))
        .await;
        tokio::task::yield_now().await;
        assert!(
            heartbeat.is_finished(),
            "heartbeat cycle should finish after the small heartbeat retry budget without waiting for cleanup retry backoff"
        );
        tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[0] + 1)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[1] + 1)).await;
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        let event = rx
            .recv()
            .await
            .expect("heartbeat cleanup should emit unpublish");
        let StreamHubEvent::UnPublish { identifier } = event else {
            panic!("expected unpublish event");
        };
        assert_eq!(
            identifier,
            StreamIdentifier::Rtmp {
                app_name: "room-bg".to_string(),
                stream_name: "media-bg".to_string(),
            }
        );
        assert!(
            !manager.active_publishers.contains_key("room-bg:media-bg"),
            "background cleanup should eventually remove local state"
        );
        assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
    };

    observe.await;
    heartbeat
        .await
        .expect("heartbeat task should complete successfully");
}

#[tokio::test(start_paused = true)]
async fn test_redis_unreachable_does_not_cleanup_active_publisher() {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room-redis", "media-redis", "test-node", "user1", "addr1")
        .await
        .unwrap();
    let current = registry
        .get_publisher("room-redis", "media-redis")
        .await
        .unwrap()
        .unwrap();

    let (manager, mut rx) = test_manager(registry.clone(), "test-node");
    manager.active_publishers.insert(
        "room-redis:media-redis".to_string(),
        Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            current.epoch,
        )),
    );

    registry.set_fail_refresh_publisher_ttl(true);

    for _ in 0..MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;
    }

    assert!(
        manager
            .active_publishers
            .contains_key("room-redis:media-redis"),
        "Redis connectivity failures must not clear active publishers"
    );
    assert!(
        rx.try_recv().is_err(),
        "Redis connectivity failures must not emit unpublish events"
    );
}

#[tokio::test(start_paused = true)]
async fn test_wrapped_redis_timeout_counts_as_unreachable_cycle() {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher(
            "room-timeout",
            "media-timeout",
            "test-node",
            "user1",
            "addr1",
        )
        .await
        .unwrap();
    let current = registry
        .get_publisher("room-timeout", "media-timeout")
        .await
        .unwrap()
        .unwrap();

    let (manager, mut rx) = test_manager(registry.clone(), "test-node");
    let entry = Arc::new(PublisherEntry::with_registration(
        "user1".to_string(),
        current.epoch,
    ));
    manager
        .active_publishers
        .insert("room-timeout:media-timeout".to_string(), Arc::clone(&entry));

    let timeout_error = RedisOperationTimeout::new(5).into();
    assert!(
        is_redis_unreachable_error(&timeout_error),
        "typed Redis timeout errors should classify as registry-unreachable"
    );

    registry.set_fail_refresh_publisher_ttl_with_wrapped_timeout(true);
    manager.run_heartbeat_cycle().await;
    tokio::task::yield_now().await;

    assert_eq!(
        entry.redis_unreachable_cycles.load(Ordering::Acquire),
        1,
        "wrapped Redis timeouts should increment redis_unreachable_cycles"
    );
    assert_eq!(
        entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
        0,
        "wrapped Redis timeouts must not count as publisher-missing failures"
    );
    assert!(
        manager
            .active_publishers
            .contains_key("room-timeout:media-timeout"),
        "last-resort cleanup must not trigger before the redis-unreachable threshold"
    );
    assert!(
        rx.try_recv().is_err(),
        "wrapped Redis timeouts must not emit unpublish before threshold"
    );
}

#[tokio::test(start_paused = true)]
async fn test_redis_outage_last_resort_cleanup_runs_in_background() {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room-outage", "media-outage", "test-node", "user1", "addr1")
        .await
        .unwrap();
    let current = registry
        .get_publisher("room-outage", "media-outage")
        .await
        .unwrap()
        .unwrap();

    let (manager, mut rx) = test_manager(registry.clone(), "test-node");
    let entry = Arc::new(PublisherEntry::with_registration(
        "user1".to_string(),
        current.epoch,
    ));
    manager
        .active_publishers
        .insert("room-outage:media-outage".to_string(), Arc::clone(&entry));

    registry.set_fail_refresh_publisher_ttl(true);
    registry.set_fail_unregister_if_epoch_matches_times(2);

    for _ in 1..MAX_CONSECUTIVE_REDIS_UNREACHABLE {
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;
    }

    let heartbeat = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move {
            manager.run_heartbeat_cycle().await;
        }
    });

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(HEARTBEAT_RETRY_BASE_DELAY_MS + 1)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(
        (HEARTBEAT_RETRY_BASE_DELAY_MS * 2) + 1,
    ))
    .await;
    tokio::task::yield_now().await;

    assert!(
        heartbeat.is_finished(),
        "redis-outage last-resort cleanup should not block the heartbeat loop on unregister retry backoff"
    );
    heartbeat
        .await
        .expect("heartbeat task should complete successfully");

    tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[0] + 1)).await;
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_millis(UNREGISTER_RETRY_DELAYS_MS[1] + 1)).await;
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    let event = rx
        .recv()
        .await
        .expect("background cleanup should eventually emit unpublish");
    let StreamHubEvent::UnPublish { identifier } = event else {
        panic!("expected unpublish event");
    };
    assert_eq!(
        identifier,
        StreamIdentifier::Rtmp {
            app_name: "room-outage".to_string(),
            stream_name: "media-outage".to_string(),
        }
    );
    assert!(
        !manager
            .active_publishers
            .contains_key("room-outage:media-outage"),
        "background cleanup should eventually remove local state after redis outage threshold"
    );
    assert_eq!(registry.unregister_if_epoch_matches_call_count(), 3);
}

#[tokio::test(start_paused = true)]
async fn test_persistent_non_io_registry_failures_still_trigger_cleanup() {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher(
            "room-response",
            "media-response",
            "test-node",
            "user1",
            "addr1",
        )
        .await
        .unwrap();
    let current = registry
        .get_publisher("room-response", "media-response")
        .await
        .unwrap()
        .unwrap();

    let (manager, mut rx) = test_manager(registry.clone(), "test-node");
    let entry = Arc::new(PublisherEntry::with_registration(
        "user1".to_string(),
        current.epoch,
    ));
    manager.active_publishers.insert(
        "room-response:media-response".to_string(),
        Arc::clone(&entry),
    );

    registry.set_fail_refresh_publisher_ttl_with_response_error(true);

    for expected_failures in 1..MAX_CONSECUTIVE_HEARTBEAT_FAILURES {
        manager.run_heartbeat_cycle().await;
        tokio::task::yield_now().await;

        assert_eq!(
            entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
            expected_failures,
            "persistent non-I/O registry failures should count toward cleanup threshold"
        );
        assert_eq!(
            entry.redis_unreachable_cycles.load(Ordering::Acquire),
            0,
            "persistent non-I/O registry failures must not increment redis_unreachable_cycles"
        );
        assert!(
            manager
                .active_publishers
                .contains_key("room-response:media-response"),
            "cleanup should wait until the heartbeat-failure threshold is reached"
        );
        assert!(
            rx.try_recv().is_err(),
            "cleanup must not emit unpublish before the threshold is reached"
        );
    }

    manager.run_heartbeat_cycle().await;
    tokio::task::yield_now().await;

    assert!(
        !manager
            .active_publishers
            .contains_key("room-response:media-response"),
        "persistent non-I/O registry failures should eventually trigger cleanup"
    );
    let event = rx
        .recv()
        .await
        .expect("cleanup should emit unpublish at threshold");
    let StreamHubEvent::UnPublish { identifier } = event else {
        panic!("expected unpublish event");
    };
    assert_eq!(
        identifier,
        StreamIdentifier::Rtmp {
            app_name: "room-response".to_string(),
            stream_name: "media-response".to_string(),
        }
    );
}

#[tokio::test(start_paused = true)]
async fn test_switching_between_failure_classes_resets_consecutive_counters() {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room-switch", "media-switch", "test-node", "user1", "addr1")
        .await
        .unwrap();
    let current = registry
        .get_publisher("room-switch", "media-switch")
        .await
        .unwrap()
        .unwrap();

    let (manager, mut rx) = test_manager(registry.clone(), "test-node");
    let entry = Arc::new(PublisherEntry::with_registration(
        "user1".to_string(),
        current.epoch,
    ));
    manager
        .active_publishers
        .insert("room-switch:media-switch".to_string(), Arc::clone(&entry));

    registry
        .unregister_publisher("room-switch", "media-switch")
        .await
        .unwrap();
    manager.run_heartbeat_cycle().await;
    tokio::task::yield_now().await;
    manager.run_heartbeat_cycle().await;
    tokio::task::yield_now().await;

    assert_eq!(
        entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
        2,
        "publisher-missing failures should count consecutively"
    );

    registry.set_fail_refresh_publisher_ttl(true);
    manager.run_heartbeat_cycle().await;
    tokio::task::yield_now().await;

    assert_eq!(
        entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
        0,
        "redis-unreachable failure must reset publisher-missing streak"
    );
    assert_eq!(
        entry.redis_unreachable_cycles.load(Ordering::Acquire),
        1,
        "redis-unreachable failure should start its own consecutive streak"
    );

    registry.set_fail_refresh_publisher_ttl(false);
    manager.run_heartbeat_cycle().await;
    tokio::task::yield_now().await;

    assert_eq!(
        entry.consecutive_heartbeat_failures.load(Ordering::Acquire),
        1,
        "publisher-missing streak should restart after a different failure class"
    );
    assert_eq!(
        entry.redis_unreachable_cycles.load(Ordering::Acquire),
        0,
        "publisher-missing failure must reset redis-unreachable streak"
    );
    assert!(
        manager
            .active_publishers
            .contains_key("room-switch:media-switch"),
        "mixed failure classes must not trigger premature cleanup"
    );
    assert!(
        rx.try_recv().is_err(),
        "mixed failure classes must not emit unpublish"
    );
}

#[tokio::test(start_paused = true)]
async fn test_start_stops_heartbeat_and_sync_when_broadcast_channel_closes() {
    let registry = Arc::new(TestStreamRegistry::with_publishers(
        std::collections::HashMap::from([(
            ("room1".to_string(), "media1".to_string()),
            PublisherInfo {
                node_id: "test-node".to_string(),
                api_address: "127.0.0.1:50051".to_string(),
                app_name: "live".to_string(),
                user_id: "user1".to_string(),
                started_at: Utc::now(),
                epoch: 1,
            },
        )]),
    ));
    let (hub_tx, _hub_rx) = tokio::sync::mpsc::channel(16);
    let manager = Arc::new(PublisherManager::new(
        registry.clone(),
        "test-node".to_string(),
        hub_tx,
    ));

    manager.active_publishers.insert(
        "room1:media1".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel(16);
    let start_handle = tokio::spawn(Arc::clone(&manager).start(broadcast_rx));

    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(HEARTBEAT_INTERVAL_SECS + 1)).await;
    tokio::task::yield_now().await;

    let refresh_before_shutdown = registry.refresh_call_count();
    assert!(
        refresh_before_shutdown >= 1,
        "heartbeat loop should refresh tracked publishers while manager is running"
    );

    drop(broadcast_tx);
    start_handle.await.unwrap();

    let sync_before_shutdown = registry.list_active_publishers_call_count();

    tokio::time::advance(Duration::from_secs(
        PERIODIC_SYNC_INTERVAL_SECS + HEARTBEAT_INTERVAL_SECS + 5,
    ))
    .await;
    tokio::task::yield_now().await;

    assert_eq!(
        registry.refresh_call_count(),
        refresh_before_shutdown,
        "heartbeat task must stop when publisher manager exits"
    );
    assert_eq!(
        registry.list_active_publishers_call_count(),
        sync_before_shutdown,
        "periodic sync task must stop when publisher manager exits"
    );
}

// Memory leak tests: reregister_all_publishers should clean up zombie entries

/// Test that `reregister_all_publishers` cleans up stale entries from local `DashMap`
/// when the registry entry no longer exists.
///
/// Scenario:
/// 1. Publisher is tracked locally (entry in `DashMap`)
/// 2. Registry entry expires or is removed (e.g., TTL, external cleanup)
/// 3. `UnPublish` event is lost - `handle_unpublish` never called
/// 4. `reregister_all_publishers` is called
///
/// Expected: The stale entry should be removed from `DashMap`.
#[tokio::test]
async fn test_reregister_removes_stale_entry_when_registry_entry_gone() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    // 1. Register a publisher
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();

    // 2. Track it locally (simulating Publish event)
    insert_entry(&manager, "room1:media1");

    // 3. Verify the entry is in DashMap
    assert_eq!(manager.active_publishers.len(), 1);

    // 4. Simulate registry entry being removed (TTL expiry, external cleanup)
    // but UnPublish event is lost
    registry
        .unregister_publisher("room1", "media1")
        .await
        .unwrap();

    // 5. Call reregister_all_publishers - this should remove the stale entry
    manager.reregister_all_publishers().await;

    // 6. Verify the stale entry is removed
    assert!(
        manager.active_publishers.is_empty(),
        "Stale entry should be removed from DashMap after reregister"
    );
}

/// Test that `reregister_all_publishers` removes local entry when
/// the publisher is now owned by another node.
#[tokio::test]
async fn test_reregister_removes_entry_taken_over_by_other_node() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    // 1. Register a publisher
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();

    // 2. Track it locally
    insert_entry(&manager, "room1:media1");

    // 3. Verify tracking
    assert_eq!(manager.active_publishers.len(), 1);

    // 4. Simulate takeover by another node (ownership change)
    registry
        .unregister_publisher("room1", "media1")
        .await
        .unwrap();
    registry
        .try_register_publisher("room1", "media1", "other-node", "user1", "other:50051")
        .await
        .unwrap();

    // 5. reregister should remove our local entry since we no longer own it
    manager.reregister_all_publishers().await;

    // 6. Local tracking should be empty
    assert!(
        manager.active_publishers.is_empty(),
        "Entry should be removed since other node took over"
    );
}

/// Test that `reregister_all_publishers` keeps entries that are still
/// owned by this node in the registry.
#[tokio::test]
async fn test_reregister_keeps_entries_owned_by_this_node() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    // 1. Register a publisher
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();

    // 2. Track it locally
    insert_registered_entry(&manager, registry.as_ref(), "room1", "media1").await;

    // 3. Verify tracking
    assert_eq!(manager.active_publishers.len(), 1);

    // 4. reregister should keep this entry since we still own it
    manager.reregister_all_publishers().await;

    // 5. Entry should still be tracked
    assert_eq!(
        manager.active_publishers.len(),
        1,
        "Entry should still be tracked since we own it"
    );
}

/// Test that reregister correctly handles a mix of:
/// - Publishers still owned by this node (keep)
/// - Publishers taken over by other nodes (remove)
/// - Publishers no longer in registry (remove)
#[tokio::test]
async fn test_reregister_partial_cleanup() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    // 1. Register three publishers
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room2", "media2", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();
    registry
        .try_register_publisher("room3", "media3", "test-node", "user1", "localhost:50051")
        .await
        .unwrap();

    // 2. Track all three locally
    insert_registered_entry(&manager, registry.as_ref(), "room1", "media1").await;
    insert_registered_entry(&manager, registry.as_ref(), "room2", "media2").await;
    insert_registered_entry(&manager, registry.as_ref(), "room3", "media3").await;

    // 3. Verify all tracked
    assert_eq!(manager.active_publishers.len(), 3);

    // 4. Remove room2 from registry (TTL expired)
    registry
        .unregister_publisher("room2", "media2")
        .await
        .unwrap();

    // 5. Transfer room3 to another node
    registry
        .unregister_publisher("room3", "media3")
        .await
        .unwrap();
    registry
        .try_register_publisher("room3", "media3", "other-node", "user1", "other:50051")
        .await
        .unwrap();

    // 6. reregister should:
    // - Keep room1 (we still own it)
    // - Remove room2 (not in registry)
    // - Remove room3 (owned by other node)
    manager.reregister_all_publishers().await;

    // 7. Only room1 should remain
    assert_eq!(
        manager.active_publishers.len(),
        1,
        "Only room1 should remain"
    );
    assert!(
        manager.active_publishers.contains_key("room1:media1"),
        "room1:media1 should still be tracked"
    );
}

/// This test simulates the exact scenario that causes the memory leak:
/// 1. Multiple publishers are tracked locally
/// 2. All registry entries expire or are removed
/// 3. `UnPublish` events are lost (e.g., broadcast channel lag)
/// 4. Without the fix, entries would remain in `DashMap` forever
/// 5. With the fix, `reregister_all_publishers` cleans them up
#[tokio::test]
async fn test_memory_leak_regression_zombie_cleanup() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    // 1. Create 10 publishers
    for i in 0..10 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        registry
            .try_register_publisher(&room, &media, "test-node", "user1", "localhost:50051")
            .await
            .unwrap();
        insert_entry(&manager, &format!("{room}:{media}"));
    }

    // 2. Verify all 10 are tracked
    assert_eq!(manager.active_publishers.len(), 10);

    // 3. Remove all from registry (simulating mass TTL expiry)
    for i in 0..10 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        registry.unregister_publisher(&room, &media).await.unwrap();
    }

    // 4. reregister should clean up all zombie entries
    manager.reregister_all_publishers().await;

    // 5. Verify DashMap is empty (no memory leak)
    assert!(
        manager.active_publishers.is_empty(),
        "All zombie entries should be cleaned up - no memory leak"
    );
}
