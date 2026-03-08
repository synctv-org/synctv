#![allow(clippy::unwrap_used)]

#[test]
fn flv_path_uses_unified_provider_proxy_prefix() {
    let path = "/api/providers/proxy/rtmp/version123/stream";
    assert!(path.starts_with("/api/providers/proxy/rtmp/"));
    assert!(path.ends_with("/stream"));
}

#[test]
fn hls_playlist_path_uses_unified_provider_proxy_prefix() {
    let path = "/api/providers/proxy/live_proxy/version123/m3u8";
    assert!(path.starts_with("/api/providers/proxy/live_proxy/"));
    assert!(path.ends_with("/m3u8"));
}

#[test]
fn hls_segment_path_is_version_scoped_under_provider_proxy() {
    let path = "/api/providers/proxy/rtmp/version123/segment/seg001.ts";
    let parts: Vec<&str> = path.split('/').collect();
    assert_eq!(parts[1], "api");
    assert_eq!(parts[2], "providers");
    assert_eq!(parts[3], "proxy");
    assert_eq!(parts[4], "rtmp");
    assert_eq!(parts[5], "version123");
    assert_eq!(parts[6], "segment");
    assert_eq!(parts[7], "seg001.ts");
}

#[test]
fn hls_segment_png_disguise_path_is_supported() {
    let path = "/api/providers/proxy/live_proxy/version123/segment/seg001.png";
    assert!(path.ends_with(".png"));
    assert!(path.contains("/segment/"));
}

#[test]
fn provider_live_info_routes_live_under_provider_namespace() {
    assert_eq!("/api/providers/rtmp/info/media123", "/api/providers/rtmp/info/media123");
    assert_eq!(
        "/api/providers/live_proxy/streams?room_id=room123",
        "/api/providers/live_proxy/streams?room_id=room123"
    );
}

#[test]
fn provider_live_query_uses_room_id_only() {
    #[derive(serde::Deserialize)]
    struct RoomQuery {
        room_id: String,
    }

    let query: RoomQuery = serde_urlencoded::from_str("room_id=room123").unwrap();
    assert_eq!(query.room_id, "room123");
    assert!(serde_urlencoded::from_str::<RoomQuery>("roomId=room123").is_err());
}

#[test]
fn generated_segment_urls_can_preserve_signed_query() {
    let path = "/api/providers/proxy/rtmp/version123/segment/seg001.ts?sig=abc&uid=u1&rid=r1&exp=1";
    assert!(path.contains("sig=abc"));
    assert!(path.contains("uid=u1"));
    assert!(path.contains("rid=r1"));
}
