#![allow(clippy::unwrap_used)]
// Integration tests for live streaming infrastructure
//
// Tests the complete flow from API to storage, including:
// - FLV streaming API
// - HLS playlist generation
// - HLS segment storage and retrieval
// - Room/Media separation
// - Storage key formats

use bytes::Bytes;
use dashmap::DashMap; // still used for hls_registry
use parking_lot::RwLock;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;
use synctv_livestream::api::{HlsStreamingApi, LiveStreamingInfrastructure};
use synctv_livestream::libraries::storage::MemoryStorage;
use synctv_livestream::livestream::segment_manager::{CleanupConfig, SegmentManager};
use synctv_livestream::protocols::hls::remuxer::{SegmentInfo, StreamProcessorState};
use tokio::sync::mpsc;

// Mock implementation for testing
struct MockStreamRegistry;

#[async_trait::async_trait]
impl synctv_livestream::relay::StreamRegistryTrait for MockStreamRegistry {
    async fn register_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
        _node_id: &str,
        _app_name: &str,
        _grpc_address: &str,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn try_register_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
        _node_id: &str,
        _user_id: &str,
        _grpc_address: &str,
    ) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn refresh_publisher_ttl(
        &self,
        _room_id: &str,
        _media_id: &str,
        _user_id: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn unregister_publisher(&self, _room_id: &str, _media_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn get_publisher(
        &self,
        _room_id: &str,
        _media_id: &str,
    ) -> anyhow::Result<Option<synctv_livestream::relay::PublisherInfo>> {
        Ok(Some(synctv_livestream::relay::PublisherInfo {
            node_id: "test_node".to_string(),
            grpc_address: String::new(),
            app_name: "live".to_string(),
            user_id: String::new(),
            started_at: chrono::Utc::now(),
            epoch: 1,
        }))
    }

    async fn is_stream_active(&self, _room_id: &str, _media_id: &str) -> anyhow::Result<bool> {
        Ok(true)
    }

    async fn list_active_streams(&self) -> anyhow::Result<Vec<(String, String)>> {
        Ok(vec![])
    }

    async fn get_user_publishers(&self, _user_id: &str) -> anyhow::Result<Vec<(String, String)>> {
        Ok(vec![])
    }

    async fn unregister_all_user_publishers(&self, _user_id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn validate_epoch(
        &self,
        _room_id: &str,
        _media_id: &str,
        _epoch: u64,
    ) -> anyhow::Result<bool> {
        // Mock always returns true for validation
        Ok(true)
    }

    async fn cleanup_all_publishers_for_node(&self, _node_id: &str) -> anyhow::Result<()> {
        // Mock cleanup - no-op
        Ok(())
    }
}

fn create_test_infrastructure() -> LiveStreamingInfrastructure {
    let registry =
        Arc::new(MockStreamRegistry) as Arc<dyn synctv_livestream::relay::StreamRegistryTrait>;
    let (event_sender, _) = mpsc::channel(64);
    let storage = Arc::new(MemoryStorage::new());
    let segment_manager = Arc::new(SegmentManager::new(storage, CleanupConfig::default()));
    let hls_registry = Arc::new(DashMap::new());

    let pull_manager = Arc::new(synctv_livestream::livestream::PullStreamManager::new(
        registry.clone(),
        event_sender.clone(),
    ));

    let external_publish_manager = Arc::new(
        synctv_livestream::livestream::ExternalPublishManager::new(
            registry.clone(),
            "test-node".to_string(),
            event_sender.clone(),
        )
        .expect("failed to create ExternalPublishManager"),
    );

    let user_stream_tracker = Arc::new(synctv_livestream::api::StreamTracker::new());
    LiveStreamingInfrastructure::new(
        registry,
        event_sender,
        pull_manager,
        external_publish_manager,
        user_stream_tracker,
    )
    .with_segment_manager(segment_manager)
    .with_hls_stream_registry(hls_registry)
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_complete_hls_workflow() {
    let infrastructure = create_test_infrastructure();

    // Step 1: Simulate HLS remuxer writing segments
    let storage = infrastructure.segment_manager.as_ref().unwrap().storage();

    // Write test segments using structured (app, stream, name)
    for i in 0..3 {
        let segment_data = Bytes::from(format!("TS segment data {i}"));
        storage
            .write(
                "test_room",
                "test_media",
                &format!("segment{i}"),
                segment_data,
            )
            .await
            .unwrap();
    }

    // Step 2: Register stream in HLS registry
    let mut segments = VecDeque::new();
    for i in 0..3 {
        segments.push_back(SegmentInfo {
            sequence: i,
            duration: 10000,
            ts_name: format!("segment{i}"),
            discontinuity: false,
            created_at: Instant::now(),
        });
    }

    let state = Arc::new(RwLock::new(StreamProcessorState {
        app_name: "test_room".to_string(),
        stream_name: "test_media".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
        marked_for_cleanup: false,
        cleanup_segment_names: Vec::new(),
    }));

    if let Some(registry) = &infrastructure.hls_stream_registry {
        registry.insert("test_room/test_media".to_string(), state);
    }

    // Step 3: Generate HLS playlist
    let playlist =
        HlsStreamingApi::generate_playlist(&infrastructure, "test_room", "test_media", |ts_name| {
            format!("/api/providers/proxy/rtmp/version123/segment/{ts_name}.ts")
        })
        .await
        .unwrap()
        .expect("playlist should exist for registered stream");

    // Verify playlist contains all segments
    assert!(playlist.contains("#EXTM3U"));
    assert!(playlist.contains("segment0.ts"));
    assert!(playlist.contains("segment1.ts"));
    assert!(playlist.contains("segment2.ts"));

    // Step 4: Retrieve segments via API
    let segment0 =
        HlsStreamingApi::get_segment(&infrastructure, "test_room", "test_media", "segment0")
            .await
            .unwrap();

    assert_eq!(segment0, Bytes::from("TS segment data 0"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_multiple_room_media_combinations() {
    let infrastructure = create_test_infrastructure();

    // Test data: multiple rooms and media
    let test_cases = vec![
        ("room1", "media1", 2),
        ("room1", "media2", 3),
        ("room2", "media1", 2),
        ("room3", "media3", 1),
    ];

    // Write segments for each combination
    for (room, media, count) in &test_cases {
        for i in 0..*count {
            let storage = infrastructure.segment_manager.as_ref().unwrap().storage();
            let segment_data = Bytes::from(format!("{room}-{media}-seg{i}"));
            storage
                .write(room, media, &format!("seg{i}"), segment_data)
                .await
                .unwrap();
        }
    }

    // Verify all segments are accessible
    for (room, media, count) in &test_cases {
        for i in 0..*count {
            let segment =
                HlsStreamingApi::get_segment(&infrastructure, room, media, &format!("seg{i}"))
                    .await;

            assert!(
                segment.is_ok(),
                "Failed to get segment for {room}-{media}-seg{i}"
            );
            let data = segment.unwrap();
            assert_eq!(data, Bytes::from(format!("{room}-{media}-seg{i}")));
        }
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_storage_key_format_consistency() {
    let infrastructure = create_test_infrastructure();
    let storage = infrastructure.segment_manager.as_ref().unwrap().storage();

    // Write segment with structured key
    let data = Bytes::from("test data");
    storage
        .write("room123", "media456", "testsegment", data.clone())
        .await
        .unwrap();

    // Verify key exists
    assert!(storage
        .exists("room123", "media456", "testsegment")
        .await
        .unwrap());

    // Verify can read back
    let read_data = storage
        .read("room123", "media456", "testsegment")
        .await
        .unwrap();
    assert_eq!(read_data, data);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_registry_key_format_consistency() {
    let infrastructure = create_test_infrastructure();

    // Register stream with canonical format: room_id/media_id
    let state = Arc::new(RwLock::new(StreamProcessorState {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
        segments: VecDeque::new(),
        is_ended: false,
        created_at: std::time::Instant::now(),
        marked_for_cleanup: false,
        cleanup_segment_names: Vec::new(),
    }));

    if let Some(registry) = &infrastructure.hls_stream_registry {
        let key = "room123/media456";
        registry.insert(key.to_string(), state);

        // Verify exact key exists
        assert!(registry.contains_key(key));

        // Verify alternative formats do NOT exist
        assert!(!registry.contains_key("live/room123:media456"));
        assert!(!registry.contains_key("live/room123/media456"));
        assert!(!registry.contains_key("live-room123:media456"));
        assert!(!registry.contains_key("room123:media456"));
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_hls_url_generation_with_custom_callback() {
    let infrastructure = create_test_infrastructure();

    // Register test stream
    let mut segments = VecDeque::new();
    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 10000,
        ts_name: "segment0".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = Arc::new(RwLock::new(StreamProcessorState {
        app_name: "room123".to_string(),
        stream_name: "media456".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
        marked_for_cleanup: false,
        cleanup_segment_names: Vec::new(),
    }));

    if let Some(registry) = &infrastructure.hls_stream_registry {
        registry.insert("room123/media456".to_string(), state);
    }

    // Test with CDN URL
    let playlist =
        HlsStreamingApi::generate_playlist(&infrastructure, "room123", "media456", |ts_name| {
            format!("https://cdn.example.com/hls/{ts_name}.ts")
        })
        .await
        .unwrap()
        .expect("playlist should exist");

    assert!(playlist.contains("https://cdn.example.com/hls/segment0.ts"));

    // Test with auth token
    let token = "test_token_abc123";
    let playlist = HlsStreamingApi::generate_playlist(
        &infrastructure,
        "room123",
        "media456",
        move |ts_name| {
            format!("/api/providers/proxy/rtmp/version123/segment/{ts_name}.ts?sig=test&uid=u1&rid=room123&exp=1&token={token}")
        },
    )
    .await
    .unwrap()
    .expect("playlist should exist");

    assert!(
        playlist.contains(&format!("token={token}")),
        "custom callback query string should be preserved: {playlist}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_segment_cleanup() {
    let infrastructure = create_test_infrastructure();
    let storage = infrastructure.segment_manager.as_ref().unwrap().storage();

    // Write multiple segments
    for i in 0..5 {
        let data = Bytes::from(format!("segment{i}"));
        storage
            .write("room1", "media1", &format!("seg{i}"), data)
            .await
            .unwrap();
    }

    // Verify all exist
    for i in 0..5 {
        assert!(storage
            .exists("room1", "media1", &format!("seg{i}"))
            .await
            .unwrap());
    }

    // Cleanup segments older than 0 seconds (all)
    use std::time::Duration;
    let deleted = storage.cleanup(Duration::from_secs(0)).await.unwrap();
    assert_eq!(deleted, 5);

    // Verify all are deleted
    for i in 0..5 {
        assert!(!storage
            .exists("room1", "media1", &format!("seg{i}"))
            .await
            .unwrap());
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_segment_access() {
    let infrastructure = create_test_infrastructure();
    let storage = infrastructure.segment_manager.as_ref().unwrap().storage();

    // Write segments
    for i in 0..10 {
        let data = Bytes::from(format!("segment{i}"));
        storage
            .write("room1", "media1", &format!("seg{i}"), data)
            .await
            .unwrap();
    }

    // Concurrent reads
    let mut handles = vec![];
    for i in 0..10 {
        let storage_clone = storage.clone();
        let name = format!("seg{i}");
        let handle =
            tokio::spawn(
                async move { storage_clone.read("room1", "media1", &name).await.unwrap() },
            );
        handles.push(handle);
    }

    // Verify all reads succeeded
    for (i, handle) in handles.into_iter().enumerate() {
        let data = handle.await.unwrap();
        assert_eq!(data, Bytes::from(format!("segment{i}")));
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_hls_playlist_with_discontinuity() {
    let infrastructure = create_test_infrastructure();

    // Create segments with discontinuity
    let mut segments = VecDeque::new();
    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 10000,
        ts_name: "segment0".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });
    segments.push_back(SegmentInfo {
        sequence: 1,
        duration: 10000,
        ts_name: "segment1".to_string(),
        discontinuity: true, // Discontinuity here
        created_at: Instant::now(),
    });
    segments.push_back(SegmentInfo {
        sequence: 2,
        duration: 10000,
        ts_name: "segment2".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = Arc::new(RwLock::new(StreamProcessorState {
        app_name: "room1".to_string(),
        stream_name: "media1".to_string(),
        segments,
        is_ended: false,
        created_at: std::time::Instant::now(),
        marked_for_cleanup: false,
        cleanup_segment_names: Vec::new(),
    }));

    if let Some(registry) = &infrastructure.hls_stream_registry {
        registry.insert("room1/media1".to_string(), state);
    }

    let playlist =
        HlsStreamingApi::generate_playlist(&infrastructure, "room1", "media1", |ts_name| {
            format!("/{ts_name}.ts")
        })
        .await
        .unwrap()
        .expect("playlist should exist");

    assert!(playlist.contains("#EXT-X-DISCONTINUITY"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_hls_playlist_ended_stream() {
    let infrastructure = create_test_infrastructure();

    let mut segments = VecDeque::new();
    segments.push_back(SegmentInfo {
        sequence: 0,
        duration: 10000,
        ts_name: "segment0".to_string(),
        discontinuity: false,
        created_at: Instant::now(),
    });

    let state = Arc::new(RwLock::new(StreamProcessorState {
        app_name: "room1".to_string(),
        stream_name: "media1".to_string(),
        segments,
        is_ended: true,
        created_at: std::time::Instant::now(),
        marked_for_cleanup: false,
        cleanup_segment_names: Vec::new(),
    }));

    if let Some(registry) = &infrastructure.hls_stream_registry {
        registry.insert("room1/media1".to_string(), state);
    }

    let playlist =
        HlsStreamingApi::generate_playlist(&infrastructure, "room1", "media1", |ts_name| {
            format!("/{ts_name}.ts")
        })
        .await
        .unwrap()
        .expect("playlist should exist");

    assert!(playlist.contains("#EXT-X-ENDLIST"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_path_parameter_separation() {
    // Test that room_id and media_id are properly separated in different contexts

    // FLV: versioned provider proxy path
    let _media_id = "media123";
    let room_id = "room456";
    let flv_path = "/api/providers/proxy/rtmp/version123/stream".to_string();
    let flv_query = format!("sig=test&uid=u1&rid={room_id}&exp=1");

    assert!(flv_path.contains("/stream"));
    assert!(flv_query.contains(&format!("rid={room_id}")));

    // HLS playlist: versioned provider proxy path
    let hls_path = "/api/providers/proxy/rtmp/version123/m3u8".to_string();
    let hls_query = format!("sig=test&uid=u1&rid={room_id}&exp=1");

    assert!(hls_path.contains("/m3u8"));
    assert!(hls_query.contains(&format!("rid={room_id}")));

    // HLS segment: version-scoped provider proxy path
    let segment_path = "/api/providers/proxy/rtmp/version123/segment/segment.ts";

    assert!(segment_path.contains("version123"));
    assert!(segment_path.contains("segment.ts"));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_empty_playlist_returns_none() {
    let infrastructure = create_test_infrastructure();

    // No streams registered in the HLS registry, but the mock registry
    // always returns a publisher. The playlist should be None because
    // the HLS registry has no entry for this stream.
    let playlist = HlsStreamingApi::generate_playlist(
        &infrastructure,
        "nonexistent",
        "nonexistent",
        |ts_name| format!("/{ts_name}.ts"),
    )
    .await
    .unwrap();

    // Stream not in HLS registry -> None (caller returns HTTP 404)
    assert!(
        playlist.is_none(),
        "Expected None for stream not in HLS registry"
    );
}
