// RTMP Authentication implementation using xiu's AuthCallback trait
//
// JWT Claims structure (from synctv-go internal/rtmp/rtmp.go):
// type Claims struct {
//     MovieID string `json:"m"`
//     jwt.RegisteredClaims
// }
//
// RTMP URL scheme:
// - Publisher: rtmp://host/room_id/JWT_TOKEN  or  rtmp://host/room_id/media_id?token=JWT
// - Player:   rtmp://host/room_id/media_id

use synctv_xiu::rtmp::auth::{AuthCallback, AuthPublishRewrite};
use async_trait::async_trait;
use std::sync::Arc;
use synctv_core::service::PublishKeyService;
use tracing::{debug, info, warn};

pub struct RtmpAuthCallbackImpl {
    publish_key_service: Arc<PublishKeyService>,
}

impl RtmpAuthCallbackImpl {
    #[must_use]
    pub const fn new(publish_key_service: Arc<PublishKeyService>) -> Self {
        Self {
            publish_key_service,
        }
    }
}

#[async_trait]
impl AuthCallback for RtmpAuthCallbackImpl {
    async fn on_publish(
        &self,
        app_name: &str,
        stream_name: &str,
        query: Option<&str>,
    ) -> Result<Option<AuthPublishRewrite>, Box<dyn std::error::Error + Send + Sync>> {
        debug!(
            "RTMP publish auth: app={}, stream={}, query={:?}",
            app_name,
            stream_name,
            query
        );

        // Extract token: prefer query string parameter, fall back to stream_name as token
        let token = if let Some(q) = query {
            extract_token_from_query(q).unwrap_or(stream_name)
        } else {
            stream_name
        };

        let claims = self
            .publish_key_service
            .validate_publish_key(token)
            .await
            .map_err(|e| format!("Authentication failed: {e}"))?;

        // Verify room_id matches app_name
        if claims.room_id != app_name {
            return Err(format!(
                "Room ID mismatch: token has {}, request is for {}",
                claims.room_id, app_name
            )
            .into());
        }

        info!(
            "RTMP publisher authenticated: room_id={}, media_id={}, user_id={}",
            claims.room_id,
            claims.media_id,
            claims.user_id
        );

        // Return rewrite so StreamHub uses canonical (room_id, media_id)
        Ok(Some(AuthPublishRewrite {
            app_name: claims.room_id,
            stream_name: claims.media_id,
        }))
    }

    async fn on_play(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        warn!(
            "RTMP play rejected: room_id={}, media_id={} — use HTTP-FLV or HLS",
            app_name,
            stream_name
        );
        Err("RTMP pull is disabled. Use HTTP-FLV or HLS endpoints for playback.".into())
    }

    async fn on_unplay(
        &self,
        app_name: &str,
        stream_name: &str,
        _query: Option<&str>,
    ) {
        info!(
            "RTMP player disconnected: room_id={}, media_id={}",
            app_name,
            stream_name
        );
    }
}

/// Extract token from query string (e.g. "token=xxx&foo=bar" -> "xxx")
fn extract_token_from_query(query: &str) -> Option<&str> {
    for pair in query.split('&') {
        if let Some(value) = pair.strip_prefix("token=") {
            return Some(value);
        }
    }
    None
}
