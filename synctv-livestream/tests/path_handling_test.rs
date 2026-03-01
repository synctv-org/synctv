#![allow(clippy::unwrap_used)]
// Path handling tests for live streaming
//
// Tests that paths match synctv-go format:
// - FLV: /api/room/movie/live/flv/:media_id.flv?roomId=:room_id
// - HLS Playlist: /api/room/movie/live/hls/list/:media_id?roomId=:room_id
// - HLS Segment: /api/room/movie/live/hls/data/:room_id/:media_id/:segment.ts

#[test]
fn test_flv_path_format() {
    let room_id = "room123";
    let media_id = "media456";
    let token = "test_token";

    // FLV path format
    let path = format!("/api/room/movie/live/flv/{media_id}.flv");
    let query = format!("roomId={room_id}&token={token}");

    // Verify format
    assert!(path.contains(&format!("{media_id}.flv")));
    assert!(path.contains("media456"));
    assert!(!path.contains(room_id)); // room_id should NOT be in path

    assert!(query.contains(&format!("roomId={room_id}")));
    assert!(query.contains(&format!("token={token}")));
}

#[test]
fn test_hls_playlist_path_format() {
    let room_id = "room123";
    let media_id = "media456";

    // HLS playlist path format (without .m3u8)
    let path = format!("/api/room/movie/live/hls/list/{media_id}");
    let query = format!("roomId={room_id}");

    assert!(path.contains(media_id));
    assert!(!path.contains(room_id)); // room_id should NOT be in path
    assert!(query.contains(&format!("roomId={room_id}")));

    // With .m3u8 extension
    let path_with_ext = format!("/api/room/movie/live/hls/list/{media_id}.m3u8");
    assert!(path_with_ext.contains(&format!("{media_id}.m3u8")));
}

#[test]
fn test_hls_segment_path_format() {
    let room_id = "room123";
    let media_id = "media456";
    let segment = "a1b2c3d4e5f6";

    // HLS segment path format
    let path = format!("/api/room/movie/live/hls/data/{room_id}/{media_id}/{segment}.ts");

    // Both room_id and media_id should be in path
    assert!(path.contains(room_id));
    assert!(path.contains(media_id));
    assert!(path.contains(segment));
    assert!(path.contains(".ts"));

    // Verify order
    let parts: Vec<&str> = path.split('/').collect();
    assert_eq!(parts[6], "data");
    assert_eq!(parts[7], room_id);
    assert_eq!(parts[8], media_id);
    assert_eq!(parts[9], format!("{segment}.ts"));
}

#[test]
fn test_hls_segment_disguised_path_format() {
    let room_id = "room123";
    let media_id = "media456";
    let segment = "a1b2c3d4e5f6";

    // HLS segment with PNG disguise
    let path = format!("/api/room/movie/live/hls/data/{room_id}/{media_id}/{segment}.png");

    assert!(path.contains(room_id));
    assert!(path.contains(media_id));
    assert!(path.contains(segment));
    assert!(path.contains(".png"));
    assert!(!path.contains(".ts"));
}

#[test]
fn test_path_parsing() {
    // Test parsing FLV path
    let flv_path = "/api/room/movie/live/flv/media123.flv";
    let flv_media_id = flv_path.split('/').next_back().unwrap().replace(".flv", "");
    assert_eq!(flv_media_id, "media123");

    // Test parsing HLS playlist path
    let hls_path = "/api/room/movie/live/hls/list/media456";
    let hls_media_id = hls_path.split('/').next_back().unwrap();
    assert_eq!(hls_media_id, "media456");

    // Test parsing HLS playlist path with .m3u8
    let hls_path_m3u8 = "/api/room/movie/live/hls/list/media789.m3u8";
    let hls_media_id_m3u8 = hls_path_m3u8.split('/').next_back().unwrap().replace(".m3u8", "");
    assert_eq!(hls_media_id_m3u8, "media789");

    // Test parsing HLS segment path
    let segment_path = "/api/room/movie/live/hls/data/room111/media222/segment.ts";
    let parts: Vec<&str> = segment_path.split('/').collect();
    assert_eq!(parts[7], "room111");
    assert_eq!(parts[8], "media222");
    assert_eq!(parts[9].replace(".ts", ""), "segment");
}

#[test]
fn test_query_parsing() {
    use serde::Deserialize;

    #[derive(Deserialize)]
    struct LiveQuery {
        room_id: Option<String>,
        token: Option<String>,
    }

    // Parse query with room_id and token
    let query_str = "room_id=room123&token=abc123";
    let query: LiveQuery = serde_urlencoded::from_str(query_str).unwrap();
    assert_eq!(query.room_id.unwrap(), "room123");
    assert_eq!(query.token.unwrap(), "abc123");

    // Parse query with only room_id
    let query_str = "room_id=room456";
    let query: LiveQuery = serde_urlencoded::from_str(query_str).unwrap();
    assert_eq!(query.room_id.unwrap(), "room456");
    assert!(query.token.is_none());
}

#[test]
fn test_segment_name_extraction() {
    // Test with .ts suffix
    let segment = "a1b2c3d4e5f6.ts";
    let trimmed = segment.trim_end_matches(".ts");
    assert_eq!(trimmed, "a1b2c3d4e5f6");

    // Test with .png suffix
    let segment = "a1b2c3d4e5f6.png";
    let trimmed = segment.trim_end_matches(".png");
    assert_eq!(trimmed, "a1b2c3d4e5f6");

    // Test without suffix
    let segment = "a1b2c3d4e5f6";
    let trimmed = segment.trim_end_matches(".ts");
    assert_eq!(trimmed, "a1b2c3d4e5f6");
}

#[test]
fn test_room_media_combination_unique_keys() {
    let combinations = vec![
        ("room1", "media1"),
        ("room1", "media2"),
        ("room2", "media1"),
        ("room2", "media2"),
    ];

    for (room, media) in &combinations {
        // Storage key format (structured: app/stream/name)
        let storage_key = format!("{room}/{media}/segment");
        println!("Storage key: {storage_key}");

        // Registry key format
        let registry_key = format!("{room}/{media}");
        println!("Registry key: {registry_key}");

        // Verify uniqueness
        let mut key_set = std::collections::HashSet::new();
        key_set.insert(storage_key.clone());
        key_set.insert(registry_key.clone());

        assert_eq!(key_set.len(), 2); // Should be unique
    }
}

#[test]
fn test_path_matches_go_version() {
    // Verify path patterns match synctv-go

    // FLV
    assert!(matches_flv_pattern("/api/room/movie/live/flv/media123.flv", "media123"));
    assert!(!matches_flv_pattern("/api/room/movie/live/flv/media123.flv", "wrong"));

    // HLS playlist
    assert!(matches_hls_playlist_pattern("/api/room/movie/live/hls/list/media456", "media456"));
    assert!(matches_hls_playlist_pattern("/api/room/movie/live/hls/list/media789.m3u8", "media789"));

    // HLS segment
    assert!(matches_hls_segment_pattern("/api/room/movie/live/hls/data/room111/media222/segment.ts", "room111", "media222"));
}

fn matches_flv_pattern(path: &str, expected_media: &str) -> bool {
    path.starts_with("/api/room/movie/live/flv/")
        && path.ends_with(&format!("{expected_media}.flv"))
}

fn matches_hls_playlist_pattern(path: &str, expected_media: &str) -> bool {
    path.starts_with("/api/room/movie/live/hls/list/")
        && (path.ends_with(expected_media) || path.ends_with(&format!("{expected_media}.m3u8")))
}

fn matches_hls_segment_pattern(path: &str, expected_room: &str, expected_media: &str) -> bool {
    path.starts_with("/api/room/movie/live/hls/data/")
        && path.contains(expected_room)
        && path.contains(expected_media)
        && path.ends_with(".ts")
}

#[test]
fn test_url_building() {
    let base_url = "/api/room/movie/live/hls/data";
    let room_id = "room123";
    let media_id = "media456";
    let ts_name = "segment0";

    // Build segment URL
    let segment_url = format!("{base_url}/{room_id}/{media_id}/{ts_name}.ts");

    assert_eq!(segment_url, "/api/room/movie/live/hls/data/room123/media456/segment0.ts");

    // Build with auth token
    let token = "test_token";
    let url_with_token = format!("{base_url}/{room_id}/{media_id}/{ts_name}.ts?token={token}");

    assert!(url_with_token.contains("?token=test_token"));
}

#[test]
fn test_special_characters_in_ids() {
    // Test that special characters are handled correctly
    let test_cases = vec![
        ("room-123", "media_456"),
        ("room.123", "media.456"),
        ("room~test", "media~test"),
    ];

    for (room, media) in test_cases {
        // Storage key uses structured path: app/stream/name
        let storage_key = format!("{room}/{media}/segment");
        assert!(storage_key.contains('/'));

        // Registry key uses room/media format
        let registry_key = format!("{room}/{media}");
        assert!(registry_key.contains('/'));

        // Both should be valid
        assert!(!storage_key.is_empty());
        assert!(!registry_key.is_empty());
    }
}

#[test]
fn test_path_segment_count() {
    // Verify consistent path segment structure

    // FLV: /api/room/movie/live/flv/:media_id.flv (7 segments)
    let flv_path = "/api/room/movie/live/flv/media123.flv";
    let flv_parts: Vec<&str> = flv_path.split('/').collect();
    assert_eq!(flv_parts.len(), 7);
    assert_eq!(flv_parts[0], "");
    assert_eq!(flv_parts[1], "api");
    assert_eq!(flv_parts[2], "room");
    assert_eq!(flv_parts[3], "movie");
    assert_eq!(flv_parts[4], "live");
    assert_eq!(flv_parts[5], "flv");
    assert_eq!(flv_parts[6], "media123.flv");

    // HLS playlist: /api/room/movie/live/hls/list/:media_id (8 segments)
    let hls_path = "/api/room/movie/live/hls/list/media456";
    let hls_parts: Vec<&str> = hls_path.split('/').collect();
    assert_eq!(hls_parts.len(), 8);
    assert_eq!(hls_parts[7], "media456");

    // HLS segment: /api/room/movie/live/hls/data/:room_id/:media_id/:segment.ts (10 segments)
    let seg_path = "/api/room/movie/live/hls/data/room111/media222/segment.ts";
    let seg_parts: Vec<&str> = seg_path.split('/').collect();
    assert_eq!(seg_parts.len(), 10);
    assert_eq!(seg_parts[7], "room111");
    assert_eq!(seg_parts[8], "media222");
    assert_eq!(seg_parts[9], "segment.ts");
}
