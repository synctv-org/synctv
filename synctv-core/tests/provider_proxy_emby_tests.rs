//! Emby `ProviderProxy` tests
//!
//! Tests for `EmbyProvider::resolve_proxy` sub_path parsing and dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::{InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback},
    EmbyProvider, PlaybackInfo, PlaybackResult, ProviderClientManager, ProviderError,
    SubtitleTrack,
};
use synctv_core_testing::{create_empty_provider_instance_manager, err, ok, some};

fn provider() -> EmbyProvider {
    EmbyProvider::with_client_manager(
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

fn emby_headers() -> HashMap<String, String> {
    HashMap::from([("X-Emby-Token".to_string(), "api-key-123".to_string())])
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

#[tokio::test]
async fn test_stream_proxy() {
    let store = new_store();
    let vp = make_versioned(
        "e1",
        "https://emby.example.com/Videos/123/stream.mp4",
        emby_headers(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e1/stream",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "stream proxy should resolve",
    ));
    assert_eq!(url, "https://emby.example.com/Videos/123/stream.mp4");
    assert_eq!(
        some(
            headers.get("X-Emby-Token"),
            "emby token header should exist"
        ),
        "api-key-123"
    );
}

#[tokio::test]
async fn test_m3u8_proxy() {
    let store = new_store();
    let vp = make_versioned(
        "e2",
        "https://emby.example.com/Videos/123/master.m3u8",
        emby_headers(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e2/m3u8",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, headers, proxy_base) =
        expect_m3u8(ok(p.resolve_proxy(&ctx).await, "m3u8 proxy should resolve"));
    assert_eq!(url, "https://emby.example.com/Videos/123/master.m3u8");
    assert_eq!(
        some(
            headers.get("X-Emby-Token"),
            "emby token header should exist"
        ),
        "api-key-123"
    );
    assert_eq!(proxy_base, "/api/providers/proxy/emby/e2");
}

#[tokio::test]
async fn test_hls_modes_resolve_to_their_own_m3u8_urls() {
    let store = new_store();
    let version = "emby-hls";
    let result = PlaybackResult {
        playback_infos: HashMap::from([
            (
                "source_a_transcode".to_string(),
                PlaybackInfo {
                    urls: vec!["https://emby.example.com/Videos/123/a/master.m3u8".to_string()],
                    format: "hls".to_string(),
                    headers: emby_headers(),
                    subtitles: vec![],
                    expires_at: None,
                    cors_proxy_required: true,
                },
            ),
            (
                "source_b_transcode".to_string(),
                PlaybackInfo {
                    urls: vec!["https://emby.example.com/Videos/123/b/master.m3u8".to_string()],
                    format: "hls".to_string(),
                    headers: emby_headers(),
                    subtitles: vec![],
                    expires_at: None,
                    cors_proxy_required: true,
                },
            ),
        ]),
        default_mode: "source_a_transcode".to_string(),
        duration_seconds: None,
        metadata: HashMap::new(),
    };
    let stored = VersionedPlayback {
        version: version.to_string(),
        result,
        expires_at: chrono::Utc::now().timestamp() + 3600,
    };
    store_versioned(&store, &stored).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "emby-hls/m3u8/source_b_transcode/0",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, headers, proxy_base) = expect_m3u8(ok(
        p.resolve_proxy(&ctx).await,
        "mode-specific m3u8 proxy should resolve",
    ));
    assert_eq!(url, "https://emby.example.com/Videos/123/b/master.m3u8");
    assert_eq!(
        some(
            headers.get("X-Emby-Token"),
            "emby token header should exist"
        ),
        "api-key-123"
    );
    assert_eq!(proxy_base, "/api/providers/proxy/emby/emby-hls");
}

#[tokio::test]
async fn test_subtitle_path_without_mode_is_rejected() {
    let store = new_store();
    let vp = make_versioned(
        "e3",
        "https://emby.example.com/Videos/123/stream.mp4",
        emby_headers(),
        vec![
            SubtitleTrack {
                language: "zh-CN".to_string(),
                name: "Chinese".to_string(),
                url: "https://emby.example.com/Videos/123/Subtitles/0/Stream.srt".to_string(),
                headers: HashMap::new(),
                format: "srt".to_string(),
            },
            SubtitleTrack {
                language: "en-US".to_string(),
                name: "English".to_string(),
                url: "https://emby.example.com/Videos/123/Subtitles/1/Stream.srt".to_string(),
                headers: HashMap::new(),
                format: "srt".to_string(),
            },
        ],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e3/subtitle/0",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "subtitle path without mode should be rejected",
    );
    assert!(matches!(err, ProviderError::NotFound));
}

#[tokio::test]
async fn test_subtitle_by_mode_and_index() {
    let store = new_store();
    let vp = make_versioned(
        "e4",
        "https://emby.example.com/Videos/123/stream.mp4",
        emby_headers(),
        vec![
            SubtitleTrack {
                language: "zh-CN".to_string(),
                name: "Chinese".to_string(),
                url: "https://emby.example.com/sub0.srt".to_string(),
                headers: HashMap::new(),
                format: "srt".to_string(),
            },
            SubtitleTrack {
                language: "en-US".to_string(),
                name: "English".to_string(),
                url: "https://emby.example.com/sub1.srt".to_string(),
                headers: HashMap::new(),
                format: "srt".to_string(),
            },
        ],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e4/subtitle/direct/1",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, _, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "subtitle proxy should resolve by mode and index",
    ));
    assert_eq!(url, "https://emby.example.com/sub1.srt");
}

#[tokio::test]
async fn test_subtitle_proxy_prefers_subtitle_headers_when_present() {
    let store = new_store();
    let vp = make_versioned(
        "ehdr",
        "https://emby.example.com/Videos/123/stream.mp4",
        emby_headers(),
        vec![SubtitleTrack {
            language: "zh-CN".to_string(),
            name: "Chinese".to_string(),
            url: "https://emby.example.com/subtitle.srt".to_string(),
            headers: HashMap::from([(
                "X-Subtitle-Token".to_string(),
                "subtitle-secret".to_string(),
            )]),
            format: "srt".to_string(),
        }],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "ehdr/subtitle/direct/0",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "subtitle proxy should resolve with merged headers",
    ));
    assert_eq!(url, "https://emby.example.com/subtitle.srt");
    assert_eq!(
        headers.get("X-Subtitle-Token").map(String::as_str),
        Some("subtitle-secret")
    );
    assert_eq!(
        headers.get("X-Emby-Token").map(String::as_str),
        Some("api-key-123")
    );
}

#[tokio::test]
async fn test_mode_specific_subtitle_path_resolves_to_matching_mode() {
    let store = new_store();
    let version = "emode";
    let result = PlaybackResult {
        playback_infos: HashMap::from([
            (
                "source_a".to_string(),
                PlaybackInfo {
                    urls: vec!["https://emby.example.com/Videos/123/a.mp4".to_string()],
                    format: "mp4".to_string(),
                    headers: emby_headers(),
                    subtitles: vec![SubtitleTrack {
                        language: "zh-CN".to_string(),
                        name: "Chinese".to_string(),
                        url: "https://emby.example.com/subtitles/a-zh.srt".to_string(),
                        headers: HashMap::new(),
                        format: "srt".to_string(),
                    }],
                    expires_at: None,
                    cors_proxy_required: true,
                },
            ),
            (
                "source_b".to_string(),
                PlaybackInfo {
                    urls: vec!["https://emby.example.com/Videos/123/b.mp4".to_string()],
                    format: "mp4".to_string(),
                    headers: emby_headers(),
                    subtitles: vec![SubtitleTrack {
                        language: "en-US".to_string(),
                        name: "English".to_string(),
                        url: "https://emby.example.com/subtitles/b-en.srt".to_string(),
                        headers: HashMap::new(),
                        format: "srt".to_string(),
                    }],
                    expires_at: None,
                    cors_proxy_required: true,
                },
            ),
        ]),
        default_mode: "source_a".to_string(),
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
        sub_path: "emode/subtitle/source_b/0",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "mode-specific subtitle path should resolve to the same playback mode",
    ));
    assert_eq!(url, "https://emby.example.com/subtitles/b-en.srt");
    assert_eq!(
        some(
            headers.get("X-Emby-Token"),
            "emby token header should exist"
        ),
        "api-key-123"
    );
}

#[tokio::test]
async fn test_subtitle_index_out_of_range() {
    let store = new_store();
    let vp = make_versioned(
        "e5",
        "https://emby.example.com/stream.mp4",
        emby_headers(),
        vec![SubtitleTrack {
            language: "zh-CN".to_string(),
            name: "Chinese".to_string(),
            url: "https://emby.example.com/sub0.srt".to_string(),
            headers: HashMap::new(),
            format: "srt".to_string(),
        }],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e5/subtitle/5",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "out-of-range subtitle index should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_subtitle_invalid_index() {
    let store = new_store();
    let vp = make_versioned(
        "e6",
        "https://emby.example.com/stream.mp4",
        emby_headers(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e6/subtitle/direct/abc",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let err = err(
        p.resolve_proxy(&ctx).await,
        "invalid subtitle index should be rejected",
    );
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_unknown_sub_path() {
    let store = new_store();
    let vp = make_versioned(
        "e7",
        "https://emby.example.com/stream.mp4",
        emby_headers(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e7/something_else",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
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
        "eexp",
        "https://emby.example.com/stream.mp4",
        emby_headers(),
        vec![],
        3600,
    );
    vp.expires_at = 0;
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "eexp/stream",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
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
        sub_path: "e1/stream",
        store: None,
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
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
        sub_path: "noslash",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
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
        sub_path: "missing/stream",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
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

#[tokio::test]
async fn test_stream_preserves_all_headers() {
    let store = new_store();
    let mut headers = emby_headers();
    headers.insert("X-Custom".to_string(), "val123".to_string());
    let vp = make_versioned(
        "e8",
        "https://emby.example.com/stream.mp4",
        headers.clone(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "e8/stream",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (_, resolved_headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "stream proxy should resolve with all headers",
    ));
    assert_eq!(resolved_headers, headers);
}
