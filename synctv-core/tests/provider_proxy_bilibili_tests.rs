//! Bilibili `ProviderProxy` tests
//!
//! Tests for `BilibiliProvider::resolve_proxy` sub_path parsing and dispatch.
//!
//! Run with: cargo nextest run -p synctv-core --test provider_proxy_bilibili_tests
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::{
    proxy::{ProxyAction, ProxyRequestContext, ProviderProxy},
    store::{InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback},
    BilibiliProvider, PlaybackInfo, PlaybackResult, SubtitleTrack,
};

fn fake_provider_instance_manager() -> Arc<synctv_core::service::RemoteProviderManager> {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let repo = Arc::new(synctv_core::repository::ProviderInstanceRepository::new(pool));
    Arc::new(synctv_core::service::RemoteProviderManager::new(
        repo, None, None,
    ))
}

fn provider() -> BilibiliProvider {
    BilibiliProvider::new(fake_provider_instance_manager())
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
    store
        .set(&format!("v:{}", vp.version), vp, Duration::from_mins(5))
        .await
        .unwrap();
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
                name: "中文".to_string(),
                url: "https://cdn.bilibili.com/subtitle_zh.srt".to_string(),
                format: "srt".to_string(),
            },
            SubtitleTrack {
                language: "en-US".to_string(),
                name: "English".to_string(),
                url: "https://cdn.bilibili.com/subtitle_en.srt".to_string(),
                format: "srt".to_string(),
            },
        ],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v1/subtitle/中文",
        store: Some(&store),
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::FetchAndForward { url, headers } => {
            assert_eq!(url, "https://cdn.bilibili.com/subtitle_zh.srt");
            assert!(headers.contains_key("Referer"));
            assert!(headers.contains_key("User-Agent"));
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
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
            format: "srt".to_string(),
        }],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v2/subtitle/English",
        store: Some(&store),
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::FetchAndForward { url, .. } => {
            assert_eq!(url, "https://cdn.bilibili.com/subtitle_en.srt");
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
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
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_m3u8_proxy() {
    let store = new_store();
    let vp = make_versioned(
        "v4",
        "https://cdn.bilibili.com/live.m3u8",
        HashMap::from([("Referer".to_string(), "https://www.bilibili.com".to_string())]),
        vec![],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let ctx = ProxyRequestContext {
        sub_path: "v4/m3u8",
        store: Some(&store),
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::M3u8Rewrite {
            url,
            headers,
            proxy_base,
        } => {
            assert_eq!(url, "https://cdn.bilibili.com/live.m3u8");
            assert_eq!(headers.get("Referer").unwrap(), "https://www.bilibili.com");
            assert_eq!(proxy_base, "/api/providers/bilibili/proxy/v4");
        }
        other => panic!("Expected M3u8Rewrite, got {other:?}"),
    }
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
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
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
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
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
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
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
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
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
        proxy_base: "/api/providers/bilibili/proxy",
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}
