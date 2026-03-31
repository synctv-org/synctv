//! Emby `ProviderProxy` tests
//!
//! Tests for `EmbyProvider::resolve_proxy` sub_path parsing and dispatch.
//!
//! Run with: cargo nextest run -p synctv-core --test provider_proxy_emby_tests
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::{
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext, ProxyServices},
    store::{InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback},
    EmbyProvider, PlaybackInfo, PlaybackResult, SubtitleTrack,
};

fn fake_provider_instance_manager() -> Arc<synctv_core::service::RemoteProviderManager> {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let repo = Arc::new(synctv_core::repository::ProviderInstanceRepository::new(
        pool,
    ));
    Arc::new(synctv_core::service::RemoteProviderManager::new(repo))
}

fn provider() -> EmbyProvider {
    EmbyProvider::new(fake_provider_instance_manager())
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

fn fake_proxy_services() -> ProxyServices {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let jwt =
        synctv_core::service::auth::JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
            .expect("jwt");
    let l2 = Arc::new(synctv_core::cache::NoopCacheL2);
    let username_cache =
        synctv_core::cache::UsernameCache::new(l2, "test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(synctv_core::service::InMemoryTokenBlacklistStore::new(
        1000, 3600, 86400,
    ));
    let key_builder = synctv_core::cache::KeyBuilder::new("test");
    let brute_force =
        synctv_core::service::auth::BruteForceProtection::in_memory("test".to_string());
    let user_service = synctv_core::service::UserService::new(
        pool.clone(),
        jwt,
        username_cache,
        synctv_core::config::PasswordComplexityConfig::default(),
        token_blacklist,
        key_builder,
        brute_force,
    );
    let credential_repo =
        Arc::new(synctv_core::repository::UserProviderCredentialRepository::new(pool.clone()));
    let room_service = synctv_core::service::RoomService::new(pool, user_service);
    ProxyServices {
        room_service: Arc::new(room_service),
        credential_encryption: None,
        credential_repo,
        signing_key: Arc::new(synctv_core::service::ProxySigningKey::derive_from(
            b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
        )),
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e1/stream",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::FetchAndForward { url, headers } => {
            assert_eq!(url, "https://emby.example.com/Videos/123/stream.mp4");
            assert_eq!(headers.get("X-Emby-Token").unwrap(), "api-key-123");
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e2/m3u8",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::M3u8Rewrite {
            url,
            headers,
            proxy_base,
        } => {
            assert_eq!(url, "https://emby.example.com/Videos/123/master.m3u8");
            assert_eq!(headers.get("X-Emby-Token").unwrap(), "api-key-123");
            assert_eq!(proxy_base, "/api/providers/proxy/emby/e2");
        }
        other => panic!("Expected M3u8Rewrite, got {other:?}"),
    }
}

#[tokio::test]
async fn test_subtitle_by_index_0() {
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
                format: "srt".to_string(),
            },
            SubtitleTrack {
                language: "en-US".to_string(),
                name: "English".to_string(),
                url: "https://emby.example.com/Videos/123/Subtitles/1/Stream.srt".to_string(),
                format: "srt".to_string(),
            },
        ],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e3/subtitle/0",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::FetchAndForward { url, headers } => {
            assert_eq!(
                url,
                "https://emby.example.com/Videos/123/Subtitles/0/Stream.srt"
            );
            assert_eq!(headers.get("X-Emby-Token").unwrap(), "api-key-123");
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
}

#[tokio::test]
async fn test_subtitle_by_index_1() {
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
                format: "srt".to_string(),
            },
            SubtitleTrack {
                language: "en-US".to_string(),
                name: "English".to_string(),
                url: "https://emby.example.com/sub1.srt".to_string(),
                format: "srt".to_string(),
            },
        ],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e4/subtitle/1",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::FetchAndForward { url, .. } => {
            assert_eq!(url, "https://emby.example.com/sub1.srt");
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
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
            format: "srt".to_string(),
        }],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e5/subtitle/5",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e6/subtitle/abc",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::ApiError(_)
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e7/something_else",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
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
        "eexp",
        "https://emby.example.com/stream.mp4",
        emby_headers(),
        vec![],
        3600,
    );
    vp.expires_at = 0;
    store_versioned(&store, &vp).await;

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "eexp/stream",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e1/stream",
        store: None,
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "noslash",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "missing/stream",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "e8/stream",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/emby",
        verified_claims: None,
    };
    let action = p.resolve_proxy(&ctx).await.unwrap();
    match action {
        ProxyAction::FetchAndForward { headers: h, .. } => {
            assert_eq!(h, headers);
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
}
