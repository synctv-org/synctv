use super::super::TestStreamRegistry;
use super::*;
use crate::relay::{ActivePublisherEntry, PublisherInfo};
use anyhow::Result;

type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
    anyhow::anyhow!(message.into()).into()
}

fn require_publisher(
    publisher: Option<PublisherInfo>,
    message: &'static str,
) -> std::result::Result<PublisherInfo, Box<dyn std::error::Error + Send + Sync>> {
    publisher.ok_or_else(|| test_error(message))
}

fn test_manager(
    registry: Arc<dyn StreamRegistryTrait>,
    node_id: &str,
) -> (
    Arc<PublisherManager>,
    synctv_xiu::streamhub::define::StreamHubEventReceiver,
) {
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    (
        Arc::new(PublisherManager::with_restarting_flag(
            registry,
            node_id.to_string(),
            tx,
            Arc::new(AtomicBool::new(false)),
        )),
        rx,
    )
}

#[tokio::test]
async fn test_active_publishers_map() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry, "test-node");

    assert!(manager.active_publishers.is_empty());
}

#[tokio::test]
async fn test_handle_publish_success() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room123", "media456", "test-node-1", "", "")
        .await?;
    let (manager, _rx) = test_manager(registry, "test-node-1");

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };

    let result = manager.handle_publish(identifier).await;
    assert!(result.is_ok());

    assert!(manager.active_publishers.contains_key("room123:media456"));
    Ok(())
}

#[tokio::test]
async fn test_handle_unpublish_success() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node-1");

    registry
        .try_register_publisher("room123", "media456", "test-node-1", "user1", "addr1")
        .await?;
    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };
    manager.handle_publish(identifier.clone()).await?;

    let result = manager.handle_unpublish(identifier).await;
    assert!(result.is_ok());

    assert!(!manager.active_publishers.contains_key("room123:media456"));
    assert!(
        registry
            .get_publisher("room123", "media456")
            .await?
            .is_none(),
        "matching unpublish should remove registry entry"
    );
    assert_eq!(
        registry.unregister_if_epoch_matches_call_count(),
        1,
        "broadcast unpublish must use epoch-fenced unregister"
    );
    Ok(())
}

#[tokio::test]
async fn test_handle_unpublish_does_not_delete_replacement_epoch() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node-1");

    registry
        .try_register_publisher("room-fast", "media-fast", "test-node-1", "user1", "addr1")
        .await?;
    let original = require_publisher(
        registry.get_publisher("room-fast", "media-fast").await?,
        "original publisher should exist",
    )?;

    registry
        .unregister_publisher("room-fast", "media-fast")
        .await?;
    registry
        .try_register_publisher("room-fast", "media-fast", "test-node-1", "user2", "addr2")
        .await?;
    let replacement = require_publisher(
        registry.get_publisher("room-fast", "media-fast").await?,
        "replacement publisher should exist",
    )?;

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
    manager.handle_unpublish(identifier).await?;

    let current = require_publisher(
        registry.get_publisher("room-fast", "media-fast").await?,
        "replacement publisher must not be removed by stale unpublish",
    )?;
    assert_eq!(current.epoch, replacement.epoch);
    assert_eq!(current.user_id, "user2");
    assert!(
        !manager
            .active_publishers
            .contains_key("room-fast:media-fast"),
        "stale local entry should still be removed"
    );
    Ok(())
}

#[tokio::test]
async fn test_handle_publish_tracks_any_stream() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room123", "media456", "test-node-1", "", "")
        .await?;
    let (manager, _rx) = test_manager(registry, "test-node-1");

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };

    let result = manager.handle_publish(identifier).await;
    assert!(result.is_ok());

    assert!(manager.active_publishers.contains_key("room123:media456"));
    Ok(())
}

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
) -> TestResult {
    let info = require_publisher(
        registry.get_publisher(room_id, media_id).await?,
        "test publisher should exist in registry",
    )?;
    manager.active_publishers.insert(
        publisher_key(room_id, media_id)?,
        Arc::new(PublisherEntry::with_registration(info.user_id, info.epoch)),
    );
    Ok(())
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
                started_at: synctv_core::SystemClock.now(),
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
                started_at: synctv_core::SystemClock.now(),
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
            started_at: synctv_core::SystemClock.now(),
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

    async fn list_streams_for_room(&self, room_id: &str) -> Result<Vec<String>> {
        crate::util::validate_stream_id_component(room_id, "room_id")?;
        let publisher = self.publisher.lock().await;
        Ok(if publisher.is_some() && room_id == "room-reregister" {
            vec!["media-reregister".to_string()]
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

    async fn get_user_publishers_for_room(
        &self,
        room_id: &str,
        user_id: &str,
    ) -> Result<Vec<(String, String)>> {
        crate::util::validate_stream_id_component(room_id, "room_id")?;
        Ok(self
            .get_user_publishers(user_id)
            .await?
            .into_iter()
            .filter(|(publisher_room_id, _)| publisher_room_id == room_id)
            .collect())
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
async fn test_reconcile_removes_stale_entries() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "test-node", "", "")
        .await?;

    let (manager, _rx) = test_manager(registry, "test-node");

    insert_entry(&manager, "room1:media1");
    insert_entry(&manager, "room2:media2");
    assert_eq!(manager.active_publishers.len(), 2);

    manager.reconcile_with_registry().await;

    assert_eq!(manager.active_publishers.len(), 1);
    assert!(manager.active_publishers.contains_key("room1:media1"));
    assert!(!manager.active_publishers.contains_key("room2:media2"));
    Ok(())
}

#[tokio::test]
async fn test_reconcile_removes_entries_moved_to_other_node() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "other-node", "", "")
        .await?;

    let (manager, _rx) = test_manager(registry, "test-node");

    insert_entry(&manager, "room1:media1");
    assert_eq!(manager.active_publishers.len(), 1);

    manager.reconcile_with_registry().await;

    assert!(manager.active_publishers.is_empty());
    Ok(())
}

#[tokio::test]
async fn test_reconcile_keeps_valid_entries() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "test-node", "", "")
        .await?;
    registry
        .try_register_publisher("room2", "media2", "test-node", "", "")
        .await?;

    let (manager, _rx) = test_manager(registry, "test-node");

    insert_entry(&manager, "room1:media1");
    insert_entry(&manager, "room2:media2");

    manager.reconcile_with_registry().await;

    assert_eq!(manager.active_publishers.len(), 2);
    Ok(())
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
async fn test_reregister_refreshes_local_epoch_after_registry_recreate() -> TestResult {
    let registry = Arc::new(RecreateOnReregisterRegistry::new());
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager = PublisherManager::with_restarting_flag(
        registry.clone(),
        "test-node".to_string(),
        tx,
        Arc::new(AtomicBool::new(false)),
    )
    .with_api_address("addr1".to_string());
    manager.active_publishers.insert(
        "room-reregister:media-reregister".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    manager.reregister_all_publishers_once().await;

    let active_entry = manager
        .active_publishers
        .get("room-reregister:media-reregister")
        .ok_or_else(|| test_error("publisher should still be tracked after re-registration"))?;
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
            .await?
            .is_none(),
        "cleanup should remove the recreated registry entry using the refreshed epoch"
    );
    Ok(())
}

#[tokio::test]
async fn test_reregister_refreshes_local_epoch_after_ttl_only_recovery() -> TestResult {
    let registry = Arc::new(RecreateOnReregisterRegistry::new());
    registry
        .replace_before_next_try_register
        .store(true, Ordering::Release);
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager = PublisherManager::with_restarting_flag(
        registry.clone(),
        "test-node".to_string(),
        tx,
        Arc::new(AtomicBool::new(false)),
    )
    .with_api_address("addr1".to_string());
    manager.active_publishers.insert(
        "room-reregister:media-reregister".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    manager.reregister_all_publishers_once().await;

    let active_entry = manager
        .active_publishers
        .get("room-reregister:media-reregister")
        .ok_or_else(|| {
            test_error("publisher should still be tracked after TTL-only re-registration")
        })?;
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
            .await?
            .is_none(),
        "cleanup should remove the live registry entry after TTL-only recovery"
    );
    Ok(())
}

#[tokio::test]
async fn test_reregister_recreates_entry_when_ttl_refresh_reports_missing() -> TestResult {
    let registry = Arc::new(RecreateOnReregisterRegistry::new());
    registry
        .expire_before_next_refresh
        .store(true, Ordering::Release);
    let (tx, _rx) = tokio::sync::mpsc::channel(64);
    let manager = PublisherManager::with_restarting_flag(
        registry.clone(),
        "test-node".to_string(),
        tx,
        Arc::new(AtomicBool::new(false)),
    )
    .with_api_address("addr1".to_string());
    manager.active_publishers.insert(
        "room-reregister:media-reregister".to_string(),
        Arc::new(PublisherEntry::with_registration("user1".to_string(), 1)),
    );

    manager.reregister_all_publishers_once().await;

    let recreated = require_publisher(
        registry
            .get_publisher("room-reregister", "media-reregister")
            .await?,
        "restart recovery should recreate the missing registry entry",
    )?;
    assert_eq!(recreated.node_id, "test-node");
    assert_eq!(recreated.epoch, 2);

    let active_entry = manager
        .active_publishers
        .get("room-reregister:media-reregister")
        .ok_or_else(|| test_error("publisher should still be tracked after recovery"))?;
    assert_eq!(
        active_entry.epoch, 2,
        "recovery should refresh the tracked epoch after recreating a missing entry"
    );
    Ok(())
}

#[tokio::test]
async fn test_lag_event_count_starts_at_zero() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry, "test-node");

    assert_eq!(manager.lag_event_count.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn test_record_publisher_activity() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "test-node", "", "")
        .await?;
    let (manager, _rx) = test_manager(registry, "test-node");

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room1".to_string(),
        stream_name: "media1".to_string(),
    };
    manager.handle_publish(identifier).await?;

    let before = manager
        .active_publishers
        .get("room1:media1")
        .ok_or_else(|| test_error("publisher should be tracked"))?
        .idle_secs();
    assert!(before <= 1);

    manager.record_publisher_activity("room1", "media1");

    let after = manager
        .active_publishers
        .get("room1:media1")
        .ok_or_else(|| test_error("publisher should be tracked"))?
        .idle_secs();
    assert!(after <= 1);
    Ok(())
}

#[tokio::test]
async fn test_handle_publish_fails_closed_on_redis_failure() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room123", "media456", "test-node-1", "user1", "")
        .await?;

    registry.set_fail_get_publisher(true);

    let (manager, _rx) = test_manager(registry, "test-node-1");

    let identifier = StreamIdentifier::Rtmp {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
    };

    let result = manager.handle_publish(identifier).await;
    assert!(
        result.is_err(),
        "Stream should be rejected on Redis failure"
    );

    assert!(
        !manager.active_publishers.contains_key("room123:media456"),
        "Publisher should not be tracked when Redis fails"
    );
    Ok(())
}

#[tokio::test]
async fn test_broadcast_event_propagates_redis_failure() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "")
        .await?;

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
    Ok(())
}

#[tokio::test]
async fn test_broadcast_event_ignores_external_pull_for_heartbeat_tracking() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry.set_fail_get_publisher(true);

    let (manager, _rx) = test_manager(registry, "test-node");

    let event = synctv_xiu::streamhub::define::BroadcastEvent::Publish {
        identifier: StreamIdentifier::Rtmp {
            app_name: "room1".to_string(),
            stream_name: "media1".to_string(),
        },
        pub_type: synctv_xiu::streamhub::define::PublishType::ExternalPull,
    };

    manager.handle_broadcast_event(event).await?;
    assert!(
        manager.active_publishers.is_empty(),
        "External pull lifecycle is owned by ExternalPublishManager"
    );
    Ok(())
}

#[tokio::test]
async fn test_record_activity_nonexistent_publisher() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry, "test-node");

    // Should not panic when recording activity for a publisher that doesn't exist
    manager.record_publisher_activity("nonexistent", "publisher");
}

#[tokio::test]
async fn test_cleanup_publisher_waits_for_unpublish_backpressure() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (tx, mut rx) = tokio::sync::mpsc::channel(1);
    let manager = PublisherManager::with_restarting_flag(
        registry.clone(),
        "test-node".to_string(),
        tx,
        Arc::new(AtomicBool::new(false)),
    );

    registry
        .try_register_publisher(
            "room-backpressure",
            "media-backpressure",
            "test-node",
            "user1",
            "",
        )
        .await?;
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
        .map_err(|error| test_error(error.to_string()))?;

    let cleanup = manager.cleanup_publisher("room-backpressure", "media-backpressure", 1, "test");
    let delayed_recv = async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            rx.recv().await.is_some(),
            "backpressure test should receive queued event"
        );
        rx.recv().await
    };

    let ((), received) = tokio::join!(cleanup, delayed_recv);
    let Some(StreamHubEvent::UnPublish { identifier }) = received else {
        return Err(test_error(
            "expected an UnPublish event after backpressure clears",
        ));
    };

    assert_eq!(
        identifier,
        StreamIdentifier::Rtmp {
            app_name: "room-backpressure".to_string(),
            stream_name: "media-backpressure".to_string(),
        }
    );
    Ok(())
}

#[tokio::test]
async fn test_cleanup_publisher_uses_epoch_fenced_unregister() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, mut rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher("room-fence", "media-fence", "test-node", "user1", "addr1")
        .await?;
    let original = require_publisher(
        registry.get_publisher("room-fence", "media-fence").await?,
        "original publisher should exist",
    )?;

    manager.active_publishers.insert(
        "room-fence:media-fence".to_string(),
        Arc::new(PublisherEntry::with_registration(
            "user1".to_string(),
            original.epoch,
        )),
    );

    registry
        .unregister_publisher("room-fence", "media-fence")
        .await?;
    registry
        .try_register_publisher("room-fence", "media-fence", "other-node", "user2", "addr2")
        .await?;
    let replacement = require_publisher(
        registry.get_publisher("room-fence", "media-fence").await?,
        "replacement owner should exist",
    )?;
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

    let current = require_publisher(
        registry.get_publisher("room-fence", "media-fence").await?,
        "new owner should still exist",
    )?;
    assert_eq!(current.node_id, "other-node");
    assert_eq!(current.epoch, replacement.epoch);
    let active_entry = manager
        .active_publishers
        .get("room-fence:media-fence")
        .ok_or_else(|| test_error("replacement publisher should still be tracked"))?;
    assert_eq!(active_entry.epoch, replacement.epoch);

    assert!(
        rx.try_recv().is_err(),
        "cleanup must not unpublish a replacement publisher"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn test_persistent_registry_failures_trigger_cleanup() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry
        .try_register_publisher(
            "room-response",
            "media-response",
            "test-node",
            "user1",
            "addr1",
        )
        .await?;
    let current = require_publisher(
        registry
            .get_publisher("room-response", "media-response")
            .await?,
        "current publisher should exist",
    )?;

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
            "registry failures should count toward cleanup threshold"
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
        "persistent registry failures should eventually trigger cleanup"
    );
    let event = rx
        .recv()
        .await
        .ok_or_else(|| test_error("cleanup should emit unpublish at threshold"))?;
    let StreamHubEvent::UnPublish { identifier } = event else {
        return Err(test_error("expected unpublish event"));
    };
    assert_eq!(
        identifier,
        StreamIdentifier::Rtmp {
            app_name: "room-response".to_string(),
            stream_name: "media-response".to_string(),
        }
    );
    Ok(())
}

#[tokio::test]
async fn test_heartbeat_refreshes_publishers_in_bounded_batches() {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");
    let publisher_count = PUBLISHER_REFRESH_BATCH_SIZE + 1;

    for index in 0..publisher_count {
        insert_entry(&manager, &format!("room{index}:media{index}"));
    }

    manager.run_heartbeat_cycle().await;

    assert_eq!(
        registry.refresh_batch_sizes(),
        vec![PUBLISHER_REFRESH_BATCH_SIZE, 1]
    );
    assert_eq!(registry.refresh_call_count(), publisher_count);
    assert_eq!(manager.active_publishers.len(), publisher_count);
}

#[tokio::test]
async fn test_registry_sync_requests_are_coalesced() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");
    for _ in 0..100 {
        manager.schedule_registry_sync();
    }

    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel(16);
    let start_handle = tokio::spawn(Arc::clone(&manager).start(broadcast_rx));
    tokio::time::timeout(Duration::from_secs(1), async {
        while registry.list_active_publishers_call_count() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    drop(broadcast_tx);
    tokio::time::timeout(Duration::from_secs(1), start_handle).await??;
    assert_eq!(registry.list_active_publishers_call_count(), 1);
    Ok(())
}

#[tokio::test]
async fn test_broadcast_loop_remains_responsive_during_blocked_maintenance() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry.set_block_batch_refresh(true);
    let (manager, _rx) = test_manager(registry.clone(), "test-node");
    insert_entry(&manager, "heartbeat-room:heartbeat-media");

    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel(16);
    let start_handle = tokio::spawn(Arc::clone(&manager).start(broadcast_rx));
    tokio::time::timeout(Duration::from_secs(1), registry.wait_for_batch_refresh()).await?;

    registry
        .try_register_publisher(
            "event-room",
            "event-media",
            "test-node",
            "event-user",
            "localhost:50051",
        )
        .await?;
    broadcast_tx.send(synctv_xiu::streamhub::define::BroadcastEvent::Publish {
        identifier: StreamIdentifier::Rtmp {
            app_name: "event-room".to_string(),
            stream_name: "event-media".to_string(),
        },
        pub_type: synctv_xiu::streamhub::define::PublishType::RtmpPush,
    })?;

    tokio::time::timeout(Duration::from_secs(1), async {
        while !manager
            .active_publishers
            .contains_key("event-room:event-media")
        {
            tokio::task::yield_now().await;
        }
    })
    .await?;

    drop(broadcast_tx);
    tokio::time::timeout(Duration::from_secs(1), start_handle).await??;
    Ok(())
}

#[tokio::test]
async fn test_reregister_is_serialized_behind_heartbeat() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    registry.set_block_batch_refresh(true);
    let (manager, _rx) = test_manager(registry.clone(), "test-node");
    insert_entry(&manager, "heartbeat-room:heartbeat-media");

    let (broadcast_tx, broadcast_rx) = tokio::sync::broadcast::channel(16);
    let start_handle = tokio::spawn(Arc::clone(&manager).start(broadcast_rx));
    tokio::time::timeout(Duration::from_secs(1), registry.wait_for_batch_refresh()).await?;

    let reregister_handle = tokio::spawn({
        let manager = Arc::clone(&manager);
        async move { manager.reregister_all_publishers().await }
    });
    tokio::task::yield_now().await;
    assert!(
        !reregister_handle.is_finished(),
        "re-registration should wait for the in-flight heartbeat"
    );

    registry.release_batch_refresh();
    tokio::time::timeout(Duration::from_secs(1), reregister_handle).await??;
    drop(broadcast_tx);
    tokio::time::timeout(Duration::from_secs(1), start_handle).await??;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn test_start_stops_heartbeat_and_sync_when_broadcast_channel_closes() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::with_publishers(
        std::collections::HashMap::from([(
            ("room1".to_string(), "media1".to_string()),
            PublisherInfo {
                node_id: "test-node".to_string(),
                api_address: "127.0.0.1:50051".to_string(),
                app_name: "live".to_string(),
                user_id: "user1".to_string(),
                started_at: synctv_core::SystemClock.now(),
                epoch: 1,
            },
        )]),
    ));
    let (hub_tx, _hub_rx) = tokio::sync::mpsc::channel(16);
    let manager = Arc::new(PublisherManager::with_restarting_flag(
        registry.clone(),
        "test-node".to_string(),
        hub_tx,
        Arc::new(AtomicBool::new(false)),
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
    start_handle.await?;

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
    Ok(())
}

#[tokio::test]
async fn test_reregister_removes_stale_entry_when_registry_entry_gone() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await?;

    insert_entry(&manager, "room1:media1");

    assert_eq!(manager.active_publishers.len(), 1);

    registry.unregister_publisher("room1", "media1").await?;

    manager.reregister_all_publishers_once().await;

    assert!(
        manager.active_publishers.is_empty(),
        "Stale entry should be removed from DashMap after reregister"
    );
    Ok(())
}

#[tokio::test]
async fn test_reregister_removes_entry_taken_over_by_other_node() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await?;

    insert_entry(&manager, "room1:media1");

    assert_eq!(manager.active_publishers.len(), 1);

    registry.unregister_publisher("room1", "media1").await?;
    registry
        .try_register_publisher("room1", "media1", "other-node", "user1", "other:50051")
        .await?;

    manager.reregister_all_publishers_once().await;

    assert!(
        manager.active_publishers.is_empty(),
        "Entry should be removed since other node took over"
    );
    Ok(())
}

#[tokio::test]
async fn test_reregister_keeps_entries_owned_by_this_node() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await?;

    insert_registered_entry(&manager, registry.as_ref(), "room1", "media1").await?;

    assert_eq!(manager.active_publishers.len(), 1);

    manager.reregister_all_publishers_once().await;

    assert_eq!(
        manager.active_publishers.len(),
        1,
        "Entry should still be tracked since we own it"
    );
    Ok(())
}

#[tokio::test]
async fn test_reregister_partial_cleanup() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    registry
        .try_register_publisher("room1", "media1", "test-node", "user1", "localhost:50051")
        .await?;
    registry
        .try_register_publisher("room2", "media2", "test-node", "user1", "localhost:50051")
        .await?;
    registry
        .try_register_publisher("room3", "media3", "test-node", "user1", "localhost:50051")
        .await?;

    insert_registered_entry(&manager, registry.as_ref(), "room1", "media1").await?;
    insert_registered_entry(&manager, registry.as_ref(), "room2", "media2").await?;
    insert_registered_entry(&manager, registry.as_ref(), "room3", "media3").await?;

    assert_eq!(manager.active_publishers.len(), 3);

    registry.unregister_publisher("room2", "media2").await?;

    registry.unregister_publisher("room3", "media3").await?;
    registry
        .try_register_publisher("room3", "media3", "other-node", "user1", "other:50051")
        .await?;

    manager.reregister_all_publishers_once().await;

    assert_eq!(
        manager.active_publishers.len(),
        1,
        "Only room1 should remain"
    );
    assert!(
        manager.active_publishers.contains_key("room1:media1"),
        "room1:media1 should still be tracked"
    );
    Ok(())
}

#[tokio::test]
async fn test_memory_leak_regression_zombie_cleanup() -> TestResult {
    let registry = Arc::new(TestStreamRegistry::new());
    let (manager, _rx) = test_manager(registry.clone(), "test-node");

    for i in 0..10 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        registry
            .try_register_publisher(&room, &media, "test-node", "user1", "localhost:50051")
            .await?;
        insert_entry(&manager, &format!("{room}:{media}"));
    }

    assert_eq!(manager.active_publishers.len(), 10);

    for i in 0..10 {
        let room = format!("room{i}");
        let media = format!("media{i}");
        registry.unregister_publisher(&room, &media).await?;
    }

    manager.reregister_all_publishers_once().await;

    assert!(
        manager.active_publishers.is_empty(),
        "All zombie entries should be cleaned up - no memory leak"
    );
    Ok(())
}
