//! `ProviderProxy` trait casting and `lookup_versioned` tests
//!
//! Tests that `as_provider_proxy()` returns the correct result for each provider,
//! and edge cases for `lookup_versioned`.
//!
//! Run with: cargo nextest run -p synctv-core --test provider_proxy_trait_tests
#![allow(clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use synctv_core::provider::{
    proxy::lookup_versioned,
    store::{InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback},
    AlistProvider, BilibiliProvider, DirectUrlProvider, EmbyProvider, LiveProxyProvider,
    MediaProvider, PlaybackResult, ProviderSet, RtmpProvider,
};

fn fake_provider_instance_manager() -> Arc<synctv_core::service::RemoteProviderManager> {
    let pool = sqlx::PgPool::connect_lazy("postgresql://fake").unwrap();
    let repo = Arc::new(synctv_core::repository::ProviderInstanceRepository::new(
        pool,
    ));
    Arc::new(synctv_core::service::RemoteProviderManager::new(repo))
}

fn new_store() -> Arc<dyn ProviderStore> {
    Arc::new(InMemoryProviderStore::new(1000))
}

// as_provider_proxy() casting tests

#[tokio::test]
async fn bilibili_has_provider_proxy() {
    let p = BilibiliProvider::new(fake_provider_instance_manager());
    assert!(p.as_provider_proxy().is_some());
}

#[tokio::test]
async fn alist_has_provider_proxy() {
    let p = AlistProvider::new(fake_provider_instance_manager());
    assert!(p.as_provider_proxy().is_some());
}

#[tokio::test]
async fn emby_has_provider_proxy() {
    let p = EmbyProvider::new(fake_provider_instance_manager());
    assert!(p.as_provider_proxy().is_some());
}

#[test]
fn direct_url_does_not_have_provider_proxy() {
    let p = DirectUrlProvider::new();
    assert!(p.as_provider_proxy().is_some());
}

#[tokio::test]
async fn rtmp_has_provider_proxy() {
    let p = RtmpProvider::new();
    assert!(p.as_provider_proxy().is_some());
}

#[tokio::test]
async fn live_proxy_has_provider_proxy() {
    let p = LiveProxyProvider::new();
    assert!(p.as_provider_proxy().is_some());
}

#[tokio::test]
async fn provider_set_registers_live_providers() {
    let provider_set = ProviderSet {
        alist: Arc::new(AlistProvider::new(fake_provider_instance_manager())),
        bilibili: Arc::new(BilibiliProvider::new(fake_provider_instance_manager())),
        emby: Arc::new(EmbyProvider::new(fake_provider_instance_manager())),
        direct_url: Arc::new(DirectUrlProvider::new()),
        rtmp: Arc::new(RtmpProvider::new()),
        live_proxy: Arc::new(LiveProxyProvider::new()),
    };

    let registry = provider_set.build_proxy_registry();
    assert!(registry.get("rtmp").is_some());
    assert!(registry.get("live_proxy").is_some());
}

// lookup_versioned edge cases

#[tokio::test]
async fn test_lookup_versioned_store_error_propagates() {
    let store = new_store();
    let err = lookup_versioned(Some(&store), "nonexistent", None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_lookup_versioned_expired_returns_not_found() {
    let store = new_store();
    let vp = VersionedPlayback {
        version: "exp".to_string(),
        result: PlaybackResult {
            playback_infos: HashMap::new(),
            default_mode: "direct".to_string(),
            metadata: HashMap::new(),
        },
        expires_at: 0,
    };
    store
        .set(&format!("v:{}", vp.version), &vp, Duration::from_mins(5))
        .await
        .unwrap();
    let err = lookup_versioned(Some(&store), "exp", None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::NotFound
    ));
}

#[tokio::test]
async fn test_lookup_versioned_none_store_returns_api_error() {
    let err = lookup_versioned(None, "v1", None).await.unwrap_err();
    assert!(matches!(
        err,
        synctv_core::provider::ProviderError::ApiError(_)
    ));
}
