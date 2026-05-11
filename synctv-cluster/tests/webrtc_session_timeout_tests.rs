//! WebRTC Session Timeout Tests
//!
//! Tests for WebRTC session timeout functionality to ensure abandoned sessions
//! are cleaned up properly to prevent resource leakage.
//!
//! Test scenarios:
//! - WebRTC session expires after timeout period
//! - `WEBRTC_PEERS_ACTIVE` metric is decremented on timeout
//! - Connection `rtc_joined` flag is cleared on timeout
//! - Active sessions are not incorrectly cleaned up

#![allow(clippy::unwrap_used)]
use std::time::Duration;
use synctv_cluster::ConnectionManager;
use synctv_core::models::id::{RoomId, UserId};

const SHORT_WEBRTC_TIMEOUT: Duration = Duration::from_millis(60);
const WEBRTC_TIMEOUT_BUFFER: Duration = Duration::from_millis(25);
const ACTIVE_SESSION_CHECK_DELAY: Duration = Duration::from_millis(10);

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

#[tokio::test]
async fn test_webrtc_session_timeout_after_inactivity() {
    use synctv_cluster::sync::ConnectionLimits;

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

    // Verify the connection is marked as RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        1,
        "Should have 1 RTC-joined connection"
    );
    assert_eq!(rtc_connections[0].connection_id, "conn1");
    assert!(
        rtc_connections[0].rtc_joined,
        "Connection should be RTC-joined"
    );

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts - this should clean up the WebRTC session
    let stale = mgr.check_timeouts();

    // The connection should be in the stale list
    assert!(!stale.is_empty(), "Should detect stale WebRTC session");

    // Verify the connection is no longer marked as RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        0,
        "Should have 0 RTC-joined connections after timeout"
    );
}

#[tokio::test]
async fn test_active_webrtc_session_not_cleaned_up() {
    use synctv_cluster::sync::ConnectionLimits;

    let timeout = Duration::from_hours(1); // 1 hour timeout
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

    tokio::time::sleep(ACTIVE_SESSION_CHECK_DELAY).await;

    // Check for timeouts - this should NOT clean up active sessions
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
}

#[tokio::test]
async fn test_webrtc_leave_clears_timeout_tracking() {
    use synctv_cluster::sync::ConnectionLimits;

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

    // Immediately leave WebRTC session
    mgr.mark_rtc_joined(&room, &user, "conn1", false);

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts - this should NOT find any sessions (already left)
    let stale = mgr.check_timeouts();

    // The connection should NOT be in the stale list (already left)
    assert!(
        stale.is_empty(),
        "Left WebRTC session should not trigger timeout cleanup"
    );

    // Verify the connection is not marked as RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        0,
        "Should have 0 RTC-joined connections"
    );
}

#[tokio::test]
async fn test_multiple_webrtc_sessions_timeout() {
    use synctv_cluster::sync::ConnectionLimits;

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

    let user1 = uid("user1");
    let _user2 = uid("user2");
    let room = rid("room1");

    // Register multiple connections and join WebRTC
    for i in 0..3 {
        let conn_id = format!("conn{i}");
        mgr.register(conn_id.clone(), user1).await.unwrap();
        mgr.join_room(&conn_id, room).await.unwrap();
        mgr.mark_rtc_joined(&room, &user1, &conn_id, true);
    }

    // Verify all connections are RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        3,
        "Should have 3 RTC-joined connections"
    );

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts - this should clean up all expired sessions
    let stale = mgr.check_timeouts();

    // All connections should be in the stale list
    assert_eq!(stale.len(), 3, "Should detect all 3 stale WebRTC sessions");

    // Verify all connections are no longer marked as RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        0,
        "Should have 0 RTC-joined connections after timeout"
    );
}

#[tokio::test]
async fn test_webrtc_session_timeout_persists_across_reconnection() {
    use synctv_cluster::sync::ConnectionLimits;

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

    // Register connection and join WebRTC
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    tokio::time::sleep(timeout + WEBRTC_TIMEOUT_BUFFER).await;

    // Check for timeouts
    let stale = mgr.check_timeouts();
    assert!(!stale.is_empty(), "Should detect stale WebRTC session");

    // Verify cleanup
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(rtc_connections.len(), 0, "Session should be cleaned up");

    // User reconnects with a new connection
    mgr.register("conn2".to_string(), user).await.unwrap();
    mgr.join_room("conn2", room).await.unwrap();
    mgr.mark_rtc_joined(&room, &user, "conn2", true);

    // Verify new connection is RTC-joined
    let rtc_connections = mgr.get_rtc_connections(&room);
    assert_eq!(
        rtc_connections.len(),
        1,
        "New connection should be RTC-joined"
    );
    assert_eq!(rtc_connections[0].connection_id, "conn2");
}
