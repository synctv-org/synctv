//! RTMP authentication callback contract tests.
//!
//! These tests use a mock `AuthCallback` to verify callback-level behavior:
//! - Publisher registry integration
//! - Authentication failure cleanup
//! - `on_unpublish` callback behavior

#![allow(clippy::unwrap_used)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use synctv_livestream::relay::StreamRegistryTrait;
use synctv_livestream::{AuthCallback, AuthPublishRewrite};

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

/// Mock auth callback that returns configured auth and room-match outcomes.
struct MockRtmpAuthCallback {
    registry: Arc<dyn StreamRegistryTrait>,
    tracker: Arc<CallbackTracker>,
    /// Whether the mocked auth result should allow publishing.
    should_authenticate: bool,
    /// Room ID the mock treats as authorized.
    authorized_room_id: String,
    /// Media ID used when registering the publisher.
    authorized_media_id: String,
    /// User ID used when registering the publisher.
    authorized_user_id: String,
}

impl MockRtmpAuthCallback {
    fn new(registry: Arc<dyn StreamRegistryTrait>, tracker: Arc<CallbackTracker>) -> Self {
        Self {
            registry,
            tracker,
            should_authenticate: true,
            authorized_room_id: "test_room".to_string(),
            authorized_media_id: "test_media".to_string(),
            authorized_user_id: "test_user".to_string(),
        }
    }

    const fn with_auth_result(mut self, should_authenticate: bool) -> Self {
        self.should_authenticate = should_authenticate;
        self
    }

    fn with_room_id(mut self, room_id: &str) -> Self {
        self.authorized_room_id = room_id.to_string();
        self
    }

    fn with_media_id(mut self, media_id: &str) -> Self {
        self.authorized_media_id = media_id.to_string();
        self
    }

    fn _with_user_id(mut self, user_id: &str) -> Self {
        self.authorized_user_id = user_id.to_string();
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

        if !self.should_authenticate {
            return Err("mock auth rejected publish".into());
        }

        if app_name != self.authorized_room_id {
            return Err(format!(
                "Room ID mismatch: mock authorized {}, request is for {}",
                self.authorized_room_id, app_name
            )
            .into());
        }

        // Register in the registry (simulates atomic Redis registration)
        let registered = self
            .registry
            .try_register_publisher(
                &self.authorized_room_id,
                &self.authorized_media_id,
                "test_node",
                &self.authorized_user_id,
                "localhost:50051",
            )
            .await?;

        if !registered {
            return Err(format!(
                "Another publisher is already active for media {} in room {}",
                self.authorized_media_id, self.authorized_room_id
            )
            .into());
        }

        // Return rewrite so StreamHub uses canonical identifiers
        Ok(Some(AuthPublishRewrite {
            app_name: self.authorized_room_id.clone(),
            stream_name: self.authorized_media_id.clone(),
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

// Mock Auth Callback Tests

/// Test that a accepted mock auth result is accepted and produces correct rewrite
#[tokio::test]
async fn test_mock_auth_publish_success() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room123")
        .with_media_id("media456");

    // Simulate RTMP publish with room_id matching the mock authorization
    let result = auth.on_publish("room123", "publish_token", None).await;

    assert!(result.is_ok(), "mock auth should allow publish");
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

/// Test that a rejected mock auth result is propagated.
#[tokio::test]
async fn test_mock_auth_rejection_failure() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone()).with_auth_result(false);

    let result = auth.on_publish("room123", "invalid_token", None).await;

    assert!(result.is_err(), "mock auth rejection should fail publish");
    let err = result.unwrap_err().to_string();
    assert!(err.contains("mock auth rejected publish"));

    // Verify registry was NOT updated
    assert!(!registry
        .is_stream_active("room123", "media456")
        .await
        .unwrap());
}

/// Test callback handling of a room mismatch.
#[tokio::test]
async fn test_mock_auth_ignores_query_string_token() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room123")
        .with_media_id("media456");

    // Simulate RTMP publish with query string token
    let result = auth
        .on_publish(
            "room123",
            "stream_name",
            Some("token=publish_token&other=value"),
        )
        .await;

    assert!(
        result.is_ok(),
        "mock auth should accept matching room even with query string"
    );
    let rewrite = result.unwrap().unwrap();
    assert_eq!(rewrite.stream_name, "media456");
}

// Room Match Tests

/// Test that room mismatch is detected
#[tokio::test]
async fn test_room_id_mismatch_rejected() {
    let registry = synctv_livestream::relay::local_stream_registry();
    let tracker = Arc::new(CallbackTracker::new());
    let auth = MockRtmpAuthCallback::new(registry.clone(), tracker.clone())
        .with_room_id("room_A")
        .with_media_id("media123");

    let result = auth.on_publish("room_B", "publish_token", None).await;

    assert!(result.is_err(), "Room ID mismatch should be rejected");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Room ID mismatch"),
        "Error should mention room mismatch: {err}"
    );
    assert!(
        err.contains("room_A"),
        "Error should show authorized room: {err}"
    );
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

    let result = auth.on_publish("correct_room", "publish_token", None).await;

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
    let result = auth.on_publish("room1", "publish_token", None).await;
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
    auth.on_publish("room1", "publish_token", None)
        .await
        .unwrap();
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
    let result1 = auth.on_publish("room1", "publish1", None).await;
    assert!(result1.is_ok());

    // Second publish to same room/media fails
    let result2 = auth.on_publish("room1", "publish2", None).await;
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
