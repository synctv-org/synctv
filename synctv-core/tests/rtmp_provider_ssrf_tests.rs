//! Tests for `RtmpProvider` source_config validation
//!
//! SSRF protection is now enforced at the DNS resolver level (synctv-common).
//! These tests verify that `RtmpProvider` correctly rejects source_config
//! that contains URL fields (which could be abused for SSRF).

#![allow(clippy::unwrap_used)]

use serde_json::json;
use synctv_core::provider::{MediaProvider, ProviderContext, ProviderError, RtmpProvider};

fn create_context() -> ProviderContext<'static> {
    ProviderContext::new("synctv")
        .with_user_id("test_user")
        .with_room_id("test_room")
}

// validate_source_config should not accept external URLs

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_rejects_url_field() {
    let provider = RtmpProvider::new();
    let ctx = create_context();

    let malicious_configs = vec![
        json!({
            "room_id": "room123",
            "media_id": "media456",
            "url": "http://192.168.1.1/internal"
        }),
        json!({
            "room_id": "room123",
            "media_id": "media456",
            "rtmp_url": "rtmp://localhost/live/stream"
        }),
        json!({
            "room_id": "room123",
            "media_id": "media456",
            "source_url": "http://169.254.169.254/metadata"
        }),
    ];

    for config in malicious_configs {
        let result = provider.validate_source_config(&ctx, &config).await;
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
            panic!("Expected InvalidConfig error, got: {err:?}");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn test_rtmp_provider_validate_source_config_accepts_valid_fields() {
    let provider = RtmpProvider::new();
    let ctx = create_context();

    let valid_config = json!({
        "room_id": "room123",
        "media_id": "media456"
    });

    let result = provider.validate_source_config(&ctx, &valid_config).await;
    assert!(
        result.is_ok(),
        "RtmpProvider should accept valid source_config: {:?}",
        result.err()
    );
}
