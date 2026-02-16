//! Common Provider Route Utilities
//!
//! Shared functionality across all provider routes

use std::collections::HashMap;

use axum::{extract::{Path, State}, routing::get, Json, Router, response::IntoResponse};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use synctv_core::models::{Media, MediaId, RoomId};
use synctv_core::provider::{MediaProvider, PlaybackResult as ProviderPlaybackResult, ProviderContext, ProviderError};

use super::AppState;
use super::error::AppError;
use super::middleware::AuthUser;

/// Register common provider routes
///
/// Routes:
/// - GET /instances - List all available provider instances
/// - GET /`backends/:provider_type` - List available backends for a provider type
pub fn register_common_routes() -> Router<AppState> {
    Router::new()
        .route("/instances", get(list_instances))
        .route("/backends/{provider_type}", get(list_backends))
}

/// List all available provider instances
async fn list_instances(_auth: AuthUser, State(state): State<AppState>) -> impl IntoResponse {
    let instances = state.provider_instance_manager.list().await;

    Json(json!({
        "instances": instances
    }))
}

/// List available backends for a given provider type (bilibili/alist/emby)
async fn list_backends(
    _auth: AuthUser,
    State(state): State<AppState>,
    Path(provider_type): Path<String>,
) -> impl IntoResponse {
    let instances = match state.provider_instance_manager.get_all_instances().await {
        Ok(all) => all
            .into_iter()
            .filter(|i| i.enabled && i.providers.iter().any(|p| p == &provider_type))
            .map(|i| i.name)
            .collect::<Vec<_>>(),
        Err(_) => vec![],
    };

    Json(json!({
        "backends": instances
    }))
}

/// Convert `ProviderError` to HTTP response
pub fn error_response(e: ProviderError) -> (StatusCode, Json<serde_json::Value>) {
    let (status, message, details) = match &e {
        ProviderError::NetworkError(msg) => (StatusCode::BAD_GATEWAY, msg.clone(), msg.clone()),
        ProviderError::ApiError(msg) => (StatusCode::BAD_GATEWAY, msg.clone(), msg.clone()),
        ProviderError::ParseError(msg) => (StatusCode::BAD_REQUEST, msg.clone(), msg.clone()),
        ProviderError::InvalidConfig(msg) => (StatusCode::BAD_REQUEST, msg.clone(), msg.clone()),
        ProviderError::NotFound => (StatusCode::NOT_FOUND, "Resource not found".to_string(), "Resource not found".to_string()),
        ProviderError::InstanceNotFound(msg) => (StatusCode::NOT_FOUND, msg.clone(), msg.clone()),
        _ => {
            tracing::error!(error = %e, "Internal provider error");
            (StatusCode::INTERNAL_SERVER_ERROR, "Provider error".to_string(), "An internal error occurred".to_string())
        }
    };

    let body = json!({
        "error": message,
        "details": details
    });

    (status, Json(body))
}

/// Convert String error to HTTP response (for implementation layer errors)
#[must_use] 
pub fn parse_provider_error(error_msg: &str) -> ProviderError {
    // Parse common error patterns and convert to ProviderError
    let lower = error_msg.to_lowercase();

    if lower.contains("network") || lower.contains("connection") {
        ProviderError::NetworkError(error_msg.to_string())
    } else if lower.contains("not found") {
        ProviderError::NotFound
    } else if lower.contains("parse") || lower.contains("invalid") {
        ProviderError::ParseError(error_msg.to_string())
    } else {
        // Unauthorized, authentication, or any other error
        ProviderError::ApiError(error_msg.to_string())
    }
}

/// Extract `instance_name` from query parameter
#[derive(Debug, Deserialize)]
pub struct InstanceQuery {
    #[serde(default)]
    pub instance_name: Option<String>,
}

impl InstanceQuery {
    #[must_use]
    pub fn as_deref(&self) -> Option<&str> {
        self.instance_name.as_deref()
    }
}

// ------------------------------------------------------------------
// Playback cache-aside helper
// ------------------------------------------------------------------

/// Default TTL for cached playback results (5 minutes).
const DEFAULT_PLAYBACK_CACHE_TTL_SECS: u64 = 300;

/// Safety margin subtracted from `expires_at`-derived TTL to avoid serving
/// URLs that are about to expire.
const CACHE_TTL_SAFETY_MARGIN_SECS: i64 = 60;

/// Very short TTL for live content (30 seconds).
const LIVE_CACHE_TTL_SECS: u64 = 30;

/// Generate playback with Redis cache-aside.
///
/// 1. Build cache key via `provider.cache_key()`
/// 2. Try Redis GET — on hit, deserialize and return if still valid
/// 3. On miss, call `provider.generate_playback()`
/// 4. Compute TTL from the minimum `expires_at` across playback infos
/// 5. Redis SET with computed TTL
/// 6. Return result
///
/// Best-effort: if Redis is unavailable, falls through to the provider call.
pub async fn cached_generate_playback(
    provider: &dyn MediaProvider,
    ctx: &ProviderContext<'_>,
    source_config: &serde_json::Value,
    redis_conn: Option<&redis::aio::ConnectionManager>,
) -> Result<ProviderPlaybackResult, ProviderError> {
    use redis::AsyncCommands;

    let cache_key = provider.cache_key(ctx, source_config);

    // 1. Try cache read
    if let Some(conn) = redis_conn {
        let mut conn = conn.clone();
        match conn.get::<_, Option<String>>(&cache_key).await {
            Ok(Some(cached_json)) => {
                match serde_json::from_str::<ProviderPlaybackResult>(&cached_json) {
                    Ok(result) => {
                        // Check if any expires_at indicates the cached data is stale
                        let now = chrono::Utc::now().timestamp();
                        let still_valid = result.playback_infos.values().all(|pi| {
                            pi.expires_at.is_none_or(|exp| exp > now + CACHE_TTL_SAFETY_MARGIN_SECS)
                        });
                        if still_valid {
                            tracing::debug!(key = %cache_key, "playback cache hit");
                            return Ok(result);
                        }
                        tracing::debug!(key = %cache_key, "playback cache expired, refreshing");
                    }
                    Err(e) => {
                        tracing::warn!(key = %cache_key, error = %e, "failed to deserialize cached playback, ignoring");
                    }
                }
            }
            Ok(None) => { /* cache miss */ }
            Err(e) => {
                tracing::warn!(key = %cache_key, error = %e, "Redis GET failed, falling through to provider");
            }
        }
    }

    // 2. Cache miss — call the provider
    let result = provider.generate_playback(ctx, source_config).await?;

    // 3. Cache the result
    if let Some(conn) = redis_conn {
        let ttl = compute_cache_ttl(&result);
        if ttl > 0 {
            let mut conn = conn.clone();
            match serde_json::to_string(&result) {
                Ok(json) => {
                    if let Err(e) = conn.set_ex::<_, _, ()>(&cache_key, &json, ttl).await {
                        tracing::warn!(key = %cache_key, error = %e, "Redis SET failed, result not cached");
                    } else {
                        tracing::debug!(key = %cache_key, ttl, "playback result cached");
                    }
                }
                Err(e) => {
                    tracing::warn!(key = %cache_key, error = %e, "failed to serialize playback result for cache");
                }
            }
        }
    }

    Ok(result)
}

/// Compute cache TTL from a `PlaybackResult`.
///
/// Uses the minimum `expires_at` across all playback infos (minus safety margin).
/// For live content, uses a very short TTL. If no `expires_at` is set, uses
/// the default TTL.
fn compute_cache_ttl(result: &ProviderPlaybackResult) -> u64 {
    // Check if this is live content
    let is_live = result
        .metadata
        .get("is_live")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if is_live {
        return LIVE_CACHE_TTL_SECS;
    }

    let now = chrono::Utc::now().timestamp();

    // Find the minimum expires_at across all playback infos
    let min_expires_at = result
        .playback_infos
        .values()
        .filter_map(|pi| pi.expires_at)
        .min();

    match min_expires_at {
        Some(exp) => {
            let remaining = exp - now - CACHE_TTL_SAFETY_MARGIN_SECS;
            if remaining <= 0 {
                0 // Already expired or about to expire, don't cache
            } else {
                // Cap at the default TTL
                (remaining as u64).min(DEFAULT_PLAYBACK_CACHE_TTL_SECS)
            }
        }
        None => DEFAULT_PLAYBACK_CACHE_TTL_SECS,
    }
}

// ------------------------------------------------------------------
// Playback cache invalidation
// ------------------------------------------------------------------

/// Delete the cached `PlaybackResult` for a media item from Redis.
///
/// Computes the cache key via `provider.cache_key()` and issues a Redis DEL.
/// Best-effort: silently ignores errors if Redis or the provider is unavailable.
pub async fn invalidate_playback_cache(
    provider: &dyn MediaProvider,
    source_config: &serde_json::Value,
    redis_conn: Option<&redis::aio::ConnectionManager>,
) {
    use redis::AsyncCommands;

    let Some(conn) = redis_conn else { return };

    // Build a minimal context — provider cache_key implementations typically
    // only use key_prefix (and sometimes user_id for the default impl).
    // We pass no user_id so the default impl produces the "anonymous" variant,
    // but all concrete providers (bilibili, alist, emby, direct_url, rtmp,
    // live_proxy) ignore ctx entirely and derive the key from source_config.
    let ctx = ProviderContext::new("synctv");
    let cache_key = provider.cache_key(&ctx, source_config);

    let mut conn = conn.clone();
    match conn.del::<_, i64>(&cache_key).await {
        Ok(n) => {
            if n > 0 {
                tracing::debug!(key = %cache_key, "playback cache invalidated");
            }
        }
        Err(e) => {
            tracing::warn!(key = %cache_key, error = %e, "failed to invalidate playback cache (best-effort)");
        }
    }
}

/// Invalidate playback cache for a batch of media items.
///
/// Best-effort: skips items whose provider cannot be resolved.
pub async fn invalidate_playback_cache_batch(
    media_items: &[synctv_core::models::Media],
    providers_manager: &synctv_core::service::ProvidersManager,
    redis_conn: Option<&redis::aio::ConnectionManager>,
) {
    if redis_conn.is_none() {
        return;
    }
    for media in media_items {
        // Skip direct-URL media (no provider-generated cache entry)
        if media.is_direct() {
            continue;
        }
        let instance_name = media
            .provider_instance_name
            .as_deref()
            .unwrap_or(&media.source_provider);
        if let Some(provider) = providers_manager.get(instance_name).await {
            invalidate_playback_cache(provider.as_ref(), &media.source_config, redis_conn).await;
        }
    }
}

// ------------------------------------------------------------------
// Shared playback resolution helpers
// ------------------------------------------------------------------

/// Verify room membership, fetch the playlist, and find a specific media item.
///
/// This is the common first phase shared by all provider proxy handlers.
pub async fn resolve_media_from_playlist(
    auth: &AuthUser,
    room_id: &RoomId,
    media_id: &MediaId,
    state: &AppState,
) -> Result<Media, AppError> {
    state
        .room_service
        .check_membership(room_id, &auth.user_id)
        .await
        .map_err(|_| AppError::forbidden("Not a member of this room"))?;

    let playlist = state
        .room_service
        .get_playlist(room_id)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to get playlist: {e}"))?;

    let media = playlist
        .into_iter()
        .find(|m| m.id == *media_id)
        .ok_or_else(|| anyhow::anyhow!("Media not found in playlist"))?;

    Ok(media)
}

/// Resolve a playback URL and headers from a `MediaProvider`.
///
/// Performs the full flow: membership check -> playlist lookup -> find media ->
/// `generate_playback` -> extract first URL + headers from the default mode.
///
/// Used by alist and emby proxy handlers.
pub async fn resolve_provider_playback_url(
    auth: &AuthUser,
    room_id: &RoomId,
    media_id: &MediaId,
    state: &AppState,
    provider: &dyn MediaProvider,
) -> Result<(String, HashMap<String, String>), AppError> {
    let media = resolve_media_from_playlist(auth, room_id, media_id, state).await?;

    let ctx = ProviderContext::new("synctv")
        .with_user_id(auth.user_id.as_str())
        .with_room_id(room_id.as_str());

    let playback_result = cached_generate_playback(
        provider,
        &ctx,
        &media.source_config,
        state.redis_conn.as_ref(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{} generate_playback failed: {e}", provider.name()))?;

    let default_mode = &playback_result.default_mode;
    let playback_info = playback_result
        .playback_infos
        .get(default_mode)
        .ok_or_else(|| anyhow::anyhow!("Default playback mode not found"))?;

    let url = playback_info
        .urls
        .first()
        .ok_or_else(|| anyhow::anyhow!("No URLs in playback info"))?;

    Ok((url.clone(), playback_info.headers.clone()))
}

/// Resolve the full `PlaybackResult` from a `MediaProvider`.
///
/// Performs the full flow: membership check -> playlist lookup -> find media ->
/// `generate_playback`.
///
/// Used by bilibili proxy handlers that need access to the complete result
/// (DASH data, multiple modes, subtitles).
pub async fn resolve_provider_playback_result(
    auth: &AuthUser,
    room_id: &RoomId,
    media_id: &MediaId,
    state: &AppState,
    provider: &dyn MediaProvider,
) -> Result<ProviderPlaybackResult, AppError> {
    let media = resolve_media_from_playlist(auth, room_id, media_id, state).await?;

    let ctx = ProviderContext::new("synctv")
        .with_user_id(auth.user_id.as_str())
        .with_room_id(room_id.as_str());

    cached_generate_playback(
        provider,
        &ctx,
        &media.source_config,
        state.redis_conn.as_ref(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("{} generate_playback failed: {e}", provider.name()).into())
}
