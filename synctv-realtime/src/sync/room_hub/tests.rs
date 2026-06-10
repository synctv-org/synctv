use super::*;
use chrono::Utc;
use std::time::Duration;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn test_subscribe_and_broadcast() -> TestResult {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_148);

    let mut rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await?;

    assert_eq!(hub.subscriber_count(&room_id), 1);
    assert_eq!(hub.connection_count(), 1);

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

    let received = rx.recv().await.ok_or("message should be delivered")?;
    assert_eq!(received.event_type(), "chat_message");
    Ok(())
}

#[tokio::test]
async fn test_unsubscribe() -> TestResult {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_148);

    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await?;
    assert_eq!(hub.subscriber_count(&room_id), 1);

    hub.unsubscribe("conn1");
    assert_eq!(hub.subscriber_count(&room_id), 0);
    assert_eq!(hub.connection_count(), 0);
    assert_eq!(hub.room_count(), 0);
    Ok(())
}

#[tokio::test]
async fn test_multiple_subscribers() -> TestResult {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    let mut rx1 = hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await?;
    let mut rx2 = hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await?;

    assert_eq!(hub.subscriber_count(&room_id), 2);

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

    let received1 = rx1.recv().await.ok_or("first subscriber should receive")?;
    let received2 = rx2.recv().await.ok_or("second subscriber should receive")?;

    assert_eq!(received1.event_type(), "chat_message");
    assert_eq!(received2.event_type(), "chat_message");
    Ok(())
}

#[tokio::test]
async fn test_distributed_subscribers_returns_error_when_redis_snapshot_unavailable() -> TestResult
{
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
    Ok(())
}

#[tokio::test]
async fn test_lifecycle_events_on_subscribe_unsubscribe() -> TestResult {
    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room_id = RoomId::expect_positive(10_000_009);
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    let _rx1 = hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await?;
    let event = lifecycle_rx.try_recv()?;
    assert!(matches!(event, RoomLifecycleEvent::RoomActivated(ref rid) if rid == &room_id));

    let _rx2 = hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await?;
    assert!(lifecycle_rx.try_recv().is_err());

    hub.unsubscribe("conn1");
    assert!(lifecycle_rx.try_recv().is_err());

    hub.unsubscribe("conn2");
    let event = lifecycle_rx.try_recv()?;
    assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid == &room_id));
    Ok(())
}

#[tokio::test]
async fn test_lifecycle_events_on_remove_room() -> TestResult {
    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_010);

    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await?;
    let _ = lifecycle_rx.try_recv()?;

    hub.remove_room(&room_id);
    let event = lifecycle_rx.try_recv()?;
    assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid == &room_id));
    Ok(())
}

#[tokio::test]
async fn test_broadcast_reliably_waits_for_critical_event_queue_space() -> TestResult {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_159);
    let deleted_by = UserId::expect_positive(10_000_027);
    let filler_user = UserId::expect_positive(10_000_160);

    let mut rx = hub
        .subscribe(room_id, filler_user, ConnectionId::new("conn-critical"))
        .await?;

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

    let drained = rx.recv().await.ok_or("filler message should be present")?;
    assert!(matches!(drained, RealtimeEvent::ChatMessage { .. }));

    let sent = tokio::time::timeout(Duration::from_secs(1), broadcast_task).await??;
    assert_eq!(
        sent, 1,
        "critical event should count as delivered once queued"
    );

    let mut saw_room_deleted = false;
    for _ in 0..SUBSCRIBER_CHANNEL_CAPACITY {
        let msg = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await?
            .ok_or("queued message should arrive")?;
        if matches!(msg, RealtimeEvent::RoomDeleted { .. }) {
            saw_room_deleted = true;
            break;
        }
    }

    assert!(
        saw_room_deleted,
        "critical room deletion event should be queued before cleanup proceeds"
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_broadcast_drops_when_subscriber_queue_is_full() -> TestResult {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_161);
    let user_id = UserId::expect_positive(10_000_162);
    let _rx = hub
        .subscribe(
            room_id,
            user_id,
            ConnectionId::new("conn-critical-broadcast"),
        )
        .await?;

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

    let sent = hub.broadcast(&room_id, &critical);
    assert_eq!(sent, 0, "non-blocking broadcast cannot queue when full");
    assert_eq!(
        hub.connection_count(),
        1,
        "single dropped event stays below slow-consumer disconnect threshold"
    );
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn test_broadcast_reliably_unsubscribes_connection_after_delivery_timeout() -> TestResult {
    let hub = Arc::new(RoomMessageHub::new());
    let room_id = RoomId::expect_positive(10_000_165);
    let user_id = UserId::expect_positive(10_000_166);
    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn-reliable-timeout"))
        .await?;

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

    let sent = broadcast_task.await?;
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
    Ok(())
}

#[tokio::test]
async fn test_unsubscribe_cleans_up_local_state_even_without_redis() -> TestResult {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user_id = UserId::expect_positive(10_000_010);

    let _rx = hub
        .subscribe(room_id, user_id, ConnectionId::new("conn1"))
        .await?;
    assert_eq!(hub.subscriber_count(&room_id), 1);
    assert_eq!(hub.connection_count(), 1);

    hub.unsubscribe("conn1");

    assert_eq!(hub.subscriber_count(&room_id), 0);
    assert_eq!(hub.connection_count(), 0);
    assert_eq!(hub.room_count(), 0);
    Ok(())
}

#[tokio::test]
async fn test_shutdown_cancels_all_background_tasks() -> TestResult {
    let hub = RoomMessageHub::new();

    let ttl_cancel = tokio_util::sync::CancellationToken::new();

    let ttl_handle = hub
        .spawn_ttl_refresh_task(Duration::from_millis(50), ttl_cancel.clone())
        .ok_or("TTL refresh task should spawn inside Tokio runtime")?;

    tokio::time::sleep(Duration::from_millis(100)).await;

    ttl_cancel.cancel();

    tokio::time::timeout(Duration::from_secs(2), ttl_handle).await??;
    Ok(())
}

#[tokio::test]
async fn test_shutdown_aborts_stuck_background_tasks() -> TestResult {
    let hub = RoomMessageHub::new();

    hub.ttl_refresh_handle.lock().replace(tokio::spawn(async {
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
    Ok(())
}

#[tokio::test]
async fn test_remove_room_cleans_connection_tracking() -> TestResult {
    let hub = RoomMessageHub::new();
    let room_id = RoomId::expect_positive(10_000_009);
    let user1 = UserId::expect_positive(10_000_010);
    let user2 = UserId::expect_positive(10_000_095);

    let _rx1 = hub
        .subscribe(room_id, user1, ConnectionId::new("conn1"))
        .await?;
    let _rx2 = hub
        .subscribe(room_id, user2, ConnectionId::new("conn2"))
        .await?;

    assert_eq!(hub.connection_count(), 2);

    hub.remove_room(&room_id);

    assert_eq!(hub.connection_count(), 0);
    assert_eq!(hub.room_count(), 0);
    Ok(())
}
