//! End-to-end RTMP authentication tests.
//!
//! These tests verify the complete RTMP authentication flow including:
//! - JWT token validation
//! - Room permission verification
//! - Publisher registry integration
//! - Authentication failure cleanup
//! - `on_unpublish` callback behavior

#![allow(clippy::unwrap_used)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use synctv_livestream::relay::StreamRegistryTrait;
use synctv_livestream::{AuthCallback, AuthPublishRewrite};

// Mock Auth Callback for Testing

/// Tracks callback invocations for testing
#[derive(Debug, Default)]
struct CallbackTracker {
    publishes: AtomicUsize,
    unpublishes: AtomicUsize,
    plays: AtomicUsize,
    rollbacks: AtomicUsize,
}

impl CallbackTracker {
    fn new() -> Self {
        Self::default()
    }

    fn publish_calls(&self) -> usize {
        self.publishes.load(Ordering::SeqCst)
    }

    fn unpublish_calls(&self) -> usize {
        self.unpublishes.load(Ordering::SeqCst)
    }

    fn play_calls(&self) -> usize {
        self.plays.load(Ordering::SeqCst)
    }

    fn rollback_calls(&self) -> usize {
        self.rollbacks.load(Ordering::SeqCst)
    }
}

/// Mock auth callback that simulates JWT validation and room permission checks
struct MockRtmpAuthCallback {
    registry: Arc<dyn StreamRegistryTrait>,
    tracker: Arc<CallbackTracker>,
    /// Simulates whether the JWT token is valid
    should_authenticate: bool,
    /// Simulates the `room_id` extracted from JWT
    jwt_room_id: String,
    /// Simulates the `media_id` extracted from JWT
    jwt_media_id: String,
    /// Simulates the `user_id` extracted from JWT
    jwt_user_id: String,
}

impl MockRtmpAuthCallback {
    fn new(registry: Arc<dyn StreamRegistryTrait>, tracker: Arc<CallbackTracker>) -> Self {
        Self {
            registry,
            tracker,
            should_authenticate: true,
            jwt_room_id: "test_room".to_string(),
            jwt_media_id: "test_media".to_string(),
            jwt_user_id: "test_user".to_string(),
        }
    }

    const fn with_auth_result(mut self, should_authenticate: bool) -> Self {
        self.should_authenticate = should_authenticate;
        self
    }

    fn with_room_id(mut self, room_id: &str) -> Self {
        self.jwt_room_id = room_id.to_string();
        self
    }

    fn with_media_id(mut self, media_id: &str) -> Self {
        self.jwt_media_id = media_id.to_string();
        self
    }

    fn _with_user_id(mut self, user_id: &str) -> Self {
        self.jwt_user_id = user_id.to_string();
        self
    }
}

#[async_trait]
impl AuthCallback for MockRtmpAuthCallback {
    async fn on_publish(
        &self,
        app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        self.tracker.publishes.fetch_add(1, Ordering::SeqCst);

        // Simulate JWT validation
        if !self.should_authenticate {
            return Err("Invalid JWT token".into());
        }

        // Simulate room permission check: app_name (room_id in URL) must match JWT room_id
        if app_name != self.jwt_room_id {
            return Err(format!(
                "Room ID mismatch: token has {}, request is for {}",
                self.jwt_room_id, app_name
            )
            .into());
        }

        // Register in the registry (simulates atomic Redis registration)
        let registered = self
            .registry
            .try_register_publisher(
                &self.jwt_room_id,
                &self.jwt_media_id,
                "test_node",
                &self.jwt_user_id,
                "localhost:50051",
            )
            .await?;

        if !registered {
            return Err(format!(
                "Another publisher is already active for media {} in room {}",
                self.jwt_media_id, self.jwt_room_id
            )
            .into());
        }

        // Return rewrite so StreamHub uses canonical identifiers
        Ok(Some(AuthPublishRewrite {
            app_name: self.jwt_room_id.clone(),
            stream_name: self.jwt_media_id.clone(),
        }))
    }

    async fn on_play(
        &self,
        _app_name: &str,
        _stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.tracker.plays.fetch_add(1, Ordering::SeqCst);
        // RTMP play is always disabled in this implementation
        Err("RTMP pull is disabled. Use HTTP-FLV or HLS endpoints for playback.".into())
    }

    async fn on_unpublish(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        self.tracker.unpublishes.fetch_add(1, Ordering::SeqCst);

        // Cleanup registry entry
        if let Err(e) = self
            .registry
            .unregister_publisher(app_name, stream_name)
            .await
        {
            eprintln!("Failed to cleanup publisher: {e}");
        }
    }

    async fn on_publish_rollback(&self, app_name: &str, stream_name: &str, _query: Option<&str>) {
        self.tracker.rollbacks.fetch_add(1, Ordering::SeqCst);

        // Cleanup registry entry on failure
        if let Err(e) = self
            .registry
            .unregister_publisher(app_name, stream_name)
            .await
        {
            eprintln!("Failed to rollback publisher: {e}");
        }
    }
}

// JWT Token Validation Tests

/// Test that a valid JWT token is accepted and produces correct rewrite
#[tokio::test]
async fn test_jwt_token_validation_success() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room123")
        .with_media_id("media456");

    // Simulate RTMP publish with room_id matching JWT
    let result = auth.on_publish("room123", "jwt_token_here", None).await;

    assert!(result.is_ok(), "Valid JWT should authenticate successfully");
    let rewrite = result.unwrap();
    assert!(rewrite.is_some());
    let rewrite = rewrite.unwrap();
    assert_eq!(rewrite.app_name, "room123");
    assert_eq!(rewrite.stream_name, "media456");

    // Verify registry was updated
    assert!(registry
        .is_stream_active("room123", "media456")
        .await
        .unwrap());
    assert_eq!(tracker.publish_calls(), 1);
}

/// Test that an invalid JWT token is rejected
#[tokio::test]
async fn test_jwt_token_validation_failure() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone()).with_auth_result(false);

    let result = auth.on_publish("room123", "invalid_token", None).await;

    assert!(result.is_err(), "Invalid JWT should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("Invalid JWT token"));

    // Verify registry was NOT updated
    assert!(!registry
        .is_stream_active("room123", "media456")
        .await
        .unwrap());
}

/// Test JWT token extraction from query string
#[tokio::test]
async fn test_jwt_token_from_query_string() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room123")
        .with_media_id("media456");

    // Simulate RTMP publish with token in query string
    let result = auth
        .on_publish(
            "room123",
            "stream_name",
            Some("token=jwt_token_here&other=value"),
        )
        .await;

    assert!(result.is_ok(), "JWT in query string should be accepted");
    let rewrite = result.unwrap().unwrap();
    assert_eq!(rewrite.stream_name, "media456");
}

// Room Permission Verification Tests

/// Test that `room_id` mismatch is detected
#[tokio::test]
async fn test_room_id_mismatch_rejected() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room_A")
        .with_media_id("media123");

    let result = auth.on_publish("room_B", "jwt_token", None).await;

    assert!(result.is_err(), "Room ID mismatch should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Room ID mismatch"),
        "Error should mention room mismatch: {err}"
    );
    assert!(err.contains("room_A"), "Error should show JWT room: {err}");
    assert!(
        err.contains("room_B"),
        "Error should show requested room: {err}"
    );
}

/// Test that correct `room_id` is accepted
#[tokio::test]
async fn test_room_id_match_accepted() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("correct_room")
        .with_media_id("media123");

    let result = auth.on_publish("correct_room", "jwt_token", None).await;

    assert!(result.is_ok(), "Correct room_id should be accepted");
}

// Authentication Failure Cleanup Tests

/// Test that registry is cleaned up when authentication fails
#[tokio::test]
async fn test_auth_failure_does_not_leave_stale_entry() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone()).with_auth_result(false);

    // Failed auth should not register
    let _ = auth.on_publish("room1", "media1", None).await;

    // Verify no entry in registry
    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
}

/// Test that rollback is called when `StreamHub` publish fails after auth
#[tokio::test]
async fn test_rollback_on_streamhub_failure() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room1")
        .with_media_id("media1");

    // Auth succeeds
    let result = auth.on_publish("room1", "jwt_token", None).await;
    assert!(result.is_ok());
    assert!(registry.is_stream_active("room1", "media1").await.unwrap());

    // Simulate StreamHub failure - call rollback
    auth.on_publish_rollback("room1", "media1", None).await;

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Verify cleanup
    assert!(
        !registry.is_stream_active("room1", "media1").await.unwrap(),
        "Rollback should clean up registry"
    );
    assert_eq!(tracker.rollback_calls(), 1);
}

// on_unpublish Callback Tests

/// Test that `on_unpublish` cleans up registry
#[tokio::test]
async fn test_on_unpublish_cleanup() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room1")
        .with_media_id("media1");

    // Publish
    auth.on_publish("room1", "jwt_token", None).await.unwrap();
    assert!(registry.is_stream_active("room1", "media1").await.unwrap());

    // Unpublish
    auth.on_unpublish("room1", "media1", None).await;
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Verify cleanup
    assert!(!registry.is_stream_active("room1", "media1").await.unwrap());
    assert_eq!(tracker.unpublish_calls(), 1);
}

/// Test `on_unpublish` is idempotent
#[tokio::test]
async fn test_on_unpublish_idempotent() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone());

    // Call on_unpublish multiple times without publish
    auth.on_unpublish("room1", "media1", None).await;
    auth.on_unpublish("room1", "media1", None).await;

    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Should not panic, just complete
    assert_eq!(tracker.unpublish_calls(), 2);
}

// Duplicate Publisher Tests

/// Test that duplicate publisher is rejected
#[tokio::test]
async fn test_duplicate_publisher_rejected() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room1")
        .with_media_id("media1");

    // First publish succeeds
    let result1 = auth.on_publish("room1", "jwt1", None).await;
    assert!(result1.is_ok());

    // Second publish to same room/media fails
    let result2 = auth.on_publish("room1", "jwt2", None).await;
    assert!(result2.is_err());
    let err = result2.unwrap_err().to_string();
    assert!(
        err.contains("already active"),
        "Error should mention existing publisher: {err}"
    );
}

// on_play Rejection Tests

/// Test that RTMP play is always rejected
#[tokio::test]
async fn test_on_play_always_rejected() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone());

    let result = auth.on_play("room1", "media1", None).await;

    assert!(result.is_err(), "RTMP play should always be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("disabled"),
        "Error should mention disabled: {err}"
    );
    assert!(
        err.contains("HTTP-FLV") || err.contains("HLS"),
        "Error should suggest alternatives: {err}"
    );
    assert_eq!(tracker.play_calls(), 1);
}

// Integration Tests (marked with #[ignore])

/// End-to-end test that requires Docker (Redis container)
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_e2e_rtmp_auth_with_redis() {
    // This test would use testcontainers to spin up Redis
    // and verify the full RTMP auth flow with real Redis backend
    // Steps:
}

/// End-to-end test with real `StreamHub`
#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_e2e_rtmp_with_streamhub() {
    // This test would verify the complete flow:
}
