//! Bilibili `ProviderProxy` tests
//!
//! Tests for `BilibiliProvider::resolve_proxy` sub_path parsing and dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::{InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback},
    BilibiliProvider, PlaybackInfo, PlaybackResult, ProviderClientManager, SubtitleTrack,
};
use synctv_core::proxy_signature::ProxyUrlClaims;
use synctv_core_testing::{create_empty_provider_instance_manager, err, ok, some};

fn provider() -> BilibiliProvider {
    BilibiliProvider::with_client_manager(
        create_empty_provider_instance_manager(),
        Arc::new(ok(
            ProviderClientManager::new(),
            "provider client manager should build",
        )),
    )
}

fn new_store() -> Arc<dyn ProviderStore> {
    Arc::new(InMemoryProviderStore::new(1000))
}

fn make_versioned(
    version: &str,
    url: &str,
    headers: HashMap<String, String>,
    subtitles: Vec<SubtitleTrack>,
    ttl_secs: i64,
) -> VersionedPlayback {
    VersionedPlayback {
        version: version.to_string(),
        result: PlaybackResult {
            playback_infos: HashMap::from([(
                "direct".to_string(),
                PlaybackInfo {
                    urls: vec![url.to_string()],
                    format: "mp4".to_string(),
                    headers,
                    subtitles,
                    expires_at: None,
                    cors_proxy_required: false,
                },
            )]),
            default_mode: "direct".to_string(),
            duration_seconds: None,
            metadata: HashMap::new(),
        },
        expires_at: chrono::Utc::now().timestamp() + ttl_secs,
    }
}

async fn store_versioned(store: &Arc<dyn ProviderStore>, vp: &VersionedPlayback) {
    ok(
        store
            .set(&format!("v:{}", vp.version), vp, Duration::from_mins(5))
            .await,
        "versioned playback should be cached",
    );
}

fn expect_fetch(action: ProxyAction) -> (String, HashMap<String, String>, Option<String>) {
    match action {
        ProxyAction::FetchAndForward {
            url,
            headers,
            range_header,
        } => (url, headers, range_header),
        other => std::panic::panic_any(format!("expected FetchAndForward, got {other:?}")),
    }
}

fn expect_m3u8(action: ProxyAction) -> (String, HashMap<String, String>, String) {
    match action {
        ProxyAction::M3u8Rewrite {
            url,
            headers,
            proxy_base,
            ..
        } => (url, headers, proxy_base),
        other => std::panic::panic_any(format!("expected M3u8Rewrite, got {other:?}")),
    }
}

fn expect_direct_body(action: ProxyAction) -> (Vec<u8>, String, u16) {
    match action {
        ProxyAction::DirectBody {
            body,
            content_type,
            status,
        } => (body, content_type, status),
        other => std::panic::panic_any(format!("expected DirectBody, got {other:?}")),
    }
}

fn dash_metadata() -> serde_json::Value {
    serde_json::json!({
        "dash": {
            "duration": 120.0,
            "min_buffer_time": 1.5,
            "video_streams": [{
                "id": 80,
                "base_url": "https://cdn.bilibili.com/video-1080.m4s",
                "mime_type": "video/mp4",
                "codecs": "avc1.640028",
                "width": 1920,
                "height": 1080,
                "frame_rate": "60",
                "bandwidth": 1_000_000,
                "start_with_sap": 1,
                "segment_base": {
                    "index_range": "0-99",
                    "initialization_range": "0-10"
                }
            }],
            "audio_streams": [{
                "id": 30280,
                "base_url": "https://cdn.bilibili.com/audio.m4s",
                "mime_type": "audio/mp4",
                "codecs": "mp4a.40.2",
                "bandwidth": 128_000,
                "start_with_sap": 1,
                "segment_base": {
                    "index_range": "0-49",
                    "initialization_range": "0-8"
                },
                "audio_sampling_rate": 48000
            }]
        }
    })
}

#[tokio::test]
async fn test_subtitle_proxy() {
    let store = new_store();
    let vp = make_versioned(
        "v1",
        "https://cdn.bilibili.com/video.m3u8",
        HashMap::new(),
        vec![
            SubtitleTrack {
                language: "zh-CN".to_string(),
                name: "Chinese".to_string(),
                url: "https://cdn.bilibili.com/subtitle_zh.srt".to_string(),
                headers: HashMap::new(),
                format: "srt".to_string(),
            },
            SubtitleTrack {
                language: "en-US".to_string(),
                name: "English".to_string(),
                url: "https://cdn.bilibili.com/subtitle_en.srt".to_string(),
                headers: HashMap::new(),
                format: "srt".to_string(),
            },
        ],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v1/subtitle/Chinese",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "subtitle proxy should resolve",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/subtitle_zh.srt");
    assert!(headers.contains_key("Referer"));
    assert!(headers.contains_key("User-Agent"));
}

#[tokio::test]
async fn test_subtitle_english() {
    let store = new_store();
    let vp = make_versioned(
        "v2",
        "https://cdn.bilibili.com/video.m3u8",
        HashMap::new(),
        vec![SubtitleTrack {
            language: "en-US".to_string(),
            name: "English".to_string(),
            url: "https://cdn.bilibili.com/subtitle_en.srt".to_string(),
            headers: HashMap::new(),
            format: "srt".to_string(),
        }],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v2/subtitle/English",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, _, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "English subtitle proxy should resolve",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/subtitle_en.srt");
}

#[tokio::test]
async fn test_subtitle_not_found() {
    let store = new_store();
    let vp = make_versioned(
        "v3",
        "https://cdn.bilibili.com/video.m3u8",
        HashMap::new(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v3/subtitle/Nonexistent",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "missing subtitle should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_mode_specific_subtitle_path_resolves_by_index() {
    let store = new_store();
    let version = "vsubtitle";
    let result = PlaybackResult {
        playback_infos: HashMap::from([(
            "dash".to_string(),
            PlaybackInfo {
                urls: vec!["https://cdn.bilibili.com/video.mpd".to_string()],
                format: "mpd".to_string(),
                headers: HashMap::new(),
                subtitles: vec![
                    SubtitleTrack {
                        language: "zh-CN".to_string(),
                        name: "Chinese".to_string(),
                        url: "https://cdn.bilibili.com/subtitle_zh.json".to_string(),
                        headers: HashMap::new(),
                        format: "json".to_string(),
                    },
                    SubtitleTrack {
                        language: "en-US".to_string(),
                        name: "English".to_string(),
                        url: "https://cdn.bilibili.com/subtitle_en.json".to_string(),
                        headers: HashMap::new(),
                        format: "json".to_string(),
                    },
                ],
                expires_at: None,
                cors_proxy_required: true,
            },
        )]),
        default_mode: "dash".to_string(),
        duration_seconds: None,
        metadata: HashMap::new(),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result: result.clone(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "vsubtitle/subtitle/dash/0",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "mode-specific subtitle path should round-trip through resolve_proxy",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/subtitle_zh.json");
    assert!(headers.contains_key("Referer"));
    assert!(headers.contains_key("User-Agent"));
}

#[tokio::test]
async fn test_mpd_manifest_paths_resolve_direct_and_proxy_delivery() {
    let store = new_store();
    let version = "vmpd";
    let result = PlaybackResult {
        playback_infos: HashMap::from([(
            "dash".to_string(),
            PlaybackInfo {
                urls: vec![
                    "https://cdn.bilibili.com/video-1080.m4s".to_string(),
                    "https://cdn.bilibili.com/video-720.m4s".to_string(),
                ],
                format: "mpd".to_string(),
                headers: HashMap::from([(
                    "Referer".to_string(),
                    "https://www.bilibili.com".to_string(),
                )]),
                subtitles: vec![],
                expires_at: None,
                cors_proxy_required: true,
            },
        )]),
        default_mode: "dash".to_string(),
        duration_seconds: None,
        metadata: HashMap::from([(
            synctv_core::provider::bilibili::DASH_MANIFEST_METADATA_KEY.to_string(),
            dash_metadata(),
        )]),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result: result.clone(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    let p = provider();
    let direct_ctx = ProxyRequestContext {
        sub_path: "vmpd/mpd/dash/direct",
        store: Some(&store),
        query_string: Some("sig=s&uid=user-1&rid=room-1&exp=9999999999"),
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (body, content_type, status) = expect_direct_body(ok(
        p.resolve_proxy(&direct_ctx).await,
        "direct MPD manifest path should resolve",
    ));
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/dash+xml");
    let manifest = String::from_utf8(body).expect("manifest should be utf8");
    assert!(manifest.contains("https://cdn.bilibili.com/video-1080.m4s"));

    let proxy_ctx = ProxyRequestContext {
        sub_path: "vmpd/mpd/dash/proxy",
        store: Some(&store),
        query_string: Some("sig=s&uid=user-1&rid=room-1&exp=9999999999"),
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (body, content_type, status) = expect_direct_body(ok(
        p.resolve_proxy(&proxy_ctx).await,
        "proxied MPD manifest path should resolve",
    ));
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/dash+xml");
    let manifest = String::from_utf8(body).expect("manifest should be utf8");
    assert!(manifest.contains("/api/providers/proxy/bilibili/vmpd/stream/dash/0?"));
}

#[tokio::test]
async fn test_mpd_manifest_proxy_builds_direct_and_proxied_dash_manifests() {
    let store = new_store();
    let version = "vmpdmanifest";
    let result = PlaybackResult {
        playback_infos: HashMap::from([(
            "dash".to_string(),
            PlaybackInfo {
                urls: vec![
                    "https://cdn.bilibili.com/video-1080.m4s".to_string(),
                    "https://cdn.bilibili.com/audio.m4s".to_string(),
                ],
                format: "mpd".to_string(),
                headers: HashMap::from([(
                    "Referer".to_string(),
                    "https://www.bilibili.com".to_string(),
                )]),
                subtitles: vec![],
                expires_at: None,
                cors_proxy_required: false,
            },
        )]),
        default_mode: "dash".to_string(),
        duration_seconds: Some(120.0),
        metadata: HashMap::from([(
            synctv_core::provider::bilibili::DASH_MANIFEST_METADATA_KEY.to_string(),
            dash_metadata(),
        )]),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result,
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    let p = provider();
    let direct_ctx = ProxyRequestContext {
        sub_path: "vmpdmanifest/mpd/dash/direct",
        store: Some(&store),
        query_string: Some("sig=s&uid=user-1&rid=room-1&exp=9999999999"),
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (body, content_type, status) = expect_direct_body(ok(
        p.resolve_proxy(&direct_ctx).await,
        "direct MPD manifest should resolve",
    ));
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/dash+xml");
    let manifest = String::from_utf8(body).expect("manifest should be utf8");
    assert!(manifest.contains("https://cdn.bilibili.com/video-1080.m4s"));
    assert!(manifest.contains("https://cdn.bilibili.com/audio.m4s"));
    assert!(manifest.contains("<SegmentBase indexRange=\"0-99\">"));

    let proxied_ctx = ProxyRequestContext {
        sub_path: "vmpdmanifest/mpd/dash/proxy",
        store: Some(&store),
        query_string: Some("sig=s&uid=user-1&rid=room-1&exp=9999999999"),
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (body, content_type, status) = expect_direct_body(ok(
        p.resolve_proxy(&proxied_ctx).await,
        "proxied MPD manifest should resolve",
    ));
    assert_eq!(status, 200);
    assert_eq!(content_type, "application/dash+xml");
    let manifest = String::from_utf8(body).expect("manifest should be utf8");
    assert!(manifest.contains("/api/providers/proxy/bilibili/vmpdmanifest/stream/dash/0?"));
    assert!(manifest.contains("/api/providers/proxy/bilibili/vmpdmanifest/stream/dash/1?"));
    assert!(!manifest.contains("https://cdn.bilibili.com/video-1080.m4s"));
}

#[tokio::test]
async fn test_indexed_hls_path_resolves_matching_url() {
    let store = new_store();
    let version = "vhls";
    let result = PlaybackResult {
        playback_infos: HashMap::from([(
            "10000P_250".to_string(),
            PlaybackInfo {
                urls: vec![
                    "https://cdn.bilibili.com/live-primary.m3u8".to_string(),
                    "https://cdn.bilibili.com/live-backup.m3u8".to_string(),
                ],
                format: "m3u8".to_string(),
                headers: HashMap::from([(
                    "Referer".to_string(),
                    "https://live.bilibili.com".to_string(),
                )]),
                subtitles: vec![],
                expires_at: None,
                cors_proxy_required: true,
            },
        )]),
        default_mode: "10000P_250".to_string(),
        duration_seconds: None,
        metadata: HashMap::new(),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result: result.clone(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "vhls/m3u8/10000P_250/1",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let (url, headers, proxy_base) = expect_m3u8(ok(
        p.resolve_proxy(&ctx).await,
        "indexed HLS path should resolve",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/live-backup.m3u8");
    assert_eq!(
        headers.get("Referer").map(String::as_str),
        Some("https://live.bilibili.com")
    );
    assert_eq!(proxy_base, "/api/providers/proxy/bilibili/vhls");
}

#[tokio::test]
async fn test_hls_segment_target_url_resolves_for_rewritten_playlist() {
    let store = new_store();
    let version = "vhlsseg";
    let vp = make_versioned(
        version,
        "https://cdn.bilibili.com/live-primary.m3u8",
        HashMap::from([(
            "Referer".to_string(),
            "https://live.bilibili.com".to_string(),
        )]),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let claims = ProxyUrlClaims {
        provider: "bilibili".to_string(),
        version: version.to_string(),
        room_id: "room-1".to_string(),
        user_id: "user-1".to_string(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        target_url: Some("https://cdn.bilibili.com/segment-1.m4s".to_string()),
    };
    let ctx = ProxyRequestContext {
        sub_path: version,
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: Some(&claims),
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "rewritten HLS segment target should resolve",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/segment-1.m4s");
    assert_eq!(
        headers.get("Referer").map(String::as_str),
        Some("https://live.bilibili.com")
    );
}

#[tokio::test]
async fn test_hls_variant_target_url_is_rewritten_again() {
    let store = new_store();
    let version = "vhlsvariant";
    let vp = make_versioned(
        version,
        "https://cdn.bilibili.com/live-primary.m3u8",
        HashMap::from([(
            "Referer".to_string(),
            "https://live.bilibili.com".to_string(),
        )]),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let claims = ProxyUrlClaims {
        provider: "bilibili".to_string(),
        version: version.to_string(),
        room_id: "room-1".to_string(),
        user_id: "user-1".to_string(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        target_url: Some("https://cdn.bilibili.com/variant.m3u8?token=abc".to_string()),
    };
    let ctx = ProxyRequestContext {
        sub_path: version,
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: Some(&claims),
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let (url, headers, proxy_base) = expect_m3u8(ok(
        p.resolve_proxy(&ctx).await,
        "rewritten HLS variant target should resolve",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/variant.m3u8?token=abc");
    assert_eq!(
        headers.get("Referer").map(String::as_str),
        Some("https://live.bilibili.com")
    );
    assert_eq!(proxy_base, "/api/providers/proxy/bilibili/vhlsvariant");
}

#[tokio::test]
async fn test_default_single_stream_proxy_path_resolves_first_url() {
    let store = new_store();
    let vp = make_versioned(
        "vdefault",
        "https://cdn.bilibili.com/fallback.mp4",
        HashMap::from([(
            "Referer".to_string(),
            "https://www.bilibili.com".to_string(),
        )]),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "vdefault/stream",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "default single stream path should resolve first URL",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/fallback.mp4");
    assert_eq!(
        headers.get("Referer").map(String::as_str),
        Some("https://www.bilibili.com")
    );
}

#[tokio::test]
async fn test_empty_stream_index_is_rejected() {
    let store = new_store();
    let vp = make_versioned(
        "vempty",
        "https://cdn.bilibili.com/fallback.mp4",
        HashMap::from([(
            "Referer".to_string(),
            "https://www.bilibili.com".to_string(),
        )]),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "vempty/stream/",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let error = err(
        p.resolve_proxy(&ctx).await,
        "empty stream index should be rejected",
    );
    assert!(matches!(
        error,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_m3u8_proxy() {
    let store = new_store();
    let vp = make_versioned(
        "v4",
        "https://cdn.bilibili.com/live.m3u8",
        HashMap::from([(
            "Referer".to_string(),
            "https://www.bilibili.com".to_string(),
        )]),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v4/m3u8",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, headers, proxy_base) =
        expect_m3u8(ok(p.resolve_proxy(&ctx).await, "m3u8 proxy should resolve"));
    assert_eq!(url, "https://cdn.bilibili.com/live.m3u8");
    assert_eq!(
        some(headers.get("Referer"), "referer header should exist"),
        "https://www.bilibili.com"
    );
    assert_eq!(proxy_base, "/api/providers/proxy/bilibili/v4");
}

#[tokio::test]
async fn test_unknown_sub_path() {
    let store = new_store();
    let vp = make_versioned(
        "v5",
        "https://cdn.bilibili.com/video.mp4",
        HashMap::new(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v5/unknown",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "unknown sub path should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_expired_version() {
    let store = new_store();
    let mut vp = make_versioned(
        "vexp",
        "https://cdn.bilibili.com/video.mp4",
        HashMap::new(),
        vec![],
        3600,
    );
    vp.expires_at = 0;
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "vexp/m3u8",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "expired version should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_no_store() {
    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v1/m3u8",
        store: None,
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "missing store should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::ApiError(_)
    ));
}

#[tokio::test]
async fn test_no_slash_in_sub_path() {
    let store = new_store();
    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "noseparator",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "sub path without slash should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_version_not_in_store() {
    let store = new_store();
    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "missing/m3u8",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "missing version should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}
