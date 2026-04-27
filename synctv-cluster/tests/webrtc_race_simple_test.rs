//! Simple WebRTC race condition test

#![allow(clippy::unwrap_used)]
use std::time::Duration;
use synctv_cluster::ConnectionManager;
use synctv_core::models::id::{RoomId, UserId};

fn stable_test_id(s: &str) -> i64 {
    s.bytes().fold(0_i64, |acc, byte| {
        (acc * 131 + i64::from(byte)) % 900_000_000
    }) + 1
}

fn uid(s: &str) -> UserId {
    UserId::from(stable_test_id(s))
}

fn rid(s: &str) -> RoomId {
    RoomId::from(stable_test_id(s))
}

#[tokio::test]
async fn test_simple_rtc_state_check() {
    use synctv_cluster::sync::ConnectionLimits;

    let limits = ConnectionLimits {
        max_per_user: 10,
        max_per_room: 10,
        max_total: 100,
        idle_timeout: Duration::from_mins(5),
        max_duration: Duration::from_hours(24),
        webrtc_session_timeout: Duration::from_hours(1),
    };
    let mgr = ConnectionManager::new(limits);

    let user = uid("user1");
    let room = rid("room1");

    // Register and join
    mgr.register("conn1".to_string(), user).await.unwrap();
    mgr.join_room("conn1", room).await.unwrap();

    // Join WebRTC
    mgr.mark_rtc_joined(&room, &user, "conn1", true);

    // Verify RTC state
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some());
    assert!(conn.unwrap().rtc_joined);

    // Leave WebRTC
    mgr.mark_rtc_joined(&room, &user, "conn1", false);

    // Verify RTC state cleared
    let conn = mgr.get_connection("conn1");
    assert!(conn.is_some());
    assert!(!conn.unwrap().rtc_joined);

    // Cleanup
    mgr.unregister("conn1").await;

    println!("Test passed!");
}
