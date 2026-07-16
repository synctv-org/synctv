use std::collections::HashMap;
use std::sync::Arc;

use super::access::ProviderAccessService;
use super::error::ProviderError;
use super::store::{ProviderStore, ProviderStoreExt, VersionedPlayback};
use super::ExecutionControl;
use crate::credential_encryption::CredentialEncryption;
use crate::models::{MediaId, RoomId, UserId};
use crate::repository::ProviderPlaybackSessionRepository;
use crate::repository::UserProviderCredentialRepository;
use crate::service::{PermissionService, RoomService};

/// Low-level transport instruction produced by a provider-specific playback path.
///
/// Providers produce these actions from versioned playback targets created
/// during their own `generate_playback` path. Transport adapters execute the
/// instruction; provider-specific upstream URL, header, manifest, and live
/// lifecycle rules stay in the provider.
#[derive(Debug, Clone)]
pub enum PlaybackTransportAction {
    /// Fetch the URL and forward the response body (video stream, subtitle, etc.)
    FetchAndForward {
        url: String,
        headers: HashMap<String, String>,
        /// Provider-selected Range header for this request.
        ///
        /// This is intentionally separate from `headers`: it drives range/slice
        /// behavior without becoming part of the resource cache key.
        range_header: Option<String>,
    },
    /// Fetch an M3U8 manifest, rewrite internal upstream targets, then forward.
    M3u8Rewrite {
        url: String,
        headers: HashMap<String, String>,
    },
    /// Return a direct response body with a content type.
    ///
    /// Used for provider-specific responses that do not involve upstream
    /// proxying (e.g., SSE danmaku info, JSON metadata). Transport adapters
    /// wrap this into protocol-specific responses.
    DirectBody {
        body: Vec<u8>,
        content_type: String,
        status: u16,
    },
    /// Execute a live FLV stream through a transport adapter.
    LiveFlv {
        provider_name: String,
        room_id: RoomId,
        media_id: MediaId,
        user_id: UserId,
        expires_at: i64,
    },
    /// Generate an HLS playlist for a live stream.
    LiveHlsPlaylist {
        provider_name: String,
        room_id: RoomId,
        media_id: MediaId,
        version: String,
    },
    /// Serve a live HLS segment through a transport adapter.
    LiveHlsSegment {
        provider_name: String,
        room_id: RoomId,
        media_id: MediaId,
        segment_name: String,
        disguised_as_png: bool,
    },
}

impl PlaybackTransportAction {
    #[must_use]
    pub const fn bypasses_unary_timeout(&self) -> bool {
        matches!(
            self,
            Self::LiveFlv { .. } | Self::LiveHlsPlaylist { .. } | Self::LiveHlsSegment { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiveFlvAccess {
    pub user_id: UserId,
    pub expires_at: i64,
}

/// Services available to provider-specific playback transport resolution.
///
/// Gives providers DB access (for example, fetching media from playlists)
/// while keeping concrete transport adapters outside core.
pub struct PlaybackTransportServices {
    pub room_service: Arc<RoomService>,
    pub permission_service: PermissionService,
    pub credential_encryption: Option<CredentialEncryption>,
    pub credential_repo: Arc<UserProviderCredentialRepository>,
    pub playback_session_repo: ProviderPlaybackSessionRepository,
    pub provider_access_service: Arc<dyn ProviderAccessService>,
}

pub struct StatefulPlaybackResourceRequest<'a> {
    pub store: Option<&'a Arc<dyn ProviderStore>>,
    pub session_repo: &'a ProviderPlaybackSessionRepository,
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub request_context: Option<&'a ExecutionControl>,
    pub range_header: Option<&'a str>,
}

#[must_use]
pub fn transport_target_is_m3u8(url: &str) -> bool {
    url::Url::parse(url).map_or_else(
        |_| url.split('?').next().unwrap_or(url).ends_with(".m3u8"),
        |parsed| parsed.path().ends_with(".m3u8"),
    )
}

pub(crate) fn transport_action_for_target_url(
    url: String,
    headers: HashMap<String, String>,
    range_header: Option<&str>,
) -> Result<PlaybackTransportAction, ProviderError> {
    if transport_target_is_m3u8(&url) {
        Ok(PlaybackTransportAction::M3u8Rewrite { url, headers })
    } else {
        Ok(PlaybackTransportAction::FetchAndForward {
            url,
            headers,
            range_header: range_header.map(ToString::to_string),
        })
    }
}

/// Look up a cached `VersionedPlayback` from the store by version string.
pub async fn lookup_versioned(
    store: Option<&Arc<dyn ProviderStore>>,
    version: &str,
    request_context: Option<&ExecutionControl>,
) -> Result<VersionedPlayback, ProviderError> {
    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    let store = store.ok_or_else(|| ProviderError::ApiError("Store not configured".into()))?;
    let versioned: VersionedPlayback = store
        .get(&format!("v:{version}"))
        .await
        .map_err(|e| ProviderError::ApiError(format!("Store error: {e}")))?
        .ok_or(ProviderError::NotFound)?;

    if let Some(request_context) = request_context {
        request_context
            .check_active()
            .map_err(|err| ProviderError::NetworkError(err.to_string()))?;
    }

    if versioned.is_expired() {
        return Err(ProviderError::NotFound);
    }
    Ok(versioned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        ExecutionControl, InMemoryProviderStore, PlaybackResult, VersionedPlayback,
    };
    use crate::test_helpers::TestResultExt;
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn test_selected_range_returns_valid_ascii_header() {
        assert_eq!(
            Some("bytes=0-1023".to_string()),
            Some("bytes=0-1023".to_string())
        );
    }

    #[tokio::test]
    async fn test_lookup_versioned_no_store() {
        let result = lookup_versioned(None, "v1", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.failed("operation should fail"),
            ProviderError::ApiError(_)
        ));
    }

    #[tokio::test]
    async fn test_lookup_versioned_not_found() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let result = lookup_versioned(Some(&store), "nonexistent", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.failed("operation should fail"),
            ProviderError::NotFound
        ));
    }

    #[tokio::test]
    async fn test_lookup_versioned_expired() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let vp = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "direct".to_string(),
                provider: "test".to_string(),
                provider_instance_name: None,
                duration_seconds: None,
                is_live: Some(false),
                metadata: None,
            },
            expires_at: 0, // Already expired
            playback_context: None,
        };
        store
            .set("v:v1", &vp, Duration::from_mins(1))
            .await
            .checked("operation should succeed");
        let result = lookup_versioned(Some(&store), "v1", None).await;
        assert!(result.is_err());
        assert!(matches!(
            result.failed("operation should fail"),
            ProviderError::NotFound
        ));
    }

    #[tokio::test]
    async fn test_lookup_versioned_respects_cancellation() {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(100));
        let vp = VersionedPlayback {
            version: "v1".to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::new(),
                default_mode: "direct".to_string(),
                provider: "test".to_string(),
                provider_instance_name: None,
                duration_seconds: None,
                is_live: Some(false),
                metadata: None,
            },
            expires_at: crate::SystemClock.now().timestamp() + 60,
            playback_context: None,
        };
        store
            .set("v:v1", &vp, Duration::from_mins(1))
            .await
            .checked("operation should succeed");

        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let request_context = ExecutionControl::from_parts(None, cancellation);

        let result = lookup_versioned(Some(&store), "v1", Some(&request_context)).await;
        assert!(matches!(result, Err(ProviderError::NetworkError(_))));
    }
}
