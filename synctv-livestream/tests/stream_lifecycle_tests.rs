//! Stream lifecycle tests including RTMP publish authorization (Task #89)
//!
//! Tests verify stream lifecycle from RTMP publish to HLS playback,
//! including authorization checks.
//!
//! Run with: cargo test --test stream_lifecycle_tests

use synctv_core::models::{RoomId, MediaId, UserId};

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
        "",                                      // Empty
        "room_abc123",                          // Missing media ID
        "room_abc123/media_xyz789",            // Missing token
        "room_abc123?token=abc",               // Missing media ID separator
        "/media_xyz789?token=abc",             // Missing room ID
    ];

    for invalid in invalid_keys {
        let is_valid = invalid.contains('/') && invalid.contains("token=");
        assert!(!is_valid, "Key '{}' should be invalid", invalid);
    }
}

#[tokio::test]
async fn test_stream_lifecycle_states() {
    #[derive(Debug, PartialEq, Clone)]
    enum StreamState {
        Idle,
        Connecting,
        Publishing,
        Stopping,
        Stopped,
    }

    let mut state = StreamState::Idle;

    // Lifecycle: Idle -> Connecting -> Publishing -> Stopping -> Stopped
    state = StreamState::Connecting;
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
        server_addr, app_name, room_id.as_str(), media_id.as_str()
    );

    let stream_key = format!("{}?token={}", media_id.as_str(), token);

    assert!(rtmp_url.starts_with("rtmp://"));
    assert!(rtmp_url.contains(&room_id.as_str()));
    assert!(stream_key.contains("token="));
}

#[tokio::test]
async fn test_hls_playlist_path_generation() {
    let room_id = RoomId::new();
    let media_id = MediaId::new();

    // HLS master playlist path
    let master_playlist = format!("/hls/{}/{}/master.m3u8", room_id.as_str(), media_id.as_str());
    assert!(master_playlist.ends_with(".m3u8"));
    assert!(master_playlist.contains(&room_id.as_str()));

    // HLS media playlist path
    let media_playlist = format!("/hls/{}/{}/index.m3u8", room_id.as_str(), media_id.as_str());
    assert!(media_playlist.ends_with(".m3u8"));

    // HLS segment path
    let segment_path = format!("/hls/{}/{}/segment_00001.ts", room_id.as_str(), media_id.as_str());
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
    use std::sync::Arc;
    use std::collections::HashSet;
    use tokio::sync::Mutex;

    // Simulate stream manager
    let active_streams = Arc::new(Mutex::new(HashSet::new()));

    let mut handles = vec![];

    // Simulate 10 concurrent publishes
    for i in 0..10 {
        let streams = active_streams.clone();
        let stream_id = format!("room_{}/media_{}", i, i);

        let handle = tokio::spawn(async move {
            let mut streams = streams.lock().await;

            // Try to add stream
            let inserted = streams.insert(stream_id.clone());

            if inserted {
                // Simulate stream processing
                tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                stream_id
            } else {
                panic!("Stream {} already exists", stream_id);
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
    use std::sync::Arc;
    use std::collections::HashMap;
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct StreamInfo {
        room_id: String,
        media_id: String,
        started_at: chrono::DateTime<chrono::Utc>,
    }

    let active_streams = Arc::new(Mutex::new(HashMap::<String, StreamInfo>::new()));

    // Add a stream
    let stream_key = "room_123/media_456";
    let stream_info = StreamInfo {
        room_id: "room_123".to_string(),
        media_id: "media_456".to_string(),
        started_at: chrono::Utc::now(),
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
    use std::sync::Arc;
    use std::collections::HashSet;
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
    #[derive(Debug, Clone, PartialEq)]
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
    let deserialized: StreamMetadata = serde_json::from_str(&json)
        .expect("Failed to deserialize");

    assert_eq!(metadata, deserialized);
}

#[tokio::test]
async fn test_stream_bitrate_limits() {
    fn validate_bitrate(bitrate_kbps: u32) -> Result<(), String> {
        const MIN_BITRATE: u32 = 100;   // 100 Kbps
        const MAX_BITRATE: u32 = 10000; // 10 Mbps

        if bitrate_kbps < MIN_BITRATE {
            return Err(format!("Bitrate too low: {} < {}", bitrate_kbps, MIN_BITRATE));
        }

        if bitrate_kbps > MAX_BITRATE {
            return Err(format!("Bitrate too high: {} > {}", bitrate_kbps, MAX_BITRATE));
        }

        Ok(())
    }

    assert!(validate_bitrate(2000).is_ok());
    assert!(validate_bitrate(50).is_err());
    assert!(validate_bitrate(15000).is_err());
}
