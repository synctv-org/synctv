//! Alist `ProviderProxy` tests
//!
//! Tests for `AlistProvider::resolve_proxy` sub_path parsing and dispatch.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext},
    store::{InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback},
    AlistProvider, PlaybackInfo, PlaybackResult, ProviderClientManager, SubtitleTrack,
};
use synctv_core_testing::{create_empty_provider_instance_manager, err, ok, some};

fn provider() -> AlistProvider {
    AlistProvider::with_client_manager(
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

fn range_header(value: &'static str) -> http::HeaderValue {
    ok(value.parse(), "range header should parse")
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
        "a1",
        "https://alist.example.com/d/movie.mp4",
        HashMap::from([("Authorization".to_string(), "Bearer tok".to_string())]),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let mut request_headers = http::HeaderMap::new();
    request_headers.insert(http::header::RANGE, range_header("bytes=10-20"));
    let ctx = ProxyRequestContext {
        sub_path: "a1/stream",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
        verified_claims: None,
        request_context: None,
        request_headers: &request_headers,
    };
    let (url, headers, range_header) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "stream proxy should resolve",
    ));
    assert_eq!(url, "https://alist.example.com/d/movie.mp4");
    assert_eq!(
        some(
            headers.get("Authorization"),
            "authorization header should exist"
        ),
        "Bearer tok"
    );
    assert_eq!(range_header.as_deref(), Some("bytes=10-20"));
}

#[tokio::test]
async fn test_thumbnail_proxy_uses_cached_playback_metadata() {
    let store = new_store();
    let mut vp = make_versioned(
        "thumb1",
        "https://alist.example.com/d/movie.mp4",
        HashMap::from([("Authorization".to_string(), "Bearer tok".to_string())]),
        vec![],
        3600,
    );
    vp.result.metadata.insert(
        "thumbnail".to_string(),
        serde_json::json!("https://alist.example.com/thumb/movie.jpg"),
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "thumb1/thumbnail",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "thumbnail proxy should resolve",
    ));
    assert_eq!(url, "https://alist.example.com/thumb/movie.jpg");
    assert_eq!(
        some(
            headers.get("Authorization"),
            "authorization header should exist"
        ),
        "Bearer tok"
    );
}

#[tokio::test]
async fn test_m3u8_proxy() {
    let store = new_store();
    let vp = make_versioned(
        "a2",
        "https://alist.example.com/d/video.m3u8",
        HashMap::new(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "a2/m3u8",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, _, proxy_base) =
        expect_m3u8(ok(p.resolve_proxy(&ctx).await, "m3u8 proxy should resolve"));
    assert_eq!(url, "https://alist.example.com/d/video.m3u8");
    assert_eq!(proxy_base, "/api/providers/proxy/alist/a2");
}

#[tokio::test]
async fn test_hls_modes_resolve_to_their_own_m3u8_urls() {
    let store = new_store();
    let version = "alist-hls";
    let result = PlaybackResult {
        playback_infos: HashMap::from([
            (
                "transcoded_HD".to_string(),
                PlaybackInfo {
                    urls: vec!["https://aliyun.example.com/hd/master.m3u8".to_string()],
                    format: "hls".to_string(),
                    headers: HashMap::new(),
                    subtitles: vec![],
                    expires_at: None,
                    cors_proxy_required: false,
                },
            ),
            (
                "transcoded_SD".to_string(),
                PlaybackInfo {
                    urls: vec!["https://aliyun.example.com/sd/master.m3u8".to_string()],
                    format: "hls".to_string(),
                    headers: HashMap::new(),
                    subtitles: vec![],
                    expires_at: None,
                    cors_proxy_required: false,
                },
            ),
        ]),
        default_mode: "transcoded_HD".to_string(),
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
        sub_path: "alist-hls/m3u8/transcoded_SD/0",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (url, _, proxy_base) = expect_m3u8(ok(
        p.resolve_proxy(&ctx).await,
        "mode-specific m3u8 proxy should resolve",
    ));
    assert_eq!(url, "https://aliyun.example.com/sd/master.m3u8");
    assert_eq!(proxy_base, "/api/providers/proxy/alist/alist-hls");
}

#[tokio::test]
async fn test_m3u8_rewritten_segment_query_fetches_target_url() {
    let store = new_store();
    let vp = make_versioned(
        "alist-segment",
        "https://aliyun.example.com/hd/master.m3u8",
        HashMap::new(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let mut request_headers = http::HeaderMap::new();
    request_headers.insert(http::header::RANGE, range_header("bytes=512-1023"));
    let ctx = ProxyRequestContext {
        sub_path: "alist-segment",
        store: Some(&store),
        query_string: Some("url=https%3A%2F%2Faliyun.example.com%2Fhd%2Fseg-1.ts"),
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
        verified_claims: None,
        request_context: None,
        request_headers: &request_headers,
    };
    let (url, headers, range_header) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "m3u8 segment proxy should resolve",
    ));
    assert_eq!(url, "https://aliyun.example.com/hd/seg-1.ts");
    assert!(headers.is_empty());
    assert_eq!(range_header.as_deref(), Some("bytes=512-1023"));
}

#[tokio::test]
async fn test_unknown_sub_path() {
    let store = new_store();
    let vp = make_versioned(
        "a3",
        "https://alist.example.com/d/movie.mp4",
        HashMap::new(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "a3/subtitle/zh",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
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
async fn test_mode_specific_subtitle_path_resolves_to_matching_mode() {
    let store = new_store();
    let version = "asub";
    let result = PlaybackResult {
        playback_infos: HashMap::from([
            (
                "direct".to_string(),
                PlaybackInfo {
                    urls: vec!["https://alist.example.com/d/movie.mp4".to_string()],
                    format: "mp4".to_string(),
                    headers: HashMap::new(),
                    subtitles: Vec::new(),
                    expires_at: None,
                    cors_proxy_required: false,
                },
            ),
            (
                "transcoded_720p".to_string(),
                PlaybackInfo {
                    urls: vec!["https://alist.example.com/d/movie-720.m3u8".to_string()],
                    format: "hls".to_string(),
                    headers: HashMap::new(),
                    subtitles: vec![SubtitleTrack {
                        language: "en-US".to_string(),
                        name: "English".to_string(),
                        url: "https://alist.example.com/subtitles/movie-en.srt".to_string(),
                        headers: HashMap::new(),
                        format: "srt".to_string(),
                    }],
                    expires_at: None,
                    cors_proxy_required: false,
                },
            ),
        ]),
        default_mode: "direct".to_string(),
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
        sub_path: "asub/subtitle/transcoded_720p/0",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };

    let (url, headers, _) = expect_fetch(ok(
        p.resolve_proxy(&ctx).await,
        "mode-specific subtitle path should resolve to the same playback mode",
    ));
    assert_eq!(url, "https://alist.example.com/subtitles/movie-en.srt");
    assert!(headers.is_empty());
}

#[tokio::test]
async fn test_expired_version() {
    let store = new_store();
    let mut vp = make_versioned(
        "aexp",
        "https://alist.example.com/d/movie.mp4",
        HashMap::new(),
        vec![],
        3600,
    );
    vp.expires_at = 0;
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "aexp/stream",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
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
        sub_path: "a1/stream",
        store: None,
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
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
        proxy_base: "/api/providers/proxy/alist",
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
        proxy_base: "/api/providers/proxy/alist",
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
async fn test_m3u8_preserves_headers() {
    let store = new_store();
    let headers = HashMap::from([
        ("Authorization".to_string(), "Bearer secret".to_string()),
        ("X-Custom".to_string(), "val".to_string()),
    ]);
    let vp = make_versioned(
        "a4",
        "https://alist.example.com/d/master.m3u8",
        headers.clone(),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "a4/m3u8",
        store: Some(&store),
        query_string: None,
        services: None,
        public_id_codec: None,
        proxy_base: "/api/providers/proxy/alist",
        verified_claims: None,
        request_context: None,
        request_headers: &http::HeaderMap::new(),
    };
    let (_, resolved_headers, _) = expect_m3u8(ok(
        p.resolve_proxy(&ctx).await,
        "m3u8 proxy should resolve with headers",
    ));
    assert_eq!(resolved_headers, headers);
}
