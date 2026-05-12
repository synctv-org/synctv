//! WebRTC Session Cleanup Race Condition Tests
//!
//! Tests for race condition between WebRTC session timeout cleanup and
//! explicit leave/disconnect. The issue: `peer_count` can become negative
//! due to race between `remove_peer` and `cleanup_task`.
//!
//! Test scenarios:
//! - Session timeout clears RTC state
//! - Connection cleanup checks RTC state to prevent double-decrement
//! - Idempotent RTC state transitions

#![allow(clippy::unwrap_used)]
use std::time::Duration;
use synctv_core::models::id::{RoomId, UserId};
use synctv_realtime::ConnectionManager;

const SHORT_WEBRTC_TIMEOUT: Duration = Duration::from_millis(60);
const WEBRTC_TIMEOUT_BUFFER: Duration = Duration::from_millis(25);

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

// Test 1: Timeout clears RTC state to prevent race with cleanup

#[tokio::test]
async fn test_timeout_clears_rtc_state() {
    use synctv_realtime::sync::ConnectionLimits;

    let timeout = SHORT_WEBRTC_TIMEOUT;
    let limits = ConnectionLimits {
        max_per_user: 10,
        max_per_room: 10,
        max_total: 100,
        idle_timeout: Duration::from_mins(5),
        max_duration: Duration::from_hours(24),
        webrtc_session_timeout: timeout,
    };
    let mgr = ConnectionManager::new(limits);

    let user = uid("user1");
    let room = rid("room1");

    // Register connection and join room
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();

    // Join WebRTC session
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    // Verify the connection is RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        1,
        "Should have 1 RTC-joined connection"
    );

    // Verify the connection info shows rtc_joined=true
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some(), "Connection should exist");
    assert!(conn.unwrap().rtc_joined, "Connection should be RTC-joined");

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts - this simulates the cleanup task
    let stale = mgr.check_timeouts();

    // The connection should be in the stale list
    assert!(!stale.is_empty(), "Should detect stale WebRTC session");

    // The connection manager should have marked it as not RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        0,
        "Should have 0 RTC-joined connections after timeout"
    );

    // Verify the connection info now shows rtc_joined=false so the messaging
    // layer can avoid double-decrementing WebRTC state.
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some(), "Connection should still exist");
    assert!(
        !conn.unwrap().rtc_joined,
        "Connection should not be RTC-joined after timeout"
    );

    // Clean up
    mgr.unregister("conn1").await;
}

// Test 2: Multiple concurrent timeouts

#[tokio::test]
async fn test_multiple_concurrent_timeouts() {
    use synctv_realtime::sync::ConnectionLimits;

    let timeout = SHORT_WEBRTC_TIMEOUT;
    let limits = ConnectionLimits {
        max_per_user: 10,
        max_per_room: 10,
        max_total: 100,
        idle_timeout: Duration::from_mins(5),
        max_duration: Duration::from_hours(24),
        webrtc_session_timeout: timeout,
    };
    let mgr = ConnectionManager::new(limits);

    let user = uid("user1");
    let room = rid("room1");

    // Register multiple connections and join WebRTC
    for i in 0..5 {
        let conn_id = format!("conn{i}");
        mgr.register(conn_id.clone(), user).await.unwrap();
        mgr.join_room(&conn_id, room).await.unwrap();
        mgr.mark_rtc_joined(&room, &user, &conn_id, true);
    }

    // Verify all connections are RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        5,
        "Should have 5 RTC-joined connections"
    );

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts - this should clean up all expired sessions
    let stale = mgr.check_timeouts();

    // All connections should be in the stale list
    assert_eq!(stale.len(), 5, "Should detect all 5 stale WebRTC sessions");

    // Verify all connections are no longer marked as RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        0,
        "Should have 0 RTC-joined connections after timeout"
    );

    // Verify each connection individually has rtc_joined=false
    for i in 0..5 {
        let conn_id = format!("conn{i}");
        let conn = mgr.get_connection(&conn_id);
        assert!(conn.is_some(), "Connection {conn_id} should exist");
        assert!(
            !conn.unwrap().rtc_joined,
            "Connection {conn_id} should not be RTC-joined"
        );
    }
}

// Test 3: Timeout does not affect active sessions

#[tokio::test]
async fn test_timeout_does_not_affect_active_sessions() {
    use synctv_realtime::sync::ConnectionLimits;

    let timeout = SHORT_WEBRTC_TIMEOUT;
    let limits = ConnectionLimits {
        max_per_user: 10,
        max_per_room: 10,
        max_total: 100,
        idle_timeout: Duration::from_mins(5),
        max_duration: Duration::from_hours(24),
        webrtc_session_timeout: timeout,
    };
    let mgr = ConnectionManager::new(limits);

    let user = uid("user1");
    let room = rid("room1");

    // Register connection and join room
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();

    // Join WebRTC session
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    // Immediately check for timeouts (before timeout expires)
    let stale = mgr.check_timeouts();

    // The connection should NOT be in the stale list
    assert!(
        stale.is_empty(),
        "Active WebRTC session should not be cleaned up"
    );

    // Verify the connection is still marked as RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        1,
        "Should still have 1 RTC-joined connection"
    );

    // Verify the connection info shows rtc_joined=true
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some(), "Connection should exist");
    assert!(
        conn.unwrap().rtc_joined,
        "Connection should still be RTC-joined"
    );
}

// Test 4: Explicit leave after timeout is idempotent

#[tokio::test]
async fn test_explicit_leave_after_timeout_is_idempotent() {
    use synctv_realtime::sync::ConnectionLimits;

    let timeout = SHORT_WEBRTC_TIMEOUT;
    let limits = ConnectionLimits {
        max_per_user: 10,
        max_per_room: 10,
        max_total: 100,
        idle_timeout: Duration::from_mins(5),
        max_duration: Duration::from_hours(24),
        webrtc_session_timeout: timeout,
    };
    let mgr = ConnectionManager::new(limits);

    let user = uid("user1");
    let room = rid("room1");

    // Register connection and join room
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();

    // Join WebRTC session
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts
    let stale = mgr.check_timeouts();
    assert!(!stale.is_empty(), "Should detect stale WebRTC session");

    // Verify the connection is no longer marked as RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        0,
        "Should have 0 RTC-joined connections after timeout"
    );

    // Now call mark_rtc_joined(false) again (simulating explicit leave)
    // This should be idempotent - calling it twice should not cause issues
    mgr.mark_rtc_joined(&room, &user, "conn1", false);

    // Verify still not RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        0,
        "Should still have 0 RTC-joined connections"
    );

    // Verify the connection info shows rtc_joined=false
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some(), "Connection should still exist");
    assert!(
        !conn.unwrap().rtc_joined,
        "Connection should not be RTC-joined"
    );
}

// Test 5: Connection info accurately reflects RTC state after timeout

#[tokio::test]
async fn test_connection_info_accurate_after_timeout() {
    use synctv_realtime::sync::ConnectionLimits;

    let timeout = SHORT_WEBRTC_TIMEOUT;
    let limits = ConnectionLimits {
        max_per_user: 10,
        max_per_room: 10,
        max_total: 100,
        idle_timeout: Duration::from_mins(5),
        max_duration: Duration::from_hours(24),
        webrtc_session_timeout: timeout,
    };
    let mgr = ConnectionManager::new(limits);

    let user = uid("user1");
    let room = rid("room1");

    // Register connection and join room
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();

    // Join WebRTC session
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    // Get connection info and verify RTC state
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some(), "Connection should exist");
    let conn_info = conn.unwrap();
    assert!(conn_info.rtc_joined, "Connection should be RTC-joined");
    assert!(
        conn_info.rtc_joined_at.is_some(),
        "RTC joined timestamp should be set"
    );

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts
    let stale = mgr.check_timeouts();
    assert!(!stale.is_empty(), "Should detect stale WebRTC session");

    // Get connection info again and verify RTC state is cleared
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some(), "Connection should still exist");
    let conn_info = conn.unwrap();
    assert!(
        !conn_info.rtc_joined,
        "Connection should not be RTC-joined after timeout"
    );
    assert!(
        conn_info.rtc_joined_at.is_none(),
        "RTC joined timestamp should be cleared"
    );
}
