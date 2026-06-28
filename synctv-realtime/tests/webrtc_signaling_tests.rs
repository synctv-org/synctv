//! WebRTC signaling tests
//!
//! Tests for WebRTC signaling events: ICE candidate exchange, SDP offer/answer,
//! and signaling timeouts.

#![allow(clippy::unwrap_used)]
use std::time::Duration;

use synctv_core::models::id::{RoomId, UserId};
use synctv_realtime::sync::{ConnectionId, ConnectionManager, RoomMessageHub};
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

// Test 1: ICE candidate exchange

/// Test ICE candidate event creation and serialization.
#[test]
fn test_ice_candidate_event_serialization() {
    let room = rid("room1");
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::IceCandidate,
        from: "user1|conn1".to_string(),
        to: "user2:conn2".to_string(),
        data:
            r#"{"candidate":"candidate:842163049 1 udp typ host","sdpMid":"0","sdpMLineIndex":0}"#
                .to_string(),
        timestamp: chrono::Utc::now(),
    };

    // Serialize
    let json = serde_json::to_string(&event).expect("Should serialize");
    assert!(
        json.contains(r#""type":"webRTCSignaling"#),
        "JSON should contain type field: {json}"
    );
    assert!(json.contains("iceCandidate"));
    assert!(json.contains("candidate:842163049"));

    // Deserialize
    let decoded: RealtimeEvent = serde_json::from_str(&json).expect("Should deserialize");
    assert_eq!(decoded.event_type(), "webrtc_signaling");

    if let RealtimeEvent::WebRTCSignaling {
        message_type,
        from,
        to,
        data,
        ..
    } = decoded
    {
        assert_eq!(message_type, WebRTCSignalKind::IceCandidate);
        assert_eq!(from, "user1|conn1");
        assert_eq!(to, "user2:conn2");
        assert!(data.contains("candidate:"));
    } else {
        panic!("Expected WebRTCSignaling variant");
    }
}

/// Test ICE candidate routing to specific connection.
#[tokio::test]
async fn test_ice_candidate_routing() {
    let hub = RoomMessageHub::new();
    let room = rid("room1");
    let user1 = uid("user1");
    let user2 = uid("user2");

    // Both users subscribe
    let mut rx1 = hub
        .subscribe(room, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx2 = hub
        .subscribe(room, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::IceCandidate,
        from: format!("{user1}|conn1"),
        to: format!("{user2}:conn2"),
        data: r#"{"candidate":"test"}"#.to_string(),
        timestamp: chrono::Utc::now(),
    };

    // Broadcast to specific connection
    let sent = hub.broadcast_to_connection(&room, "conn2", event).await;
    assert_eq!(sent, 1, "Should send to exactly one connection");

    // user2 should receive
    let result = tokio::time::timeout(Duration::from_millis(100), rx2.recv()).await;
    assert!(result.is_ok(), "user2 should receive ICE candidate");

    // user1 should NOT receive
    let result = tokio::time::timeout(Duration::from_millis(100), rx1.recv()).await;
    assert!(
        result.is_err(),
        "user1 should not receive (targeted message)"
    );
}

#[tokio::test]
async fn test_ice_candidate_routing_uses_explicit_target_connection() {
    let hub = RoomMessageHub::new();
    let room = rid("room1");
    let user1 = uid("user1");
    let user2 = uid("user2");

    let mut rx1 = hub
        .subscribe(room, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx2 = hub
        .subscribe(room, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::IceCandidate,
        from: format!("{user1}|conn1"),
        to: format!("{user2}:conn2"),
        data: r#"{"candidate":"test"}"#.to_string(),
        timestamp: chrono::Utc::now(),
    };

    let sent = hub.broadcast_to_connection(&room, "conn2", event).await;
    assert_eq!(sent, 1, "signaling should target one connection");

    let result = tokio::time::timeout(Duration::from_millis(100), rx2.recv()).await;
    assert!(result.is_ok(), "target connection should receive signal");

    let result = tokio::time::timeout(Duration::from_millis(100), rx1.recv()).await;
    assert!(
        result.is_err(),
        "non-target connection should not receive the signal"
    );
}

// Test 2: SDP offer/answer exchange

/// Test SDP offer event.
#[test]
fn test_sdp_offer_event() {
    let room = rid("room1");
    let offer_sdp = r"v=0
o=- 4611731400430051338 2 IN IP4 127.0.0.1
s=-
t=0 0
a=group:BUNDLE 0 1
m=audio 9 UDP/TLS/RTP/SAVPF 111 103 104 9 0 8 106 105 13 110 112 113 126
...";

    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::Offer,
        from: "caller|conn1".to_string(),
        to: "callee:conn2".to_string(),
        data: offer_sdp.to_string(),
        timestamp: chrono::Utc::now(),
    };

    let json = serde_json::to_string(&event).expect("Should serialize");
    assert!(json.contains("\"offer\""));

    let decoded: RealtimeEvent = serde_json::from_str(&json).expect("Should deserialize");
    if let RealtimeEvent::WebRTCSignaling {
        message_type, data, ..
    } = decoded
    {
        assert_eq!(message_type, WebRTCSignalKind::Offer);
        assert!(data.contains("v=0"));
    } else {
        panic!("Expected WebRTCSignaling variant");
    }
}

/// Test SDP answer event.
#[test]
fn test_sdp_answer_event() {
    let room = rid("room1");
    let answer_sdp = r"v=0
o=- 4611731400430051339 2 IN IP4 127.0.0.1
s=-
t=0 0
a=group:BUNDLE 0 1
...";

    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::Answer,
        from: "callee|conn2".to_string(),
        to: "caller:conn1".to_string(),
        data: answer_sdp.to_string(),
        timestamp: chrono::Utc::now(),
    };

    let json = serde_json::to_string(&event).expect("Should serialize");
    assert!(json.contains("\"answer\""));

    let decoded: RealtimeEvent = serde_json::from_str(&json).expect("Should deserialize");
    if let RealtimeEvent::WebRTCSignaling { message_type, .. } = decoded {
        assert_eq!(message_type, WebRTCSignalKind::Answer);
    } else {
        panic!("Expected WebRTCSignaling variant");
    }
}

/// Test full SDP exchange flow.
#[tokio::test]
async fn test_sdp_offer_answer_flow() {
    let hub = RoomMessageHub::new();
    let room = rid("room1");
    let caller = uid("caller");
    let callee = uid("callee");

    let mut rx_caller = hub
        .subscribe(room, caller, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx_callee = hub
        .subscribe(room, callee, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    // Caller sends offer
    let offer = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::Offer,
        from: format!("{caller}|conn1"),
        to: format!("{callee}:conn2"),
        data: "OFFER_SDP".to_string(),
        timestamp: chrono::Utc::now(),
    };
    hub.broadcast_to_connection(&room, "conn2", offer).await;

    // Callee receives offer
    let received = tokio::time::timeout(Duration::from_millis(100), rx_callee.recv()).await;
    assert!(received.is_ok(), "Callee should receive offer");

    // Callee sends answer
    let answer = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::Answer,
        from: format!("{callee}|conn2"),
        to: format!("{caller}:conn1"),
        data: "ANSWER_SDP".to_string(),
        timestamp: chrono::Utc::now(),
    };
    hub.broadcast_to_connection(&room, "conn1", answer).await;

    // Caller receives answer
    let received = tokio::time::timeout(Duration::from_millis(100), rx_caller.recv()).await;
    assert!(received.is_ok(), "Caller should receive answer");
}

// Test 3: WebRTC join/leave events

/// Test WebRTC join event.
#[test]
fn test_webrtc_join_event() {
    let room = rid("room1");

    let event = RealtimeEvent::WebRTCJoin {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        actor_id: "usr_user1".to_string(),
        conn_id: "conn1".to_string(),
        username: "testuser".to_string(),
        timestamp: chrono::Utc::now(),
    };

    assert_eq!(event.event_type(), "webrtc_join");
    assert!(event.room_id().is_some());
    assert!(event.user_id().is_none());
    assert!(!event.is_critical(), "WebRTCJoin is not critical");
}

/// Test WebRTC leave event.
#[test]
fn test_webrtc_leave_event() {
    let room = rid("room1");

    let event = RealtimeEvent::WebRTCLeave {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        actor_id: "usr_user1".to_string(),
        conn_id: "conn1".to_string(),
        timestamp: chrono::Utc::now(),
    };

    assert_eq!(event.event_type(), "webrtc_leave");
    assert!(event.room_id().is_some());
    assert!(event.user_id().is_none());
    assert!(!event.is_critical(), "WebRTCLeave is not critical");
}

/// Test WebRTC join/leave broadcast in room.
#[tokio::test]
async fn test_webrtc_join_leave_broadcast() {
    let hub = RoomMessageHub::new();
    let room = rid("room1");
    let user1 = uid("user1");
    let user2 = uid("user2");

    let mut rx1 = hub
        .subscribe(room, user1, ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx2 = hub
        .subscribe(room, user2, ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");

    // User1 joins WebRTC
    let join_event = RealtimeEvent::WebRTCJoin {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        actor_id: "usr_user1".to_string(),
        conn_id: "conn1".to_string(),
        username: "user1".to_string(),
        timestamp: chrono::Utc::now(),
    };
    hub.broadcast(&room, &join_event);

    // Both should receive
    let r1 = tokio::time::timeout(Duration::from_millis(100), rx1.recv()).await;
    let r2 = tokio::time::timeout(Duration::from_millis(100), rx2.recv()).await;
    assert!(r1.is_ok());
    assert!(r2.is_ok());

    // User1 leaves WebRTC
    let leave_event = RealtimeEvent::WebRTCLeave {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        actor_id: "usr_user1".to_string(),
        conn_id: "conn1".to_string(),
        timestamp: chrono::Utc::now(),
    };
    hub.broadcast(&room, &leave_event);

    let r1 = tokio::time::timeout(Duration::from_millis(100), rx1.recv()).await;
    let r2 = tokio::time::timeout(Duration::from_millis(100), rx2.recv()).await;
    assert!(r1.is_ok());
    assert!(r2.is_ok());
}

// Test 4: Connection manager RTC state

/// Test RTC connection tracking in `ConnectionManager`.
#[tokio::test]
async fn test_rtc_connection_tracking() {
    let mgr = ConnectionManager::default();
    let room = rid("room1");
    let user1 = uid("user1");
    let user2 = uid("user2");

    mgr.register("conn1".to_string(), user1).await.unwrap();
    mgr.register("conn2".to_string(), user2).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();
    mgr.join_room("conn2", room).await.unwrap();

    // Initially no RTC connections
    let rtc = mgr.get_rtc_connections(&room);
    assert!(rtc.is_empty());

    // Mark conn1 as RTC joined
    mgr.mark_rtc_joined(&room, &user1, "conn1", true);
    let rtc = mgr.get_rtc_connections(&room);
    assert_eq!(rtc.len(), 1);
    assert_eq!(rtc[0].connection_id, "conn1");

    // Mark conn2 as RTC joined
    mgr.mark_rtc_joined(&room, &user2, "conn2", true);
    let rtc = mgr.get_rtc_connections(&room);
    assert_eq!(rtc.len(), 2);

    // Unmark conn1
    mgr.mark_rtc_joined(&room, &user1, "conn1", false);
    let rtc = mgr.get_rtc_connections(&room);
    assert_eq!(rtc.len(), 1);
    assert_eq!(rtc[0].connection_id, "conn2");
}

// Test 5: Signaling message format validation

/// Test from field parsing (`user_id|conn_id` format).
#[test]
fn test_signaling_from_field_format() {
    let from = "user123|conn456";
    let parts: Vec<&str> = from.splitn(2, '|').collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], "user123");
    assert_eq!(parts[1], "conn456");

    // User ID with colons should still work (using | as separator)
    let from_with_colon = "provider:bilibili:user123|conn789";
    let parts: Vec<&str> = from_with_colon.splitn(2, '|').collect();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], "provider:bilibili:user123");
    assert_eq!(parts[1], "conn789");
}

/// Test to field parsing (`public_user_id:conn_id`).
#[test]
fn test_signaling_to_field_format() {
    let to = "user123:conn456";
    let (user_id, conn_id) = to
        .rsplit_once(':')
        .expect("signaling target must include user_id and conn_id");
    assert_eq!(user_id, "user123");
    assert_eq!(conn_id, "conn456");

    let to_short = "conn789";
    assert!(to_short.rsplit_once(':').is_none());
}

// Test 6: Multi-user signaling

/// Test signaling between multiple users in same room.
#[tokio::test]
async fn test_multi_user_signaling() {
    let hub = RoomMessageHub::new();
    let room = rid("room1");

    // 3 users in same room
    let mut rx1 = hub
        .subscribe(room, uid("user1"), ConnectionId::new("conn1"))
        .await
        .expect("subscribe should succeed");
    let mut rx2 = hub
        .subscribe(room, uid("user2"), ConnectionId::new("conn2"))
        .await
        .expect("subscribe should succeed");
    let mut rx3 = hub
        .subscribe(room, uid("user3"), ConnectionId::new("conn3"))
        .await
        .expect("subscribe should succeed");

    // User1 sends ICE candidate to user2 only
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: room,
        message_type: WebRTCSignalKind::IceCandidate,
        from: "user1|conn1".to_string(),
        to: "user2:conn2".to_string(),
        data: "ICE_DATA".to_string(),
        timestamp: chrono::Utc::now(),
    };
    hub.broadcast_to_connection(&room, "conn2", event).await;

    // Only user2 should receive
    let r2 = tokio::time::timeout(Duration::from_millis(50), rx2.recv()).await;
    assert!(r2.is_ok(), "user2 should receive");

    let r1 = tokio::time::timeout(Duration::from_millis(50), rx1.recv()).await;
    assert!(r1.is_err(), "user1 should not receive (sender)");

    let r3 = tokio::time::timeout(Duration::from_millis(50), rx3.recv()).await;
    assert!(r3.is_err(), "user3 should not receive (not targeted)");
}

// Test 7: Signaling timeout handling (conceptual)

/// Test that signaling events have timestamps for timeout detection.
#[test]
fn test_signaling_event_timestamp() {
    let before = chrono::Utc::now();
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("room1"),
        message_type: WebRTCSignalKind::Offer,
        from: "user1|conn1".to_string(),
        to: "user2:conn2".to_string(),
        data: "SDP".to_string(),
        timestamp: chrono::Utc::now(),
    };
    let after = chrono::Utc::now();

    let ts = event.timestamp();
    assert!(ts >= &before);
    assert!(ts <= &after);

    // Simulate timeout check: if event is older than 30 seconds, it's stale
    let is_stale = chrono::Utc::now().signed_duration_since(*ts) > chrono::Duration::seconds(30);
    assert!(!is_stale, "Fresh event should not be stale");
}

// Test 8: Connection cleanup on WebRTC leave

/// Test that connection is cleaned up when user leaves WebRTC.
#[tokio::test]
async fn test_connection_cleanup_on_webrtc_leave() {
    let mgr = ConnectionManager::default();
    let room = rid("room1");
    let user = uid("user1");

    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    // RTC connection exists
    let rtc = mgr.get_rtc_connections(&room);
    assert_eq!(rtc.len(), 1);

    // Unregister connection (simulating disconnect)
    mgr.unregister("conn1").await;

    // RTC connections should be empty
    let rtc = mgr.get_rtc_connections(&room);
    assert!(rtc.is_empty());
}

// Test 9: WebRTC signaling not critical

/// Test that WebRTC signaling events are not classified as critical.
#[test]
fn test_webrtc_signaling_not_critical() {
    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("room1"),
        message_type: WebRTCSignalKind::IceCandidate,
        from: "user1|conn1".to_string(),
        to: "user2:conn2".to_string(),
        data: "{}".to_string(),
        timestamp: chrono::Utc::now(),
    };

    assert!(
        !event.is_critical(),
        "WebRTC signaling should not be critical"
    );
}

// Test 10: Large SDP payload

/// Test that large SDP payloads can be serialized.
#[test]
fn test_large_sdp_payload() {
    // Simulate a large SDP with many ICE candidates
    let large_sdp = format!(
        "v=0\n{}\n",
        (0..100)
            .map(|i| format!("a=candidate:{i} typical candidate line"))
            .collect::<Vec<_>>()
            .join("\n")
    );

    let event = RealtimeEvent::WebRTCSignaling {
        event_id: synctv_common::snanoid!(16),
        room_id: rid("room1"),
        message_type: WebRTCSignalKind::Offer,
        from: "user1|conn1".to_string(),
        to: "user2:conn2".to_string(),
        data: large_sdp.clone(),
        timestamp: chrono::Utc::now(),
    };

    // Should serialize without issue
    let json = serde_json::to_string(&event).expect("Should serialize large SDP");
    let decoded: RealtimeEvent = serde_json::from_str(&json).expect("Should deserialize");

    if let RealtimeEvent::WebRTCSignaling { data, .. } = decoded {
        assert_eq!(data.len(), large_sdp.len());
    } else {
        panic!("Expected WebRTCSignaling");
    }
}
