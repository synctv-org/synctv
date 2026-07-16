//! `RoomMessageHub` integration tests
//!
//! Tests for message routing, targeted broadcast, room removal, and
//! safe unsubscribe of unknown connections.

#![allow(clippy::unwrap_used)]
use synctv_core::models::id::{RoomId, UserId};
use synctv_realtime::sync::RoomMessageHub;
use synctv_realtime::sync::{RealtimeEvent, WebRTCSignalKind};

fn stable_test_id(s: &str) -> i64 {
    s.bytes().fold(0_i64, |acc, byte| {
        (acc * 131 + i64::from(byte)) % 900_000_000
    }) + 1
}

fn uid(s: &str) -> UserId {
    UserId::expect_positive(stable_test_id(s))
}

fn rid(s: &str) -> RoomId {
    RoomId::expect_positive(stable_test_id(s))
}

fn chat_event(room: &RoomId, user: &UserId) -> RealtimeEvent {
    RealtimeEvent::ChatMessage {
        event_id: synctv_common::snanoid!(16),
        room_id: *room,
        user_id: *user,
        username: "tester".to_string(),
        message: "hello".to_string(),
        timestamp: chrono::Utc::now(),
        display_position: None,
        display_color: None,
    }
}

fn webrtc_event(room: &RoomId) -> RealtimeEvent {
    RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: *room,
        message_type: WebRTCSignalKind::Offer,
        from: "sender|conn-from".to_string(),
        to: "receiver:conn-target".to_string(),
        data: "{\"sdp\":\"test\"}".to_string(),
        timestamp: chrono::Utc::now(),
    }
}

// Test 1: broadcast_to_connection delivers only to target

#[tokio::test]
async fn test_broadcast_to_connection_targeted() {
    let hub = RoomMessageHub::new();
    let room = rid("r1");
    let u1 = uid("u1");
    let u2 = uid("u2");

    let mut rx1 = hub
        .subscribe(room, u1, "c1".to_string().into())
        .await
        .expect("subscribe should succeed");
    let mut rx2 = hub
        .subscribe(room, u2, "c2".to_string().into())
        .await
        .expect("subscribe should succeed");

    let event = chat_event(&room, &u1);
    let sent = hub.broadcast_to_connection(&room, "c2", event).await;
    assert_eq!(sent, 1, "broadcast_to_connection should return 1");

    // c2 should receive
    let msg = tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv())
        .await
        .expect("c2 should receive")
        .expect("channel not closed");
    assert_eq!(msg.event_type(), "chat_message");

    // c1 should NOT receive
    let r = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv()).await;
    assert!(
        r.is_err(),
        "c1 should not have received the targeted message"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_broadcast_to_connection_reliably_delivers_webrtc_when_channel_full() {
    let hub = RoomMessageHub::new();
    let room = rid("r1");
    let user = uid("u1");

    let mut rx = hub
        .subscribe(room, user, "conn-target".to_string().into())
        .await
        .expect("subscribe should succeed");

    for _ in 0..512 {
        let sent = hub
            .broadcast_to_connection(&room, "conn-target", chat_event(&room, &uid("u2")))
            .await;
        assert_eq!(
            sent, 1,
            "prefill targeted messages should enqueue until channel fills"
        );
    }

    let room_for_task = room;
    let hub_for_task = hub.clone();
    let notify = tokio::spawn(async move {
        hub_for_task
            .broadcast_to_connection(&room_for_task, "conn-target", webrtc_event(&room))
            .await
    });

    tokio::task::yield_now().await;
    let _drained = rx.recv().await.expect("prefill message should exist");

    let sent = tokio::time::timeout(std::time::Duration::from_secs(1), notify)
        .await
        .expect("reliable broadcast task should complete after capacity is freed")
        .expect("broadcast task should not panic");
    assert_eq!(
        sent, 1,
        "WebRTC signaling should be reported as delivered once it is actually queued"
    );

    let mut delivered_webrtc = false;
    for _ in 0..512 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("message should arrive after draining one slot")
            .expect("channel should remain open");
        if matches!(msg.as_ref(), RealtimeEvent::WebRTCSignaling { .. }) {
            delivered_webrtc = true;
            break;
        }
    }

    assert!(
        delivered_webrtc,
        "reliable targeted delivery must eventually enqueue the WebRTC signaling event"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_broadcast_to_connection_does_not_report_success_when_reliable_delivery_times_out() {
    let hub = RoomMessageHub::new();
    let room = rid("r1");
    let user = uid("u1");

    let _rx = hub
        .subscribe(room, user, "conn-target".to_string().into())
        .await
        .expect("subscribe should succeed");

    for _ in 0..512 {
        let sent = hub
            .broadcast_to_connection(&room, "conn-target", chat_event(&room, &uid("u2")))
            .await;
        assert_eq!(sent, 1, "prefill should saturate the subscriber channel");
    }

    let notify = tokio::spawn({
        let hub = hub.clone();
        async move {
            hub.broadcast_to_connection(&room, "conn-target", webrtc_event(&room))
                .await
        }
    });

    tokio::task::yield_now().await;
    tokio::time::sleep(std::time::Duration::from_secs(6)).await;

    let sent = notify.await.expect("notification task should complete");
    assert_eq!(
        sent, 0,
        "targeted reliable delivery must not report success before the message is actually queued"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_broadcast_to_connection_keeps_current_thread_target_registered_when_channel_full() {
    let hub = RoomMessageHub::new();
    let room = rid("r1-current-thread");
    let user = uid("u1");

    let mut rx = hub
        .subscribe(room, user, "conn-target".to_string().into())
        .await
        .expect("subscribe should succeed");

    for _ in 0..512 {
        let sent = hub
            .broadcast_to_connection(&room, "conn-target", chat_event(&room, &uid("u2")))
            .await;
        assert_eq!(sent, 1, "prefill should saturate the subscriber channel");
    }

    let notify = tokio::spawn({
        let hub = hub.clone();
        async move {
            hub.broadcast_to_connection(&room, "conn-target", webrtc_event(&room))
                .await
        }
    });

    assert_eq!(
        hub.connection_count(),
        1,
        "reliable targeted delivery must not unsubscribe the connection while waiting for capacity"
    );

    let drained = rx.recv().await.expect("prefill message should exist");
    assert!(matches!(
        drained.as_ref(),
        RealtimeEvent::ChatMessage { .. }
    ));

    let sent = tokio::time::timeout(std::time::Duration::from_secs(1), notify)
        .await
        .expect("current-thread reliable delivery should complete after capacity is freed")
        .expect("broadcast task should not panic");
    assert_eq!(
        sent, 1,
        "current-thread runtimes should report success only after the event is actually queued"
    );

    let mut delivered_webrtc = false;
    for _ in 0..512 {
        let msg = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("reliable targeted retry should eventually enqueue once capacity is available")
            .expect("channel should remain open");
        if matches!(msg.as_ref(), RealtimeEvent::WebRTCSignaling { .. }) {
            delivered_webrtc = true;
            break;
        }
    }

    assert!(
        delivered_webrtc,
        "expected queued WebRTC signaling after draining capacity"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn test_broadcast_current_thread_drops_when_channel_full() {
    let hub = RoomMessageHub::new();
    let room = rid("r1-broadcast-current-thread");
    let user = uid("u1");

    let _rx = hub
        .subscribe(room, user, "conn-broadcast".to_string().into())
        .await
        .expect("subscribe should succeed");

    for _ in 0..512 {
        let event = chat_event(&room, &uid("u2"));
        let sent = hub.broadcast(&room, &event);
        assert_eq!(sent, 1, "prefill should saturate the subscriber channel");
    }

    let event = RealtimeEvent::RoomDeleted {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        deleted_by: uid("deleter"),
        timestamp: chrono::Utc::now(),
    };

    let sent = hub.broadcast(&room, &event);
    assert_eq!(
        sent, 0,
        "non-blocking broadcast cannot queue a message when the subscriber channel is full"
    );
    assert_eq!(
        hub.connection_count(),
        1,
        "single dropped broadcast stays below slow-consumer disconnect threshold"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_broadcast_to_connection_unsubscribes_target_when_reliable_delivery_times_out() {
    let hub = RoomMessageHub::new();
    let room = rid("r1-cleanup-timeout");
    let user = uid("u1");

    let _rx = hub
        .subscribe(room, user, "conn-target".to_string().into())
        .await
        .expect("subscribe should succeed");

    for _ in 0..512 {
        let sent = hub
            .broadcast_to_connection(&room, "conn-target", chat_event(&room, &uid("u2")))
            .await;
        assert_eq!(sent, 1, "prefill should saturate the subscriber channel");
    }

    let sent = hub
        .broadcast_to_connection(&room, "conn-target", webrtc_event(&room))
        .await;
    assert_eq!(
        sent, 0,
        "reliable targeted delivery should report failure after timing out"
    );
    assert_eq!(
        hub.connection_count(),
        0,
        "timed-out reliable targeted delivery must clean up the dead/stuck connection"
    );
    assert_eq!(
        hub.subscriber_count(&room),
        0,
        "timed-out reliable targeted delivery must remove the subscriber from the room"
    );
}

// Test 2: remove_room cleans up all state

#[tokio::test]
async fn test_remove_room_cleans_connections() {
    let hub = RoomMessageHub::new();
    let room = rid("r1");
    let u1 = uid("u1");
    let u2 = uid("u2");

    let rx1 = hub
        .subscribe(room, u1, "c1".to_string().into())
        .await
        .expect("subscribe should succeed");
    let rx2 = hub
        .subscribe(room, u2, "c2".to_string().into())
        .await
        .expect("subscribe should succeed");

    assert_eq!(hub.subscriber_count(&room), 2);
    assert_eq!(hub.connection_count(), 2);

    hub.remove_room(&room);

    assert_eq!(
        hub.subscriber_count(&room),
        0,
        "Room should have 0 subscribers after removal"
    );
    assert_eq!(
        hub.connection_count(),
        0,
        "All connections should be cleaned up"
    );
    assert_eq!(hub.room_count(), 0, "Room should be removed");

    drop(rx1);
    drop(rx2);
}

// Test 4: unsubscribe unknown connection is safe

#[tokio::test]
async fn test_lifecycle_events_emitted_on_subscribe_unsubscribe() {
    use synctv_realtime::sync::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room = rid("lc_room");
    let user = uid("lc_user");

    // Subscribe should emit RoomActivated
    let _rx = hub
        .subscribe(room, user, "lc_conn".to_string().into())
        .await
        .expect("subscribe should succeed");

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomActivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomActivated, got: {other:?}"),
    }

    // Unsubscribe the only subscriber should emit RoomDeactivated
    hub.unsubscribe("lc_conn");

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomDeactivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomDeactivated, got: {other:?}"),
    }
}

/// Lifecycle events are not lost when multiple rooms are created quickly.
#[tokio::test]
async fn test_lifecycle_events_not_lost_under_room_churn() {
    use synctv_realtime::sync::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    // Rapidly create and destroy 10 rooms
    for i in 0..10 {
        let room = rid(&format!("churn_room_{i}"));
        let user = uid(&format!("churn_user_{i}"));
        let conn_id = format!("churn_conn_{i}");

        let _rx = hub
            .subscribe(room, user, conn_id.clone().into())
            .await
            .expect("subscribe should succeed");
        hub.unsubscribe(&conn_id);
    }

    // We should receive all 20 events (10 activated + 10 deactivated)
    let mut activated = 0;
    let mut deactivated = 0;
    while let Ok(event) = lifecycle_rx.try_recv() {
        match event {
            RoomLifecycleEvent::RoomActivated(_) => activated += 1,
            RoomLifecycleEvent::RoomDeactivated(_) => deactivated += 1,
        }
    }

    assert_eq!(
        activated, 10,
        "All 10 RoomActivated events should be received"
    );
    assert_eq!(
        deactivated, 10,
        "All 10 RoomDeactivated events should be received"
    );
}

/// When the last subscriber unsubscribes and a new subscriber joins the same
/// room concurrently, the new subscriber must see a RoomActivated event.
#[tokio::test]
async fn test_unsubscribe_last_then_subscribe_emits_activated() {
    use synctv_realtime::sync::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room = rid("race_room");
    let user1 = uid("user1");
    let user2 = uid("user2");

    // Subscribe first user -> RoomActivated
    let _rx1 = hub
        .subscribe(room, user1, "conn1".to_string().into())
        .await
        .expect("subscribe should succeed");

    let event = lifecycle_rx.try_recv().unwrap();
    assert!(matches!(event, RoomLifecycleEvent::RoomActivated(_)));

    // Unsubscribe first user -> RoomDeactivated
    hub.unsubscribe("conn1");

    let event = lifecycle_rx.try_recv().unwrap();
    assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(_)));

    // After the room is fully removed, subscribing a second user must
    // emit another RoomActivated (the room is re-created from scratch).
    let _rx2 = hub
        .subscribe(room, user2, "conn2".to_string().into())
        .await
        .expect("subscribe should succeed");

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomActivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomActivated after re-subscribe, got: {other:?}"),
    }

    // Verify room has exactly 1 subscriber
    assert_eq!(hub.subscriber_count(&room), 1);
    assert_eq!(hub.room_count(), 1);
}

/// remove_room emits a RoomDeactivated lifecycle event.
#[tokio::test]
async fn test_remove_room_emits_deactivated_event() {
    use synctv_realtime::sync::RoomLifecycleEvent;

    let hub = RoomMessageHub::new();
    let mut lifecycle_rx = hub.subscribe_lifecycle();

    let room = rid("rm_room");
    let user = uid("rm_user");

    let _rx = hub
        .subscribe(room, user, "rm_conn".to_string().into())
        .await
        .expect("subscribe should succeed");

    // Consume the RoomActivated event
    let _ = lifecycle_rx.try_recv().unwrap();

    // Remove the room (simulates cross-replica deletion)
    hub.remove_room(&room);

    let event = lifecycle_rx.try_recv().unwrap();
    match event {
        RoomLifecycleEvent::RoomDeactivated(r) => assert_eq!(r, room),
        other => panic!("Expected RoomDeactivated on remove_room, got: {other:?}"),
    }
}
