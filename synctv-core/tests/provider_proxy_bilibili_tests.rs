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
    proxy::{ProviderProxy, ProxyAction, ProxyRequestContext, ProxyServices},
    sign_playback_urls,
    store::{InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback},
    BilibiliProvider, PlaybackInfo, PlaybackResult, SubtitleTrack,
};

fn fake_provider_instance_manager() -> Arc<synctv_core::service::RemoteProviderManager> {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let repo = Arc::new(synctv_core::repository::ProviderInstanceRepository::new(
        pool,
    ));
    Arc::new(synctv_core::service::RemoteProviderManager::new(repo))
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

fn fake_proxy_services() -> ProxyServices {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let jwt =
        synctv_core::service::auth::JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")
            .expect("jwt");
    let username_cache =
        synctv_core::cache::UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(synctv_core::service::auth::token_blacklist::InMemoryTokenBlacklistStore::new(
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "v1/subtitle/中文",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
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
            headers: HashMap::new(),
            format: "srt".to_string(),
        }],
        3600,
    );
    store_versioned(&store, &vp).await;

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "v2/subtitle/English",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "v3/subtitle/Nonexistent",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_signed_subtitle_url_round_trips_with_generic_index_contract() {
    let store = new_store();
    let signing_key = synctv_core::service::ProxySigningKey::derive_from(
        b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
    );
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
                        name: "中文".to_string(),
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
    let sub_path_with_query = subtitle_url
        .strip_prefix("/api/providers/proxy/bilibili/")
        .expect("signed subtitle url should use bilibili proxy prefix");
    let sub_path = urlencoding::decode(
        sub_path_with_query
            .split('?')
            .next()
            .expect("signed subtitle url should include sub_path"),
    )
    .expect("signed subtitle path should be valid percent-encoding");
    let sub_path = sub_path
        .split('?')
        .next()
        .expect("decoded subtitle path should still be present");

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path,
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
    };

    let action = p
        .resolve_proxy(&ctx)
        .await
        .expect("signed subtitle path should round-trip through resolve_proxy");

    match action {
        ProxyAction::FetchAndForward { url, headers } => {
            assert_eq!(url, "https://cdn.bilibili.com/subtitle_zh.json");
            assert!(headers.contains_key("Referer"));
            assert!(headers.contains_key("User-Agent"));
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
}

#[tokio::test]
async fn test_signed_mpd_stream_url_round_trips_with_indexed_proxy_contract() {
    let store = new_store();
    let signing_key = synctv_core::service::ProxySigningKey::derive_from(
        b"Test_Secret_Key_For_JWT_Tokens_32Bytes!!",
    );
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
    let sub_path_with_query = stream_url
        .strip_prefix("/api/providers/proxy/bilibili/")
        .expect("signed stream url should use bilibili proxy prefix");
    let sub_path = urlencoding::decode(
        sub_path_with_query
            .split('?')
            .next()
            .expect("signed stream url should include sub_path"),
    )
    .expect("signed stream path should be valid percent-encoding");

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: sub_path.as_ref(),
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
    };

    let action = p
        .resolve_proxy(&ctx)
        .await
        .expect("signed DASH stream path should resolve");

    match action {
        ProxyAction::FetchAndForward { url, headers } => {
            assert_eq!(url, "https://cdn.bilibili.com/video-720.m4s");
            assert_eq!(
                headers.get("Referer").map(String::as_str),
                Some("https://www.bilibili.com")
            );
        }
        other => panic!("Expected FetchAndForward, got {other:?}"),
    }
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "v4/m3u8",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
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
            assert_eq!(proxy_base, "/api/providers/proxy/bilibili/v4");
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
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "v5/unknown",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
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
        "vexp",
        "https://cdn.bilibili.com/video.mp4",
        HashMap::new(),
        vec![],
        3600,
    );
    vp.expires_at = 0;
    store_versioned(&store, &vp).await;

    let p = provider();
    let fake_services = fake_proxy_services();
    let ctx = ProxyRequestContext {
        sub_path: "vexp/m3u8",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
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
        sub_path: "v1/m3u8",
        store: None,
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
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
        sub_path: "noseparator",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
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
        sub_path: "missing/m3u8",
        store: Some(&store),
        query_string: None,
        services: &fake_services,
        proxy_base: "/api/providers/proxy/bilibili",
        verified_claims: None,
    };
    let err = p.resolve_proxy(&ctx).await.unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}
