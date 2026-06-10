//! Tests for `RtmpProvider` source_config validation
//!
//! SSRF protection is now enforced at the DNS resolver level (synctv-common).
//! These tests verify that `RtmpProvider` correctly rejects source_config
//! that contains URL fields (which could be abused for SSRF).

use serde_json::json;
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

// validate_source_config should not accept external URLs

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_rejects_url_field() {
    let provider = RtmpProvider::new();
    let ctx = create_context();

    let malicious_configs = vec![
        json!({
            "url": "http://192.168.1.1/internal"
        }),
        json!({
            "rtmp_url": "rtmp://localhost/live/stream"
        }),
        json!({
            "source_url": "http://169.254.169.254/metadata"
        }),
    ];

    for config in malicious_configs {
        let result = provider
            .validate_source_config(&ctx, SourceConfig::media(&config))
            .await;
        assert!(
            result.is_err(),
            "RtmpProvider should reject source_config with URL fields: {config}"
        );
        if let Err(ProviderError::InvalidConfig(msg)) = result {
            assert!(
                msg.contains("URL") || msg.contains("not supported") || msg.contains("invalid"),
                "Error message should mention URL or not supported: {msg}"
            );
        } else if let Err(err) = result {
            std::panic::panic_any(format!("Expected InvalidConfig error, got: {err:?}"));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_accepts_valid_fields() {
    let provider = RtmpProvider::new();
    let ctx = create_context();

    let result = provider
        .validate_source_config(&ctx, SourceConfig::media(&json!({})))
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
        .validate_source_config(&ctx, SourceConfig::media(&json!({})))
        .await;
    assert!(
        result.is_ok(),
        "RtmpProvider should accept empty source_config when context provides room/media binding: {:?}",
        result.err()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_rejects_identity_fields() {
    let provider = RtmpProvider::new();
    let ctx = create_context();

    let result = provider
        .validate_source_config(&ctx, SourceConfig::media(&json!({"room_id": "room123"})))
        .await;
    assert!(
        matches!(result, Err(ProviderError::InvalidConfig(_))),
        "RtmpProvider should reject source_config identity fields for internal streams"
    );
}
