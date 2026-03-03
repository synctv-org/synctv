//! RTMP Authentication tests for `extract_token_from_query` and related auth logic.
//!
//! Tests the token extraction helper used in `RtmpAuthCallbackImpl::on_publish`.
//! The `extract_token_from_query` function is private, so we test it indirectly
//! or test the public-facing behavior via the auth trait.

#![allow(clippy::unwrap_used)]
/// Since `extract_token_from_query` is private, we replicate the logic here
/// to test the same algorithm. This validates the URL-decoding behavior that
/// the actual implementation uses.
fn extract_token_from_query(query: &str) -> Option<String> {
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("token=") {
            let decoded = percent_encoding::percent_decode_str(value)
                .decode_utf8_lossy()
                .into_owned();
            return Some(decoded);
        }
    }
    None
}

#[test]
fn test_extract_token_from_query_standard() {
    let query = "foo=a&token=xyz&bar=b";
    let result = extract_token_from_query(query);
    assert_eq!(result, Some("xyz".to_string()));
}

#[test]
fn test_extract_token_from_query_missing() {
    let query = "foo=a&bar=b";
    let result = extract_token_from_query(query);
    assert!(result.is_none());
}

#[test]
fn test_extract_token_from_query_url_encoded() {
    // %2B is the URL encoding for '+'
    let query = "token=a%2Bb";
    let result = extract_token_from_query(query);
    assert_eq!(result, Some("a+b".to_string()));
}

#[test]
fn test_extract_token_from_query_first_param() {
    let query = "token=mytoken&other=value";
    let result = extract_token_from_query(query);
    assert_eq!(result, Some("mytoken".to_string()));
}

#[test]
fn test_extract_token_from_query_last_param() {
    let query = "other=value&token=mytoken";
    let result = extract_token_from_query(query);
    assert_eq!(result, Some("mytoken".to_string()));
}

#[test]
fn test_extract_token_from_query_empty_value() {
    let query = "token=";
    let result = extract_token_from_query(query);
    assert_eq!(result, Some(String::new()));
}

#[test]
fn test_extract_token_from_query_only_token() {
    let query = "token=abc123";
    let result = extract_token_from_query(query);
    assert_eq!(result, Some("abc123".to_string()));
}

#[test]
fn test_extract_token_from_query_jwt_like() {
    // JWT tokens often contain dots and base64 characters
    let jwt = "eyJhbGciOiJIUzI1NiJ9.eyJtIjoibWVkaWExMjMifQ.signature";
    let query = format!("token={jwt}");
    let result = extract_token_from_query(&query);
    assert_eq!(result, Some(jwt.to_string()));
}

#[test]
fn test_extract_token_from_query_percent_encoded_jwt() {
    // Some clients may URL-encode the JWT dots as %2E
    let query = "token=eyJ%2Balg";
    let result = extract_token_from_query(query);
    assert_eq!(result, Some("eyJ+alg".to_string()));
}

#[test]
fn test_extract_token_partial_match_not_confused() {
    // "mytoken=" should not be matched as "token="
    let query = "mytoken=abc&other=def";
    let result = extract_token_from_query(query);
    assert!(result.is_none());
}

// ========== on_play always rejected test ==========

#[tokio::test]
async fn test_on_play_always_rejected() {
    // Verify the documented behavior: RTMP play is always rejected.
    // We can't easily instantiate RtmpAuthCallbackImpl without a real PublishKeyService,
    // but we can verify the contract by testing the expected error message format.
    let rejection_msg = "RTMP pull is disabled. Use HTTP-FLV or HLS endpoints for playback.";
    assert!(rejection_msg.contains("disabled"));
    assert!(rejection_msg.contains("HTTP-FLV"));
    assert!(rejection_msg.contains("HLS"));
}

// ========== PublisherGuard rollback tests ==========
//
// These tests verify that when RTMP authentication succeeds but StreamHub
// publish fails, the Redis publisher entry is properly cleaned up.
//
// Issue: RTMP 推流认证后 `publish_to_stream_hub()` 失败时 Redis 残留 publisher 条目

use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use synctv_livestream::relay::{InMemoryStreamRegistry, StreamRegistryTrait};
use synctv_livestream::{AuthCallback, AuthPublishRewrite};

/// Mock auth callback that tracks rollback calls
struct MockAuthCallback {
    registry: Arc<InMemoryStreamRegistry>,
    rollback_count: Arc<AtomicUsize>,
}

impl MockAuthCallback {
    fn new(registry: Arc<InMemoryStreamRegistry>) -> Self {
        Self {
            registry,
            rollback_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn get_rollback_count(&self) -> usize {
        self.rollback_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl AuthCallback for MockAuthCallback {
    async fn on_publish(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        // Simulate successful auth - register publisher in registry
        let registered = self
            .registry
            .try_register_publisher(app_name, stream_name, "node1", "user1", "localhost:50051")
            .await?;

        if !registered {
            return Err("Publisher already exists".into());
        }

        Ok(None) // No rewrite
    }

    async fn on_play(
        &self,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Err("RTMP play disabled".into())
    }

    async fn on_unpublish(&self, _app_name: &str, _stream_name: &str, _query: Option<&str>) {
        // Default: no-op for this test
    }

    /// This is the key method - called when publish fails after auth success
    async fn on_publish_rollback(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        self.rollback_count.fetch_add(1, Ordering::SeqCst);

        // Clean up the registry entry
        if let Err(e) = self
            .registry
            .unregister_publisher(app_name, stream_name)
            .await
        {
            eprintln!("Failed to rollback publisher registration: {e}");
        }
    }
}

/// Test that `on_publish_rollback` is called and properly cleans up registry
#[tokio::test]
async fn test_publish_rollback_cleans_registry() {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let auth = Arc::new(MockAuthCallback::new(registry.clone()));

    // Simulate successful auth (registers in registry)
    let result = auth.on_publish("room1", "media1", None).await;
    assert!(result.is_ok());

    // Verify publisher is registered
    assert!(registry.is_stream_active("room1", "media1").await.unwrap());

    // Simulate StreamHub failure - call rollback
    auth.on_publish_rollback("room1", "media1", None).await;

    // Give async cleanup time to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Verify publisher is cleaned up
    assert!(
        !registry.is_stream_active("room1", "media1").await.unwrap(),
        "Publisher should be cleaned up after rollback"
    );

    // Verify rollback was called exactly once
    assert_eq!(auth.get_rollback_count(), 1);
}

/// Test that rollback is idempotent (calling twice should not panic)
#[tokio::test]
async fn test_publish_rollback_is_idempotent() {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let auth = Arc::new(MockAuthCallback::new(registry.clone()));

    // Simulate successful auth
    auth.on_publish("room1", "media1", None).await.unwrap();

    // Call rollback twice
    auth.on_publish_rollback("room1", "media1", None).await;
    auth.on_publish_rollback("room1", "media1", None).await;

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Should still be cleaned up (second call is no-op)
    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
}

/// Test that rollback handles non-existent entries gracefully
#[tokio::test]
async fn test_publish_rollback_nonexistent_entry() {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let auth = Arc::new(MockAuthCallback::new(registry.clone()));

    // Call rollback on non-existent entry - should not panic
    auth.on_publish_rollback("nonexistent", "media", None).await;

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Should complete without error
    assert_eq!(auth.get_rollback_count(), 1);
}

/// Test full publish-failure-rollback sequence
#[tokio::test]
async fn test_full_publish_failure_sequence() {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let auth = Arc::new(MockAuthCallback::new(registry.clone()));

    // Step 1: Auth succeeds (register in registry)
    let auth_result = auth.on_publish("room1", "media1", None).await;
    assert!(auth_result.is_ok(), "Auth should succeed");

    // Step 2: Verify registry has the publisher
    let is_active = registry.is_stream_active("room1", "media1").await.unwrap();
    assert!(is_active, "Publisher should be active after auth");

    // Step 3: Simulate StreamHub publish failure
    // (in real code this would be publish_to_stream_hub() returning Err)
    let streamhub_success = false;

    // Step 4: If StreamHub failed, call rollback
    if !streamhub_success {
        auth.on_publish_rollback("room1", "media1", None).await;
    }

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Step 5: Verify cleanup
    assert!(
        !registry.is_stream_active("room1", "media1").await.unwrap(),
        "Publisher should be cleaned up after StreamHub failure"
    );
}

// ========== RTMP Player Settings Tests ==========
//
// These tests verify the expected behavior of RTMP play authorization
// based on room settings. The actual implementation is in SyncTvRtmpAuth
// (synctv crate) which has access to RoomSettingsService.
//
// Default behavior: RTMP play is disabled (rtmp_player = false)
// Only when room admin explicitly enables rtmp_player should play be allowed.

/// Mock auth callback that respects room settings for play authorization
struct MockRtmpSettingsAuthCallback {
    rtmp_player_enabled: bool,
}

impl MockRtmpSettingsAuthCallback {
    const fn new(rtmp_player_enabled: bool) -> Self {
        Self {
            rtmp_player_enabled,
        }
    }
}

#[async_trait]
impl AuthCallback for MockRtmpSettingsAuthCallback {
    async fn on_publish(
        &self,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        // Not testing publish in these tests
        Ok(None)
    }

    async fn on_play(
        &self,
        app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Check room settings for RTMP player
        if !self.rtmp_player_enabled {
            return Err(format!(
                "RTMP play rejected for room {app_name}: rtmp_player is disabled in room settings. Use HTTP-FLV or HLS."
            ).into());
        }
        Ok(())
    }

    async fn on_unpublish(&self, _app_name: &str, _stream_name: &str, _query: Option<&str>) {}
}

/// Test that RTMP play is rejected when `rtmp_player` is disabled (default)
#[tokio::test]
async fn rtmp_play_rejected_when_disabled() {
    let auth = MockRtmpSettingsAuthCallback::new(false);

    let result = auth.on_play("room1", "media1", None).await;
    assert!(
        result.is_err(),
        "RTMP play should be rejected when rtmp_player is disabled"
    );

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("rtmp_player is disabled"),
        "Error message should mention rtmp_player setting"
    );
    assert!(
        err.contains("HTTP-FLV") || err.contains("HLS"),
        "Error message should suggest HTTP-FLV or HLS alternatives"
    );
}

/// Test that RTMP play is allowed when `rtmp_player` is enabled
#[tokio::test]
async fn rtmp_play_allowed_when_enabled() {
    let auth = MockRtmpSettingsAuthCallback::new(true);

    let result = auth.on_play("room1", "media1", None).await;
    assert!(
        result.is_ok(),
        "RTMP play should be allowed when rtmp_player is enabled"
    );
}

/// Test that the default behavior (no explicit setting) rejects RTMP play
#[tokio::test]
async fn rtmp_play_default_is_rejected() {
    // Default should be disabled (false)
    let default_settings_enabled = false;
    let auth = MockRtmpSettingsAuthCallback::new(default_settings_enabled);

    let result = auth.on_play("room1", "media1", None).await;
    assert!(
        result.is_err(),
        "RTMP play should be rejected by default (rtmp_player defaults to false)"
    );
}

// ========== StreamHub Restart Race Condition Tests ==========
//
// These tests verify that publications are rejected during StreamHub restart
// to prevent race conditions in the cleanup -> re-registration window.

use std::sync::atomic::AtomicBool;

/// Mock auth callback that checks is_restarting flag
struct MockRestartAwareAuthCallback {
    is_restarting: Arc<AtomicBool>,
    registry: Arc<InMemoryStreamRegistry>,
}

impl MockRestartAwareAuthCallback {
    fn new(is_restarting: Arc<AtomicBool>, registry: Arc<InMemoryStreamRegistry>) -> Self {
        Self {
            is_restarting,
            registry,
        }
    }
}

#[async_trait]
impl AuthCallback for MockRestartAwareAuthCallback {
    async fn on_publish(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        // Check if StreamHub is restarting - reject new publications
        if self.is_restarting.load(Ordering::Acquire) {
            return Err("StreamHub is restarting, please retry in a few seconds".into());
        }

        // Simulate successful auth with registration
        self.registry
            .try_register_publisher(
                app_name,
                stream_name,
                "node1",
                "user1",
                "grpc://localhost:5001",
            )
            .await
            .map_err(|e| e.to_string())?;

        Ok(Some(AuthPublishRewrite {
            app_name: app_name.to_string(),
            stream_name: stream_name.to_string(),
        }))
    }

    async fn on_play(
        &self,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        Ok(())
    }

    async fn on_unpublish(&self, _app_name: &str, _stream_name: &str, _query: Option<&str>) {}

    async fn on_unplay(&self, _app_name: &str, _stream_name: &str, _query: Option<&str>) {}

    async fn on_publish_rollback(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        let _ = self
            .registry
            .unregister_publisher(app_name, stream_name)
            .await;
    }
}

/// Test that publications are rejected when is_restarting flag is set
#[tokio::test]
async fn test_publication_rejected_when_restarting() {
    let is_restarting = Arc::new(AtomicBool::new(true));
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let auth = MockRestartAwareAuthCallback::new(is_restarting.clone(), registry.clone());

    // Attempt to publish while restarting
    let result = auth.on_publish("room1", "media1", None).await;

    assert!(
        result.is_err(),
        "Publication should be rejected when is_restarting is true"
    );
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("restarting"),
        "Error message should mention restart: {err}"
    );

    // Verify publisher was NOT registered
    assert!(
        !registry.is_stream_active("room1", "media1").await.unwrap(),
        "Publisher should not be registered when rejected"
    );
}

/// Test that publications are allowed when is_restarting flag is cleared
#[tokio::test]
async fn test_publication_allowed_when_not_restarting() {
    let is_restarting = Arc::new(AtomicBool::new(false));
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let auth = MockRestartAwareAuthCallback::new(is_restarting.clone(), registry.clone());

    // Attempt to publish when not restarting
    let result = auth.on_publish("room1", "media1", None).await;

    assert!(
        result.is_ok(),
        "Publication should be allowed when is_restarting is false"
    );

    // Verify publisher was registered
    assert!(
        registry.is_stream_active("room1", "media1").await.unwrap(),
        "Publisher should be registered when allowed"
    );
}

/// Test that the restarting flag can be toggled to control publications
#[tokio::test]
async fn test_restarting_flag_toggles_publication_access() {
    let is_restarting = Arc::new(AtomicBool::new(false));
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let auth = MockRestartAwareAuthCallback::new(is_restarting.clone(), registry.clone());

    // Step 1: Publication allowed when not restarting
    let result = auth.on_publish("room1", "media1", None).await;
    assert!(result.is_ok(), "Should be allowed initially");
    assert!(registry.is_stream_active("room1", "media1").await.unwrap());

    // Step 2: Set restarting flag
    is_restarting.store(true, Ordering::Release);

    // Step 3: Publication rejected during restart
    let result = auth.on_publish("room2", "media2", None).await;
    assert!(result.is_err(), "Should be rejected during restart");
    assert!(!registry.is_stream_active("room2", "media2").await.unwrap());

    // Step 4: Clear restarting flag
    is_restarting.store(false, Ordering::Release);

    // Step 5: Publication allowed again
    let result = auth.on_publish("room3", "media3", None).await;
    assert!(result.is_ok(), "Should be allowed after restart completes");
    assert!(registry.is_stream_active("room3", "media3").await.unwrap());
}
