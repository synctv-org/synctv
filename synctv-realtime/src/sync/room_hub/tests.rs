use super::*;
use chrono::Utc;
use std::time::Duration;

#[tokio::test]
async fn test_subscribe_and_broadcast() {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_148);

    // Subscribe
    let mut rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");

    assert_eq!(hub.subscriber_count(&room_id), 1);
    assert_eq!(hub.connection_count(), 1);

    // Broadcast event
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id,
        username: "testuser".to_string(),
        message: "Hello!".to_string(),
        timestamp: Utc::now(),
        display_position: None,
        display_color: None,
    };

    let sent_count = hub.broadcast(&room_id, &event);
    assert_eq!(sent_count, 1);

    // Receive event
    let received = rx.recv().await.unwrap();
    assert_eq!(received.event_type(), "chat_message");
}

#[tokio::test]
async fn test_unsubscribe() {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_148);

    // Subscribe
    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    assert_eq!(hub.subscriber_count(&room_id), 1);

    // Unsubscribe
    hub.unsubscribe("conn1");
    assert_eq!(hub.subscriber_count(&room_id), 0);
    assert_eq!(hub.connection_count(), 0);
    assert_eq!(hub.room_count(), 0);
}

#[tokio::test]
async fn test_multiple_subscribers() {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    // Subscribe two clients
    let mut rx1 = hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx2 = hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    assert_eq!(hub.subscriber_count(&room_id), 2);

    // Broadcast event
    let event = RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id,
        user_id: user1,
        username: "user1".to_string(),
        message: "Hello!".to_string(),
        timestamp: Utc::now(),
        display_position: None,
        display_color: None,
    };

    let sent_count = hub.broadcast(&room_id, &event);
    assert_eq!(sent_count, 2);

    // Both should receive
    let received1 = rx1.recv().await.unwrap();
    let received2 = rx2.recv().await.unwrap();

    assert_eq!(received1.event_type(), "chat_message");
    assert_eq!(received2.event_type(), "chat_message");
}

#[tokio::test]
async fn test_distributed_subscribers_returns_error_when_redis_snapshot_unavailable() {
    struct HangingRedisRuntime;

    #[async_trait::async_trait]
    impl RedisConnectionRuntime for HangingRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            std::future::pending().await
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    let hub =
        RoomMessageHub::new_with_redis_runtime(Arc::new(HangingRedisRuntime), "test-timeout:");
    let room_id = RoomId::expect_positive(10_000_196);
    let user_id = UserId::expect_positive(10_000_197);
    let connection_id = ConnectionId::new("local-only-would-be-misleading");

    {
        let mut room = HashMap::new();
        room.insert(
            connection_id.clone(),
            Subscriber {
                connection_id: connection_id.clone(),
                user_id,
                sender: mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY).0,
                consecutive_drops: Arc::new(AtomicU32::new(0)),
            },
        );
        hub.rooms.insert(room_id, room);
        hub.connections
            .insert(connection_id.clone(), (room_id, user_id));
    }

    let error = hub
        .get_room_subscribers_distributed(&room_id)
        .await
        .expect_err("Redis-backed distributed lookup must not fall back to local-only data");

    assert!(
        error
            .to_string()
            .contains("load distributed room subscribers"),
        "unexpected error: {error}"
    );

    hub.shutdown().await;
}

#[tokio::test]
async fn test_broadcast_to_specific_user() {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    // Subscribe two clients
    let mut rx1 = hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx2 = hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    // Broadcast to user1 only
    let event = RealtimeEvent::SystemNotification {
        event_id: synctv_common::snanoid!(16),
        message: "Private message".to_string(),
        level: crate::sync::NotificationLevel::Info,
        timestamp: Utc::now(),
    };

    let sent_count = hub.broadcast_to_user(&room_id, &user1, &event);
    assert_eq!(sent_count, 1);

    // Only user1 should receive
    let received1 = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(received1.event_type(), "system_notification");

    // User2 should not receive
    let received2 = tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv()).await;

    assert!(
        received2.is_err(),
        "User2 should not have received the message"
    );
}

#[tokio::test]
async fn test_lifecycle_events_on_subscribe_unsubscribe() {
    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room_id = RoomId::expect_positive(10_000_009);
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    // First subscriber triggers RoomActivated
    let _rx1 = hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let event = lifecycle_rx.try_recv().unwrap();
    assert!(matches!(event, RoomLifecycleEvent::RoomActivated(ref rid) if rid == &room_id));

    // Second subscriber does NOT trigger RoomActivated
    let _rx2 = hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");
    assert!(lifecycle_rx.try_recv().is_err());

    // Unsubscribe first user: room still has subscribers, no event
    hub.unsubscribe("conn1");
    assert!(lifecycle_rx.try_recv().is_err());

    // Unsubscribe last user: triggers RoomDeactivated
    hub.unsubscribe("conn2");
    let event = lifecycle_rx.try_recv().unwrap();
    assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid == &room_id));
}

#[tokio::test]
async fn test_lifecycle_events_on_remove_room() {
    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_010);

    // Subscribe triggers RoomActivated
    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let _ = lifecycle_rx.try_recv().unwrap(); // consume RoomActivated

    // remove_room triggers RoomDeactivated
    hub.remove_room(&room_id);
    let event = lifecycle_rx.try_recv().unwrap();
    assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid == &room_id));
}

#[tokio::test]
async fn test_broadcast_reliably_waits_for_critical_event_queue_space() {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_159);
    let deleted_by = UserId::expect_positive(10_000_027);
    let filler_user = UserId::expect_positive(10_000_160);

    let mut rx = hub
        .subscribe(room_id, filler_user, ConnectionId::new("conn-critical"))
        .await
        .expect("subscribe should succeed");

    for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: deleted_by,
            username: "filler".to_string(),
            message: "fill".to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        };
        let sent = hub.broadcast(&room_id, &event);
        assert_eq!(sent, 1, "filler message should enqueue");
    }

    let room_deleted = RealtimeEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id,
        deleted_by,
        timestamp: Utc::now(),
    };

    let hub_for_task = hub.clone();
    let room_for_task = room_id;
    let broadcast_task = tokio::spawn(async move {
        hub_for_task
            .broadcast_reliably(&room_for_task, room_deleted)
            .await
    });

    tokio::task::yield_now().await;
    assert!(
        !broadcast_task.is_finished(),
        "critical broadcast should wait until the subscriber channel has capacity"
    );

    let drained = rx.recv().await.expect("filler message should be present");
    assert!(matches!(drained, RealtimeEvent::ChatMessage { .. }));

    let sent = tokio::time::timeout(Duration::from_secs(1), broadcast_task)
        .await
        .expect("reliable broadcast should complete after capacity is freed")
        .expect("broadcast task should not panic");
    assert_eq!(
        sent, 1,
        "critical event should count as delivered once queued"
    );

    let mut saw_room_deleted = false;
    for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
        let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("queued message should arrive")
            .expect("channel should stay open");
        if matches!(msg, RealtimeEvent::RoomDeleted { .. }) {
            saw_room_deleted = true;
            break;
        }
    }

    assert!(
        saw_room_deleted,
        "critical room deletion event should be queued before cleanup proceeds"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_broadcast_waits_for_critical_event_queue_space() {
    let hub = Arc::new(RoomMessageHub::new());
    let room_id = RoomId::expect_positive(10_000_161);
    let user_id = UserId::expect_positive(10_000_162);
    let mut rx = hub
        .subscribe(
            room_id,
            user_id,
            ConnectionId::new("conn-critical-broadcast"),
        )
        .await
        .expect("subscribe should succeed");

    for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_163),
            username: "filler".to_string(),
            message: "fill".to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        };
        let sent = hub.broadcast(&room_id, &event);
        assert_eq!(sent, 1, "filler message should enqueue");
    }

    let critical = RealtimeEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id,
        deleted_by: UserId::expect_positive(10_000_164),
        timestamp: Utc::now(),
    };

    let hub_for_task = hub.clone();
    let room_for_task = room_id;
    let broadcast_task =
        tokio::spawn(async move { hub_for_task.broadcast(&room_for_task, &critical) });

    tokio::task::yield_now().await;
    assert!(
        !broadcast_task.is_finished(),
        "critical broadcast should wait until channel capacity is freed"
    );

    let drained = rx.recv().await.expect("filler message should be present");
    assert!(matches!(drained, RealtimeEvent::ChatMessage { .. }));

    let sent = tokio::time::timeout(Duration::from_secs(1), broadcast_task)
        .await
        .expect("critical broadcast should complete after capacity is freed")
        .expect("broadcast task should not panic");
    assert_eq!(
        sent, 1,
        "critical broadcast must only count deliveries that were actually queued"
    );

    let mut saw_room_deleted = false;
    for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
        let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("queued message should arrive")
            .expect("channel should stay open");
        if matches!(msg, RealtimeEvent::RoomDeleted { .. }) {
            saw_room_deleted = true;
            break;
        }
    }

    assert!(
        saw_room_deleted,
        "critical event must be queued before broadcast() returns success"
    );
}

#[tokio::test(start_paused = true)]
async fn test_broadcast_reliably_unsubscribes_connection_after_delivery_timeout() {
    let hub = Arc::new(RoomMessageHub::new());
    let room_id = RoomId::expect_positive(10_000_165);
    let user_id = UserId::expect_positive(10_000_166);
    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn-reliable-timeout"))
        .await
        .expect("subscribe should succeed");

    for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
        let event = RealtimeEvent::ChatMessage {
            event_id: synctv_common::snanoid!(16),
            room_id,
            user_id: UserId::expect_positive(10_000_163),
            username: "filler".to_string(),
            message: "fill".to_string(),
            timestamp: Utc::now(),
            display_position: None,
            display_color: None,
        };
        let sent = hub.broadcast(&room_id, &event);
        assert_eq!(sent, 1, "filler message should enqueue");
    }

    let hub_for_task = Arc::clone(&hub);
    let room_for_task = room_id;
    let broadcast_task = tokio::spawn(async move {
        hub_for_task
            .broadcast_reliably(
                &room_for_task,
                RealtimeEvent::RoomDeleted {
                    event_id: synctv_common::snanoid!(16),
                    room_id: room_for_task,
                    deleted_by: UserId::expect_positive(10_000_164),
                    timestamp: Utc::now(),
                },
            )
            .await
    });

    tokio::task::yield_now().await;
    tokio::time::advance(CRITICAL_EVENT_SEND_TIMEOUT + Duration::from_secs(1)).await;

    let sent = broadcast_task
        .await
        .expect("reliable timeout task should not panic");
    assert_eq!(
        sent, 0,
        "timed out reliable delivery must not count as sent"
    );
    assert_eq!(
        hub.subscriber_count(&room_id),
        0,
        "timed out reliable delivery must unsubscribe the stuck connection"
    );
    assert_eq!(
        hub.connection_count(),
        0,
        "timed out reliable delivery must clear connection tracking"
    );
}

#[test]
fn test_broadcast_counts_deferred_critical_delivery_on_current_thread_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::expect_positive(10_000_167);
        let user_id = UserId::expect_positive(10_000_168);

        let mut rx = hub
            .subscribe(room_id, user_id, ConnectionId::new("conn-critical-deferred"))
            .await
            .expect("subscribe should succeed");

        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let event = RealtimeEvent::ChatMessage {
                event_id: synctv_common::snanoid!(16),
                room_id,
                user_id: UserId::expect_positive(10_000_163),
                username: "filler".to_string(),
                message: "fill".to_string(),
                timestamp: Utc::now(),
                display_position: None,
                display_color: None,
            };
            let sent = hub.broadcast(
                &room_id,
                &event,
            );
            assert_eq!(sent, 1, "filler message should enqueue");
        }

        let event = RealtimeEvent::RoomDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id,
            deleted_by: UserId::expect_positive(10_000_164),
            timestamp: Utc::now(),
        };
        let sent = hub.broadcast(
            &room_id,
            &event,
        );

        assert_eq!(
            sent, 1,
            "current-thread deferred critical delivery should count as locally accepted"
        );

        assert_eq!(
            hub.connection_count(),
            1,
            "deferred current-thread delivery must keep the subscriber registered"
        );

        let _drained = rx.recv().await.expect("prefill message should exist");
        let mut delivered_delete = false;
        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect(
                    "deferred critical broadcast should eventually enqueue once capacity is available",
                )
                .expect("channel should remain open");
            if matches!(msg, RealtimeEvent::RoomDeleted { .. }) {
                delivered_delete = true;
                break;
            }
        }

        assert!(
            delivered_delete,
            "current-thread broadcast fallback must still enqueue the critical event after capacity is freed"
        );
    });
}

#[test]
fn test_broadcast_to_user_counts_deferred_critical_delivery_on_current_thread_runtime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("current-thread runtime");

    runtime.block_on(async {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::expect_positive(10_000_169);
        let user_id = UserId::expect_positive(10_000_170);

        let mut rx = hub
            .subscribe(
                room_id,
                user_id,
                ConnectionId::new("conn-targeted-deferred"),
            )
            .await
            .expect("subscribe should succeed");

        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let event = RealtimeEvent::ChatMessage {
                event_id: synctv_common::snanoid!(16),
                room_id,
                user_id: UserId::expect_positive(10_000_163),
                username: "filler".to_string(),
                message: "fill".to_string(),
                timestamp: Utc::now(),
                display_position: None,
                display_color: None,
            };
            let sent = hub.broadcast(
                &room_id,
                &event,
            );
            assert_eq!(sent, 1, "filler message should enqueue");
        }

        let event = RealtimeEvent::RoomDeleted {
            event_id: synctv_common::snanoid!(16),
            room_id,
            deleted_by: UserId::expect_positive(10_000_164),
            timestamp: Utc::now(),
        };
        let sent = hub.broadcast_to_user(&room_id, &user_id, &event);

        assert_eq!(
            sent, 1,
            "current-thread deferred targeted critical delivery should count as locally accepted"
        );

        assert_eq!(
            hub.connection_count(),
            1,
            "deferred targeted delivery must keep the subscriber registered"
        );

        let _drained = rx.recv().await.expect("prefill message should exist");
        let mut delivered_delete = false;
        for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
            let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect(
                    "deferred targeted critical delivery should eventually enqueue once capacity is available",
                )
                .expect("channel should remain open");
            if matches!(msg, RealtimeEvent::RoomDeleted { .. }) {
                delivered_delete = true;
                break;
            }
        }

        assert!(
            delivered_delete,
            "current-thread targeted fallback must still enqueue the critical event after capacity is freed"
        );
    });
}

#[tokio::test]
async fn test_unsubscribe_cleans_up_local_state_even_without_redis() {
    // Verify that unsubscribe properly cleans up local state (rooms + connections)
    // even when Redis is not configured. This is the baseline behavior.
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_010);

    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    assert_eq!(hub.subscriber_count(&room_id), 1);
    assert_eq!(hub.connection_count(), 1);

    hub.unsubscribe("conn1");

    // Local state should be fully cleaned up
    assert_eq!(hub.subscriber_count(&room_id), 0);
    assert_eq!(hub.connection_count(), 0);
    assert_eq!(hub.room_count(), 0);
}

#[tokio::test]
async fn test_cleanup_orphaned_subscriptions_only_tracks_local_failed_cleanup() {
    let hub = RoomMessageHub::new();

    hub.pending_redis_cleanup.insert(
        ConnectionId::new("conn_local"),
        RoomId::expect_positive(10_000_171),
    );

    assert_eq!(hub.pending_redis_cleanup.len(), 1);

    hub.cleanup_orphaned_redis_subscriptions().await;

    assert_eq!(
        hub.pending_redis_cleanup.len(),
        1,
        "Without Redis, cleanup must not mutate locally tracked retry state"
    );
    assert!(hub.pending_redis_cleanup.contains_key("conn_local"));
}

#[tokio::test]
async fn test_subscribe_clears_stale_pending_cleanup_for_reused_connection_id() {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_172);
    let user_id = UserId::expect_positive(10_000_173);

    hub.pending_redis_cleanup.insert(
        ConnectionId::new("conn_reuse"),
        RoomId::expect_positive(10_000_174),
    );

    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn_reuse"))
        .await
        .expect("subscribe should succeed");

    assert!(
        !hub.pending_redis_cleanup.contains_key("conn_reuse"),
        "New subscription must clear stale pending cleanup for reused connection IDs"
    );
}

#[tokio::test]
async fn test_stale_cleanup_task_can_be_cancelled() {
    // Verify the stale cleanup task respects cancellation tokens
    let hub = RoomMessageHub::new();
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = hub
        .spawn_stale_subscription_cleanup_task(Duration::from_millis(50), cancel.clone())
        .expect("stale cleanup task should spawn inside Tokio runtime");

    // Let it run briefly
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel and verify the task completes
    cancel.cancel();
    let result = tokio::time::timeout(Duration::from_secs(2), handle).await;
    assert!(
        result.is_ok(),
        "Cleanup task should complete after cancellation"
    );
}

#[tokio::test]
async fn test_shutdown_cancels_all_background_tasks() {
    // Verify that shutdown() cancels both the TTL refresh and stale cleanup tasks
    let hub = RoomMessageHub::new();

    // Manually spawn tasks with known cancel tokens to verify they stop
    let ttl_cancel = tokio_util::sync::CancellationToken::new();
    let cleanup_cancel = tokio_util::sync::CancellationToken::new();

    let ttl_handle = hub
        .spawn_ttl_refresh_task(Duration::from_millis(50), ttl_cancel.clone())
        .expect("TTL refresh task should spawn inside Tokio runtime");
    let cleanup_handle = hub
        .spawn_stale_subscription_cleanup_task(Duration::from_millis(50), cleanup_cancel.clone())
        .expect("stale cleanup task should spawn inside Tokio runtime");

    // Let tasks start running
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cancel both
    ttl_cancel.cancel();
    cleanup_cancel.cancel();

    // Both tasks should complete within a reasonable timeout
    let ttl_result = tokio::time::timeout(Duration::from_secs(2), ttl_handle).await;
    let cleanup_result = tokio::time::timeout(Duration::from_secs(2), cleanup_handle).await;

    assert!(
        ttl_result.is_ok(),
        "TTL refresh task should complete after cancellation"
    );
    assert!(
        cleanup_result.is_ok(),
        "Stale cleanup task should complete after cancellation"
    );
}

#[tokio::test]
async fn test_shutdown_aborts_stuck_background_tasks() {
    let hub = RoomMessageHub::new();

    hub.ttl_refresh_handle.lock().replace(tokio::spawn(async {
        futures::future::pending::<()>().await;
    }));
    hub.stale_cleanup_handle.lock().replace(tokio::spawn(async {
        futures::future::pending::<()>().await;
    }));

    let result = tokio::time::timeout(Duration::from_secs(6), hub.shutdown()).await;
    assert!(
        result.is_ok(),
        "shutdown should abort stuck RoomMessageHub background tasks instead of hanging"
    );
    assert!(
        hub.ttl_refresh_handle.lock().is_none(),
        "shutdown must drain timed-out ttl refresh handles"
    );
    assert!(
        hub.stale_cleanup_handle.lock().is_none(),
        "shutdown must drain timed-out stale cleanup handles"
    );
}

#[tokio::test]
async fn test_remove_room_cleans_connection_tracking() {
    // Verify that remove_room removes connections from the tracking map
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    let _rx1 = hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let _rx2 = hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    assert_eq!(hub.connection_count(), 2);

    hub.remove_room(&room_id);

    assert_eq!(hub.connection_count(), 0);
    assert_eq!(hub.room_count(), 0);
}
