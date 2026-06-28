//! Tests for `RtmpProvider` source_config validation.

use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::provider::{
    MediaProvider, ProviderContext, ProviderError, RtmpProvider, SourceConfig,
};

fn create_context() -> ProviderContext<'static> {
    ProviderContext::new("synctv")
        .with_user_id(UserId::expect_positive(1))
        .with_room_id(RoomId::expect_positive(10))
        .with_media_id(MediaId::expect_positive(100))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_rejects_non_rtmp_config() {
    let provider = RtmpProvider::new();
    let ctx = create_context();
    let config = synctv_core_testing::direct_url_media_source_config("http://192.168.1.1/internal");

    let result = provider
        .validate_source_config(&ctx, SourceConfig::media(&config))
        .await;
    assert!(
        matches!(result, Err(ProviderError::InvalidConfig(_))),
        "RtmpProvider should reject non-RTMP media source_config"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_accepts_valid_fields() {
    let provider = RtmpProvider::new();
    let ctx = create_context();

    let result = provider
        .validate_source_config(
            &ctx,
            SourceConfig::media(&synctv_core_testing::rtmp_managed_live_media_source_config()),
        )
        .await;
    assert!(
        result.is_ok(),
        "RtmpProvider should accept valid source_config: {:?}",
        result.err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_accepts_empty_config_with_context_binding() {
    let provider = RtmpProvider::new();
    let ctx = create_context();

    let result = provider
        .validate_source_config(
            &ctx,
            SourceConfig::media(&synctv_core_testing::rtmp_managed_live_media_source_config()),
        )
        .await;
    assert!(
        result.is_ok(),
        "RtmpProvider should accept empty source_config when context provides room/media binding: {:?}",
        result.err()
    );
}
