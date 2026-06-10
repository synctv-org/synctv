//! Bilibili `ProviderProxy` tests
//!
//! Tests for `BilibiliProvider::resolve_proxy` sub_path parsing and dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    sign_playback_urls,
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

fn signing_key() -> synctv_core::proxy_signature::ProxySigningKey {
    ok(
        synctv_core::proxy_signature::ProxySigningKey::try_derive_from(
            b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
        ),
        "test proxy signing key should derive",
    )
}

fn signed_sub_path<'a>(url: &'a str, prefix: &str, context: &str) -> std::borrow::Cow<'a, str> {
    let sub_path_with_query = some(
        url.strip_prefix(prefix),
        "signed proxy URL should use provider proxy prefix",
    );
    let encoded = some(
        sub_path_with_query.split('?').next(),
        "signed proxy URL should include sub_path",
    );
    ok(
        urlencoding::decode(encoded),
        &format!("{context} should be valid percent-encoding"),
    )
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
async fn test_signed_subtitle_url_round_trips_with_generic_index_contract() {
    let store = new_store();
    let signing_key = signing_key();
    let version = "vsigned";
    let mut result = PlaybackResult {
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
        metadata: HashMap::new(),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result: result.clone(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    sign_playback_urls(
        &mut result,
        "bilibili",
        version,
        &signing_key,
        "room-1",
        "user-1",
        chrono::Utc::now().timestamp() + 3600,
    );

    let subtitle_url = result.playback_infos["dash"].subtitles[0].url.clone();
    let sub_path = signed_sub_path(
        &subtitle_url,
        "/api/providers/proxy/bilibili/",
        "signed subtitle path",
    );
    let sub_path = some(
        sub_path.split('?').next(),
        "decoded subtitle path should still be present",
    );

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path,
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
        "signed subtitle path should round-trip through resolve_proxy",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/subtitle_zh.json");
    assert!(headers.contains_key("Referer"));
    assert!(headers.contains_key("User-Agent"));
}

#[tokio::test]
async fn test_signed_mpd_stream_url_round_trips_with_indexed_proxy_contract() {
    let store = new_store();
    let signing_key = signing_key();
    let version = "vmpd";
    let mut result = PlaybackResult {
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
        metadata: HashMap::new(),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result: result.clone(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    sign_playback_urls(
        &mut result,
        "bilibili",
        version,
        &signing_key,
        "room-1",
        "user-1",
        chrono::Utc::now().timestamp() + 3600,
    );

    let stream_url = result.playback_infos["dash"].urls[1].clone();
    let sub_path = signed_sub_path(
        &stream_url,
        "/api/providers/proxy/bilibili/",
        "signed stream path",
    );

    let p = provider();
    let claims = ProxyUrlClaims {
        provider: "bilibili".to_string(),
        version: version.to_string(),
        room_id: "room-1".to_string(),
        user_id: "user-1".to_string(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
        target_url: None,
    };
    let ctx = ProxyRequestContext {
        sub_path: sub_path.as_ref(),
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
        "signed DASH stream path should resolve",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/video-720.m4s");
    assert_eq!(
        headers.get("Referer").map(String::as_str),
        Some("https://www.bilibili.com")
    );
}

#[tokio::test]
async fn test_signed_hls_url_round_trips_with_indexed_proxy_contract() {
    let store = new_store();
    let signing_key = signing_key();
    let version = "vhls";
    let mut result = PlaybackResult {
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
        metadata: HashMap::new(),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result: result.clone(),
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    sign_playback_urls(
        &mut result,
        "bilibili",
        version,
        &signing_key,
        "room-1",
        "user-1",
        chrono::Utc::now().timestamp() + 3600,
    );

    let hls_url = result.playback_infos["10000P_250"].urls[1].clone();
    let sub_path = signed_sub_path(
        &hls_url,
        "/api/providers/proxy/bilibili/",
        "signed HLS path",
    );

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: sub_path.as_ref(),
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
        "signed HLS path should resolve",
    ));
    assert_eq!(url, "https://cdn.bilibili.com/live-backup.m3u8");
    assert_eq!(
        headers.get("Referer").map(String::as_str),
        Some("https://live.bilibili.com")
    );
    assert_eq!(proxy_base, "/api/providers/proxy/bilibili/vhls");
}

#[tokio::test]
async fn test_signed_hls_segment_target_url_resolves_for_rewritten_playlist() {
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
async fn test_signed_hls_variant_target_url_is_rewritten_again() {
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
