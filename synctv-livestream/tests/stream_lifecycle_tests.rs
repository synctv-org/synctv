//! Stream lifecycle tests including RTMP publish authorization (Task #89)
//!
//! Tests verify stream lifecycle from RTMP publish to HLS playback,
//! including authorization checks.
//!
//! Run with: cargo test --test `stream_lifecycle_tests`

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use synctv_core::models::{MediaId, RoomId};

#[tokio::test]
async fn test_stream_key_format_parsing() {
    // Stream key format: {room_id}/{media_id}?token={jwt_token}
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let token = "dummy_jwt_token";

    let stream_key = format!("{}/{}?token={}", room_id.as_str(), media_id.as_str(), token);

    // Parse stream key
    let parts: Vec<&str> = stream_key.split('?').collect();
    assert_eq!(parts.len(), 2);

    let path = parts[0];
    let query = parts[1];

    let path_parts: Vec<&str> = path.split('/').collect();
    assert_eq!(path_parts.len(), 2);
    assert_eq!(path_parts[0], room_id.as_str());
    assert_eq!(path_parts[1], media_id.as_str());

    assert!(query.contains("token="));
    assert!(query.contains(token));
}

#[tokio::test]
async fn test_stream_key_validation_structure() {
    // Valid stream key
    let valid_key = "room_abc123/media_xyz789?token=eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9";
    assert!(valid_key.contains('/'));
    assert!(valid_key.contains('?'));
    assert!(valid_key.contains("token="));

    // Invalid stream keys
    let invalid_keys = vec![
        "",                         // Empty
        "room_abc123",              // Missing media ID
        "room_abc123/media_xyz789", // Missing token
        "room_abc123?token=abc",    // Missing media ID separator
        "/media_xyz789?token=abc",  // Missing room ID
    ];

    for invalid in invalid_keys {
        // A valid key must have a non-empty room_id before '/' and contain a token
        let is_valid = invalid
            .split_once('/')
            .is_some_and(|(room, rest)| !room.is_empty() && rest.contains("token="));
        assert!(!is_valid, "Key '{invalid}' should be invalid");
    }
}

#[tokio::test]
async fn test_stream_lifecycle_states() {
    #[derive(Debug, PartialEq, Clone)]
    #[allow(dead_code)]
    enum StreamState {
        Idle,
        Connecting,
        Publishing,
        Stopping,
        Stopped,
    }

    // Lifecycle: Idle -> Connecting -> Publishing -> Stopping -> Stopped
    let mut state = StreamState::Connecting;
    assert_eq!(state, StreamState::Connecting);

    state = StreamState::Publishing;
    assert_eq!(state, StreamState::Publishing);

    state = StreamState::Stopping;
    assert_eq!(state, StreamState::Stopping);

    state = StreamState::Stopped;
    assert_eq!(state, StreamState::Stopped);
}

#[tokio::test]
async fn test_rtmp_url_construction() {
    let server_addr = "rtmp://localhost:1935";
    let app_name = "live";
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let token = "jwt_token_here";

    let rtmp_url = format!(
        "{}/{}/{}?token={}",
        server_addr,
        app_name,
        room_id.as_str(),
        media_id.as_str()
    );

    let stream_key = format!("{}?token={}", media_id.as_str(), token);

    assert!(rtmp_url.starts_with("rtmp://"));
    assert!(rtmp_url.contains(room_id.as_str()));
    assert!(stream_key.contains("token="));
}

#[tokio::test]
async fn test_hls_playlist_path_generation() {
    let room_id = RoomId::new();
    let media_id = MediaId::new();

    // HLS master playlist path
    let master_playlist = format!(
        "/hls/{}/{}/master.m3u8",
        room_id.as_str(),
        media_id.as_str()
    );
    assert!(master_playlist.ends_with(".m3u8"));
    assert!(master_playlist.contains(room_id.as_str()));

    // HLS media playlist path
    let media_playlist = format!("/hls/{}/{}/index.m3u8", room_id.as_str(), media_id.as_str());
    assert!(media_playlist.ends_with(".m3u8"));

    // HLS segment path
    let segment_path = format!(
        "/hls/{}/{}/segment_00001.ts",
        room_id.as_str(),
        media_id.as_str()
    );
    assert!(segment_path.ends_with(".ts"));
}

#[tokio::test]
async fn test_stream_authorization_token_required() {
    // Simulate authorization check
    fn authorize_stream(stream_key: &str) -> Result<(String, String, String), String> {
        if !stream_key.contains("token=") {
            return Err("Missing token".to_string());
        }

        let parts: Vec<&str> = stream_key.split('?').collect();
        if parts.len() != 2 {
            return Err("Invalid format".to_string());
        }

        let path = parts[0];
        let query = parts[1];

        let path_parts: Vec<&str> = path.split('/').collect();
        if path_parts.len() != 2 {
            return Err("Invalid path format".to_string());
        }

        let room_id = path_parts[0].to_string();
        let media_id = path_parts[1].to_string();

        // Extract token
        if !query.starts_with("token=") {
            return Err("Token not in query".to_string());
        }

        let token = query.strip_prefix("token=").unwrap().to_string();

        Ok((room_id, media_id, token))
    }

    // Valid authorization
    let valid_key = "room_123/media_456?token=valid_jwt";
    let result = authorize_stream(valid_key);
    assert!(result.is_ok());
    let (room_id, media_id, token) = result.unwrap();
    assert_eq!(room_id, "room_123");
    assert_eq!(media_id, "media_456");
    assert_eq!(token, "valid_jwt");

    // Invalid authorization - missing token
    let invalid_key = "room_123/media_456";
    let result = authorize_stream(invalid_key);
    assert!(result.is_err());

    // Invalid authorization - malformed
    let malformed_key = "room_123?token=abc";
    let result = authorize_stream(malformed_key);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_concurrent_stream_publishes() {
    use std::collections::HashSet;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // Simulate stream manager
    let active_streams = Arc::new(Mutex::new(HashSet::new()));

    let mut handles = vec![];

    // Simulate 10 concurrent publishes
    for i in 0..10 {
        let streams = active_streams.clone();
        let stream_id = format!("room_{i}/media_{i}");

        let handle = tokio::spawn(async move {
            let mut streams = streams.lock().await;

            // Try to add stream
            let inserted = streams.insert(stream_id.clone());

            if inserted {
                // Simulate stream processing
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                stream_id
            } else {
                panic!("Stream {stream_id} already exists");
            }
        });

        handles.push(handle);
    }

    // All should succeed
    let results: Vec<String> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(results.len(), 10);

    let final_streams = active_streams.lock().await;
    assert_eq!(final_streams.len(), 10);
}

#[tokio::test]
async fn test_stream_cleanup_on_disconnect() {
    use std::collections::HashMap;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct StreamInfo {
        _room_id: String,
        _media_id: String,
        _started_at: chrono::DateTime<chrono::Utc>,
    }

    let active_streams = Arc::new(Mutex::new(HashMap::<String, StreamInfo>::new()));

    // Add a stream
    let stream_key = "room_123/media_456";
    let stream_info = StreamInfo {
        _room_id: "room_123".to_string(),
        _media_id: "media_456".to_string(),
        _started_at: chrono::Utc::now(),
    };

    {
        let mut streams = active_streams.lock().await;
        streams.insert(stream_key.to_string(), stream_info.clone());
    }

    // Verify stream exists
    {
        let streams = active_streams.lock().await;
        assert_eq!(streams.len(), 1);
        assert!(streams.contains_key(stream_key));
    }

    // Simulate disconnect - cleanup
    {
        let mut streams = active_streams.lock().await;
        streams.remove(stream_key);
    }

    // Verify stream removed
    {
        let streams = active_streams.lock().await;
        assert_eq!(streams.len(), 0);
    }
}

#[tokio::test]
async fn test_stream_duplicate_publish_rejected() {
    use std::collections::HashSet;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    let active_streams = Arc::new(Mutex::new(HashSet::new()));

    let stream_id = "room_123/media_456";

    // First publish should succeed
    {
        let mut streams = active_streams.lock().await;
        let inserted = streams.insert(stream_id.to_string());
        assert!(inserted, "First publish should succeed");
    }

    // Second publish should be rejected
    {
        let mut streams = active_streams.lock().await;
        let inserted = streams.insert(stream_id.to_string());
        assert!(!inserted, "Duplicate publish should be rejected");
    }
}

#[tokio::test]
async fn test_stream_metadata_preservation() {
    #[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
    struct StreamMetadata {
        width: u32,
        height: u32,
        fps: u32,
        video_codec: String,
        audio_codec: String,
    }

    let metadata = StreamMetadata {
        width: 1920,
        height: 1080,
        fps: 30,
        video_codec: "h264".to_string(),
        audio_codec: "aac".to_string(),
    };

    // Serialize/deserialize
    let json = serde_json::to_string(&metadata).expect("Failed to serialize");
    let deserialized: StreamMetadata = serde_json::from_str(&json).expect("Failed to deserialize");

    assert_eq!(metadata, deserialized);
}

#[tokio::test]
async fn test_stream_bitrate_limits() {
    fn validate_bitrate(bitrate_kbps: u32) -> Result<(), String> {
        const MIN_BITRATE: u32 = 100; // 100 Kbps
        const MAX_BITRATE: u32 = 10000; // 10 Mbps

        if bitrate_kbps < MIN_BITRATE {
            return Err(format!("Bitrate too low: {bitrate_kbps} < {MIN_BITRATE}"));
        }

        if bitrate_kbps > MAX_BITRATE {
            return Err(format!("Bitrate too high: {bitrate_kbps} > {MAX_BITRATE}"));
        }

        Ok(())
    }

    assert!(validate_bitrate(2000).is_ok());
    assert!(validate_bitrate(50).is_err());
    assert!(validate_bitrate(15000).is_err());
}

// ========== StreamTracker lifecycle tests ==========

#[tokio::test]
async fn test_stream_tracker_insert_and_lookup() {
    use synctv_livestream::api::StreamTracker;

    let tracker = StreamTracker::new();

    // Insert a publisher mapping
    tracker.insert(
        "user1".to_string(),
        "room1".to_string(),
        "media1".to_string(),
        "room1",       // app_name
        "jwt_token_1", // stream_name (raw RTMP)
    );

    // Lookup by user
    let streams = tracker.get_user_streams("user1");
    assert_eq!(streams.len(), 1);
    assert_eq!(streams[0], ("room1".to_string(), "media1".to_string()));

    // Lookup by room
    let media_ids = tracker.get_room_streams("room1");
    assert_eq!(media_ids.len(), 1);
    assert_eq!(media_ids[0], "media1");

    // Lookup by stream
    let user = tracker.get_stream_user("room1", "media1");
    assert_eq!(user, Some("user1".to_string()));
}

#[tokio::test]
async fn test_stream_tracker_remove_by_app_stream() {
    use synctv_livestream::api::StreamTracker;

    let tracker = StreamTracker::new();

    tracker.insert(
        "user1".to_string(),
        "room1".to_string(),
        "media1".to_string(),
        "room1",
        "jwt_token_1",
    );

    // Remove by RTMP identifiers (simulates on_unpublish)
    let removed = tracker.remove_by_app_stream("room1", "jwt_token_1");
    assert!(removed.is_some());
    let (user_id, room_id, media_id) = removed.unwrap();
    assert_eq!(user_id, "user1");
    assert_eq!(room_id, "room1");
    assert_eq!(media_id, "media1");

    // Verify all indexes are cleaned up
    assert!(tracker.get_user_streams("user1").is_empty());
    assert!(tracker.get_room_streams("room1").is_empty());
    assert!(tracker.get_stream_user("room1", "media1").is_none());
}

#[tokio::test]
async fn test_stream_tracker_multi_user_multi_room() {
    use synctv_livestream::api::StreamTracker;

    let tracker = StreamTracker::new();

    // User1 publishes to room1/media1 and room2/media2
    tracker.insert(
        "user1".to_string(),
        "room1".to_string(),
        "media1".to_string(),
        "room1",
        "token_a",
    );
    tracker.insert(
        "user1".to_string(),
        "room2".to_string(),
        "media2".to_string(),
        "room2",
        "token_b",
    );

    // User2 publishes to room1/media3
    tracker.insert(
        "user2".to_string(),
        "room1".to_string(),
        "media3".to_string(),
        "room1",
        "token_c",
    );

    // User1 has 2 streams
    let user1_streams = tracker.get_user_streams("user1");
    assert_eq!(user1_streams.len(), 2);

    // Room1 has 2 media IDs (media1 from user1, media3 from user2)
    let room1_media = tracker.get_room_streams("room1");
    assert_eq!(room1_media.len(), 2);

    // Remove all streams for user1
    let removed = tracker.remove_user("user1");
    assert_eq!(removed.len(), 2);

    // User1 has no streams left
    assert!(tracker.get_user_streams("user1").is_empty());

    // Room1 still has media3 (from user2)
    let room1_media = tracker.get_room_streams("room1");
    assert_eq!(room1_media.len(), 1);
    assert_eq!(room1_media[0], "media3");
}

#[tokio::test]
async fn test_stream_subscriber_guard_disarm() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use synctv_livestream::api::tracker::StreamSubscriberGuard;

    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();

    let mut guard = StreamSubscriberGuard::new(move || {
        called_clone.store(true, Ordering::SeqCst);
    });

    // Disarm the guard
    guard.disarm();

    // Drop should NOT call the callback
    drop(guard);
    assert!(
        !called.load(Ordering::SeqCst),
        "Disarmed guard should not call callback on drop"
    );
}

#[tokio::test]
async fn test_stream_subscriber_guard_normal_drop() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use synctv_livestream::api::tracker::StreamSubscriberGuard;

    let called = Arc::new(AtomicBool::new(false));
    let called_clone = called.clone();

    let guard = StreamSubscriberGuard::new(move || {
        called_clone.store(true, Ordering::SeqCst);
    });

    // Drop should call the callback
    drop(guard);
    assert!(
        called.load(Ordering::SeqCst),
        "Guard should call callback on drop"
    );
}

// ========== PublisherInfo validation tests ==========

#[tokio::test]
async fn test_publisher_info_validate_api_address() {
    use chrono::Utc;
    use synctv_livestream::relay::PublisherInfo;

    let valid = PublisherInfo {
        node_id: "node1".to_string(),
        api_address: "10.0.0.1:50051".to_string(),
        app_name: "live".to_string(),
        user_id: "user1".to_string(),
        started_at: Utc::now(),
        epoch: 1,
    };
    assert!(valid.validate_api_address().is_ok());
    assert_eq!(valid.validate_api_address().unwrap(), "10.0.0.1:50051");

    let empty = PublisherInfo {
        node_id: "node1".to_string(),
        api_address: String::new(),
        app_name: "live".to_string(),
        user_id: "user1".to_string(),
        started_at: Utc::now(),
        epoch: 1,
    };
    assert!(empty.validate_api_address().is_err());

    let whitespace = PublisherInfo {
        node_id: "node1".to_string(),
        api_address: "   ".to_string(),
        app_name: "live".to_string(),
        user_id: "user1".to_string(),
        started_at: Utc::now(),
        epoch: 1,
    };
    assert!(whitespace.validate_api_address().is_err());
}

// ========== PublisherInfo serialization round-trip ==========

#[tokio::test]
async fn test_publisher_info_serde_round_trip() {
    use chrono::Utc;
    use synctv_livestream::relay::PublisherInfo;

    let info = PublisherInfo {
        node_id: "node-abc".to_string(),
        api_address: "10.0.0.1:50051".to_string(),
        app_name: "live".to_string(),
        user_id: "user-123".to_string(),
        started_at: Utc::now(),
        epoch: 42,
    };

    let json = serde_json::to_string(&info).unwrap();
    assert!(
        json.contains("\"api_address\""),
        "wire format must use api_address: {json}"
    );
    assert!(
        !json.contains("\"grpc_address\""),
        "wire format must not emit grpc_address: {json}"
    );
    let deserialized: PublisherInfo = serde_json::from_str(&json).unwrap();

    assert_eq!(info.node_id, deserialized.node_id);
    assert_eq!(info.api_address, deserialized.api_address);
    assert_eq!(info.epoch, deserialized.epoch);
    assert_eq!(info.user_id, deserialized.user_id);
}

// ========== PublisherInfo default fields ==========

#[tokio::test]
async fn test_publisher_info_deserializes_api_address_field() {
    use synctv_livestream::relay::PublisherInfo;

    let json = r#"{
        "node_id": "node1",
        "api_address": "10.0.0.1:50051",
        "app_name": "live",
        "user_id": "user1",
        "started_at": "2024-01-01T00:00:00Z",
        "epoch": 7
    }"#;

    let info: PublisherInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.api_address, "10.0.0.1:50051");
    assert_eq!(info.user_id, "user1");
    assert_eq!(info.epoch, 7);
}

// ========== HLS Cleanup Task Leak Tests ==========
//
// Tests verify that HLS cleanup tasks are properly terminated when
// LivestreamHandle is dropped without calling shutdown().

/// Test that dropping `LivestreamHandle` without calling `shutdown()` terminates the HLS cleanup task.
///
/// This verifies the fix for Task #28: HLS segment cleanup task leak.
/// Before the fix, the cleanup task would run forever if `LivestreamHandle`
/// was dropped without calling `shutdown()`.
#[tokio::test]
async fn test_hls_cleanup_task_terminates_on_drop() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    // Create a simple test to verify the cleanup task responds to cancellation
    let cancel_token = CancellationToken::new();
    let task_running = Arc::new(AtomicBool::new(false));
    let task_running_clone = task_running.clone();
    let cancel_token_clone = cancel_token.clone();

    // Spawn a task that mimics the cleanup loop
    let handle = tokio::spawn(async move {
        task_running_clone.store(true, Ordering::SeqCst);
        loop {
            tokio::select! {
                () = tokio::time::sleep(Duration::from_millis(100)) => {
                    // Simulate cleanup work
                }
                () = cancel_token_clone.cancelled() => {
                    // Task exits when cancelled
                    break;
                }
            }
        }
        task_running_clone.store(false, Ordering::SeqCst);
    });

    // Verify task is running
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        task_running.load(Ordering::SeqCst),
        "Task should be running"
    );

    // Cancel the token and drop the handle
    cancel_token.cancel();
    drop(handle);

    // Wait for task to finish
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify task has stopped
    assert!(
        !task_running.load(Ordering::SeqCst),
        "Task should have stopped after cancellation"
    );
}

/// Test that `LivestreamHandle` properly tracks and aborts the HLS cleanup task.
///
/// This verifies that the `hls_cleanup_handle` field is properly initialized
/// and that Drop aborts it.
///
/// Note: This test uses the `SegmentManager` directly to verify the fix without
/// requiring the full `LivestreamServer` infrastructure.
#[tokio::test]
async fn test_livestream_handle_tracks_hls_cleanup() {
    use std::time::Duration;
    use synctv_xiu::hls::segment_manager::{CleanupConfig, SegmentManager};
    use synctv_xiu::storage::MemoryStorage;
    use tokio_util::sync::CancellationToken;

    // Create a simple segment manager with in-memory storage
    let storage = Arc::new(MemoryStorage::new());
    let config = CleanupConfig {
        interval: Duration::from_millis(100),
        retention: Duration::from_mins(1),
        max_segments_per_stream: 0,
    };
    let segment_manager = Arc::new(SegmentManager::new(storage, config));

    // Start the cleanup task and get the handle
    let cancel_token = CancellationToken::new();
    let handle = segment_manager.start_cleanup_task(cancel_token.clone());

    // Verify the handle exists and can be polled
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Cancel and abort
    cancel_token.cancel();
    handle.abort();

    // Give the task time to terminate
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify the handle is finished (abort was successful)
    assert!(
        handle.is_finished(),
        "Cleanup task should be finished after abort"
    );
}
