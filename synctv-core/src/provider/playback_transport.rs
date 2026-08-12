use std::collections::{HashMap, HashSet};
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
    /// Try equivalent upstream URLs in order and commit the downstream
    /// response only after the selected candidate produces its first body data.
    FetchAndForwardCandidates {
        urls: Vec<String>,
        headers: HashMap<String, String>,
        range_header: Option<String>,
    },
    /// Fetch an M3U8 manifest, rewrite internal upstream targets, then forward.
    M3u8Rewrite {
        url: String,
        headers: HashMap<String, String>,
    },
    /// Fetch an M3U8 manifest while resolving its child references against a
    /// stable provider-owned path instead of the temporary upstream URL.
    M3u8RewriteWithSource {
        url: String,
        headers: HashMap<String, String>,
        source_url: String,
    },
    /// Fetch an MPEG-DASH MPD manifest, rewrite its resource scopes, then forward.
    MpdRewrite {
        url: String,
        headers: HashMap<String, String>,
    },
    /// Rewrite an already generated MPEG-DASH MPD body through the signed
    /// resource pipeline.
    MpdBodyRewrite { body: Vec<u8>, source_url: String },
    /// Rewrite an already generated M3U8 body through the normal signed segment pipeline.
    M3u8BodyRewrite { body: Vec<u8> },
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
    /// Generate an HLS master playlist that resolves the active stream generation.
    LiveHlsMaster {
        provider_name: String,
        room_id: RoomId,
        media_id: MediaId,
        version: String,
    },
    /// Generate the media playlist for one immutable stream generation.
    LiveHlsPlaylist {
        provider_name: String,
        room_id: RoomId,
        media_id: MediaId,
        version: String,
        generation_id: String,
    },
    /// Serve a live HLS segment through a transport adapter.
    LiveHlsSegment {
        provider_name: String,
        room_id: RoomId,
        media_id: MediaId,
        generation_id: String,
        segment_name: String,
        disguised_as_png: bool,
    },
}

impl PlaybackTransportAction {
    #[must_use]
    pub const fn bypasses_unary_timeout(&self) -> bool {
        matches!(
            self,
            Self::LiveFlv { .. }
                | Self::LiveHlsMaster { .. }
                | Self::LiveHlsPlaylist { .. }
                | Self::LiveHlsSegment { .. }
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

#[derive(Debug, Clone, Copy)]
pub struct HlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

pub(crate) fn hls_target_headers(
    root_url: &str,
    target_url: &str,
    mut headers: HashMap<String, String>,
) -> HashMap<String, String> {
    if urls_have_same_origin(root_url, target_url) {
        return headers;
    }
    headers.retain(|name, _| {
        !matches!(
            name.to_ascii_lowercase().as_str(),
            "authorization" | "cookie" | "proxy-authorization"
        )
    });
    headers
}

fn urls_have_same_origin(left: &str, right: &str) -> bool {
    let Ok(left) = url::Url::parse(left) else {
        return false;
    };
    let Ok(right) = url::Url::parse(right) else {
        return false;
    };
    left.origin().ascii_serialization() == right.origin().ascii_serialization()
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

const DYNAMIC_HLS_SOURCE_ORIGIN: &str = "https://dynamic-hls.synctv.invalid";
const STORAGE_HLS_SOURCE_ORIGIN: &str = "https://storage-hls.synctv.invalid";

pub(crate) fn dynamic_hls_source_url(root_url: &str) -> Result<String, ProviderError> {
    let root = url::Url::parse(root_url).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid dynamic HLS root URL: {error}"))
    })?;
    if !matches!(root.scheme(), "http" | "https") {
        return Err(ProviderError::InvalidConfig(
            "Dynamic HLS root URL must use HTTP or HTTPS".to_string(),
        ));
    }
    let mut source = url::Url::parse(DYNAMIC_HLS_SOURCE_ORIGIN)
        .map_err(|error| ProviderError::Internal(error.to_string()))?;
    source.set_path(root.path());
    Ok(source.to_string())
}

pub(crate) fn resolve_dynamic_hls_target(
    original_root_url: &str,
    refreshed_root_url: &str,
    target_url: &str,
) -> Result<String, ProviderError> {
    let original_root = url::Url::parse(original_root_url).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid original dynamic HLS root URL: {error}"))
    })?;
    let refreshed_root = url::Url::parse(refreshed_root_url).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid refreshed dynamic HLS root URL: {error}"))
    })?;
    let source = url::Url::parse(&dynamic_hls_source_url(original_root_url)?)
        .map_err(|error| ProviderError::Internal(error.to_string()))?;
    let target = url::Url::parse(target_url).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid dynamic HLS target URL: {error}"))
    })?;
    if target.origin().ascii_serialization() != source.origin().ascii_serialization() {
        return Ok(target.to_string());
    }
    if target.port().is_some() || !target.username().is_empty() || target.password().is_some() {
        return Err(ProviderError::InvalidConfig(
            "Dynamic HLS target escaped its signed provider scope".to_string(),
        ));
    }
    let relative = source.make_relative(&target).ok_or_else(|| {
        ProviderError::InvalidConfig(
            "Dynamic HLS target cannot be mapped to its refreshed root".to_string(),
        )
    })?;
    let mut resolved = refreshed_root.join(&relative).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid refreshed dynamic HLS target: {error}"))
    })?;
    refresh_dynamic_hls_query(&original_root, &refreshed_root, &target, &mut resolved);
    Ok(resolved.to_string())
}

fn refresh_dynamic_hls_query(
    original_root: &url::Url,
    refreshed_root: &url::Url,
    target: &url::Url,
    resolved: &mut url::Url,
) {
    let original_values = query_values_by_key(original_root);
    let refreshed_values = query_values_by_key(refreshed_root);
    let rotating_keys = original_values
        .iter()
        .filter_map(|(key, values)| {
            refreshed_values
                .get(key)
                .is_some_and(|refreshed| refreshed != values)
                .then_some(key.as_str())
        })
        .collect::<HashSet<_>>();
    if rotating_keys.is_empty() {
        return;
    }

    let target_pairs = target
        .query_pairs()
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect::<Vec<_>>();

    let mut emitted = HashSet::new();
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in target_pairs {
        if rotating_keys.contains(key.as_str()) {
            if emitted.insert(key.clone()) {
                if let Some(values) = refreshed_values.get(&key) {
                    for refreshed in values {
                        serializer.append_pair(&key, refreshed);
                    }
                }
            }
        } else {
            serializer.append_pair(&key, &value);
        }
    }
    for (key, _) in refreshed_root.query_pairs() {
        if rotating_keys.contains(key.as_ref()) && emitted.insert(key.to_string()) {
            if let Some(values) = refreshed_values.get(key.as_ref()) {
                for refreshed in values {
                    serializer.append_pair(key.as_ref(), refreshed);
                }
            }
        }
    }
    let query = serializer.finish();
    resolved.set_query((!query.is_empty()).then_some(query.as_str()));
}

fn query_values_by_key(url: &url::Url) -> HashMap<String, Vec<String>> {
    let mut values = HashMap::<String, Vec<String>>::new();
    for (key, value) in url.query_pairs() {
        values
            .entry(key.into_owned())
            .or_default()
            .push(value.into_owned());
    }
    values
}

pub(crate) fn transport_action_for_dynamic_hls_target(
    original_root_url: &str,
    refreshed_root_url: &str,
    headers: HashMap<String, String>,
    target_url: &str,
    is_manifest: bool,
    range_header: Option<&str>,
) -> Result<PlaybackTransportAction, ProviderError> {
    let resolved_url =
        resolve_dynamic_hls_target(original_root_url, refreshed_root_url, target_url)?;
    let headers = hls_target_headers(refreshed_root_url, &resolved_url, headers);
    if is_manifest {
        return Ok(PlaybackTransportAction::M3u8RewriteWithSource {
            url: resolved_url,
            headers,
            source_url: target_url.to_string(),
        });
    }
    Ok(PlaybackTransportAction::FetchAndForward {
        url: resolved_url,
        headers,
        range_header: range_header.map(ToString::to_string),
    })
}

pub(crate) fn transport_action_for_storage_hls_target(
    url: String,
    headers: HashMap<String, String>,
    resource_path: &str,
    is_manifest: bool,
    range_header: Option<&str>,
) -> Result<PlaybackTransportAction, ProviderError> {
    if is_manifest {
        return Ok(PlaybackTransportAction::M3u8RewriteWithSource {
            url,
            headers,
            source_url: storage_hls_source_url(resource_path)?,
        });
    }
    Ok(PlaybackTransportAction::FetchAndForward {
        url,
        headers,
        range_header: range_header.map(ToString::to_string),
    })
}

pub(crate) fn storage_hls_source_url(path: &str) -> Result<String, ProviderError> {
    let path = normalized_storage_hls_path(path)?;
    let mut source = url::Url::parse(STORAGE_HLS_SOURCE_ORIGIN)
        .map_err(|error| ProviderError::Internal(error.to_string()))?;
    source.set_path(&path);
    Ok(source.to_string())
}

pub(crate) fn storage_hls_resource_path(
    root_manifest_path: &str,
    target_url: &str,
) -> Result<String, ProviderError> {
    let target = url::Url::parse(target_url).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid storage HLS resource URL: {error}"))
    })?;
    if target.scheme() != "https"
        || target.host_str() != Some("storage-hls.synctv.invalid")
        || target.port().is_some()
        || !target.username().is_empty()
        || target.password().is_some()
    {
        return Err(ProviderError::InvalidConfig(
            "Storage HLS resource escaped its signed provider scope".to_string(),
        ));
    }
    normalized_storage_hls_path(root_manifest_path)?;
    let target = normalized_storage_hls_path(target.path())?;
    Ok(target)
}

fn normalized_storage_hls_path(path: &str) -> Result<String, ProviderError> {
    let decoded = percent_encoding::percent_decode_str(path)
        .decode_utf8()
        .map_err(|_| {
            ProviderError::InvalidConfig(
                "Storage HLS path contains invalid UTF-8 encoding".to_string(),
            )
        })?
        .into_owned();
    if decoded.contains('\\')
        || decoded.chars().any(char::is_control)
        || decoded.split('/').any(|segment| segment == "..")
    {
        return Err(ProviderError::InvalidConfig(
            "Storage HLS path contains an invalid segment".to_string(),
        ));
    }
    let trimmed = decoded.trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(ProviderError::InvalidConfig(
            "Storage HLS path must identify a resource".to_string(),
        ));
    }
    Ok(format!("/{trimmed}"))
}

pub(crate) fn resolve_dash_scope_target(
    scope_url: &str,
    resource_path: &str,
    resource_query: Option<&str>,
) -> Result<String, ProviderError> {
    let scope = url::Url::parse(scope_url).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid DASH scope URL: {error}"))
    })?;
    if !matches!(scope.scheme(), "http" | "https") {
        return Err(ProviderError::InvalidConfig(
            "DASH scope URL must use HTTP or HTTPS".to_string(),
        ));
    }
    validate_dash_resource_path(resource_path)?;
    if resource_path.is_empty() {
        return Ok(scope.to_string());
    }
    if !scope.path().ends_with('/') {
        return Err(ProviderError::InvalidConfig(
            "DASH exact resource scope does not accept a child path".to_string(),
        ));
    }

    let mut relative = resource_path.to_string();
    if let Some(query) = resource_query.filter(|query| !query.is_empty()) {
        relative.push('?');
        relative.push_str(query);
    }
    let target = scope.join(&relative).map_err(|error| {
        ProviderError::InvalidConfig(format!("Invalid DASH resource URL: {error}"))
    })?;
    if target.scheme() != scope.scheme()
        || target.host_str() != scope.host_str()
        || target.port_or_known_default() != scope.port_or_known_default()
    {
        return Err(ProviderError::InvalidConfig(
            "DASH resource escaped its signed origin scope".to_string(),
        ));
    }
    if !target.path().starts_with(scope.path()) {
        return Err(ProviderError::InvalidConfig(
            "DASH resource escaped its signed path scope".to_string(),
        ));
    }
    Ok(target.to_string())
}

fn validate_dash_resource_path(resource_path: &str) -> Result<(), ProviderError> {
    let mut decoded = resource_path.to_string();
    loop {
        let next = percent_encoding::percent_decode_str(&decoded)
            .decode_utf8()
            .map_err(|_| {
                ProviderError::InvalidConfig(
                    "DASH resource path contains invalid UTF-8 encoding".to_string(),
                )
            })?
            .into_owned();
        if next == decoded {
            break;
        }
        decoded = next;
    }
    if decoded.starts_with('/')
        || decoded.starts_with('\\')
        || decoded.contains('\\')
        || decoded.split('/').any(|segment| segment == "..")
    {
        return Err(ProviderError::InvalidConfig(
            "DASH resource path must stay within its signed path scope".to_string(),
        ));
    }
    Ok(())
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

    #[test]
    fn storage_hls_paths_stay_within_the_signed_provider_namespace() {
        let root = "/Videos/Show/master.m3u8";
        assert_eq!(
            storage_hls_resource_path(
                root,
                "https://storage-hls.synctv.invalid/Videos/Show/720p/index.m3u8"
            )
            .checked("nested manifest path should resolve"),
            "/Videos/Show/720p/index.m3u8"
        );
        assert_eq!(
            storage_hls_resource_path(
                root,
                "https://storage-hls.synctv.invalid/Videos/Show/segments/part%2001.m4s?token=old"
            )
            .checked("encoded media path should resolve"),
            "/Videos/Show/segments/part 01.m4s"
        );
        assert_eq!(
            storage_hls_resource_path(
                root,
                "https://storage-hls.synctv.invalid/Videos/private/key.bin"
            )
            .checked("provider-root paths should resolve"),
            "/Videos/private/key.bin"
        );
        assert!(
            storage_hls_resource_path(root, "https://example.com/Videos/Show/segment.ts").is_err()
        );
        assert_eq!(
            storage_hls_resource_path(
                root,
                "https://storage-hls.synctv.invalid/Videos/Show/%252e%252e/private/key.bin"
            )
            .checked("double-encoded names should stay literal"),
            "/Videos/Show/%2e%2e/private/key.bin"
        );
    }

    #[test]
    fn dynamic_hls_targets_follow_refreshed_root_paths_and_queries() {
        let original = "https://cdn.example/live/generation-1/master.m3u8?token=old";
        let source = dynamic_hls_source_url(original).checked("source URL should build");
        assert_eq!(
            source,
            "https://dynamic-hls.synctv.invalid/live/generation-1/master.m3u8"
        );
        let child = url::Url::parse(&source)
            .checked("source URL should parse")
            .join("720p/index.m3u8?token=intermediate")
            .checked("child URL should resolve")
            .to_string();

        assert_eq!(
            resolve_dynamic_hls_target(
                original,
                "https://cdn.example/live/generation-2/master.m3u8?token=new",
                &child,
            )
            .checked("child URL should follow the refreshed root"),
            "https://cdn.example/live/generation-2/720p/index.m3u8?token=new"
        );

        let segment = url::Url::parse(&source)
            .checked("source URL should parse")
            .join("720p/segment.ts")
            .checked("segment URL should resolve")
            .to_string();
        assert_eq!(
            resolve_dynamic_hls_target(
                original,
                "https://cdn.example/live/generation-2/master.m3u8?token=new",
                &segment,
            )
            .checked("segment should inherit the refreshed root token"),
            "https://cdn.example/live/generation-2/720p/segment.ts?token=new"
        );
    }

    #[test]
    fn dynamic_hls_targets_preserve_parent_navigation_and_external_urls() {
        let original = "https://cdn.example/live/playlists/master.m3u8";
        let source = dynamic_hls_source_url(original).checked("source URL should build");
        let sibling = url::Url::parse(&source)
            .checked("source URL should parse")
            .join("../segments/001.ts")
            .checked("sibling URL should resolve")
            .to_string();

        assert_eq!(
            resolve_dynamic_hls_target(
                original,
                "https://cdn.example/session-2/playlists/master.m3u8",
                &sibling,
            )
            .checked("sibling URL should follow the refreshed root"),
            "https://cdn.example/session-2/segments/001.ts"
        );
        assert_eq!(
            resolve_dynamic_hls_target(
                original,
                "https://cdn.example/session-2/playlists/master.m3u8",
                "https://external.example/key.bin?token=external",
            )
            .checked("external URL should remain exact"),
            "https://external.example/key.bin?token=external"
        );
    }

    #[test]
    fn storage_hls_transport_distinguishes_manifests_and_ranged_media() {
        let manifest = transport_action_for_storage_hls_target(
            "https://temporary.example/master".to_string(),
            HashMap::from([("Authorization".to_string(), "current".to_string())]),
            "/Videos/Show/master.m3u8",
            true,
            Some("bytes=0-99"),
        )
        .checked("manifest action should build");
        assert!(matches!(
            manifest,
            PlaybackTransportAction::M3u8RewriteWithSource {
                source_url,
                ..
            } if source_url
                == "https://storage-hls.synctv.invalid/Videos/Show/master.m3u8"
        ));

        let media = transport_action_for_storage_hls_target(
            "https://temporary.example/segment".to_string(),
            HashMap::new(),
            "/Videos/Show/segment.m4s",
            false,
            Some("bytes=100-199"),
        )
        .checked("media action should build");
        assert!(matches!(
            media,
            PlaybackTransportAction::FetchAndForward {
                range_header: Some(range),
                ..
            } if range == "bytes=100-199"
        ));
    }

    #[test]
    fn hls_headers_keep_sensitive_values_only_for_the_root_origin() {
        let headers = HashMap::from([
            ("Cookie".to_string(), "session=current".to_string()),
            ("Authorization".to_string(), "Bearer current".to_string()),
            (
                "Proxy-Authorization".to_string(),
                "Basic current".to_string(),
            ),
            ("Origin".to_string(), "https://live.example".to_string()),
            ("Referer".to_string(), "https://live.example/".to_string()),
        ]);

        let same_origin = hls_target_headers(
            "https://cdn.example/live/master.m3u8?token=one",
            "https://cdn.example/live/segment.ts?token=two",
            headers.clone(),
        );
        assert_eq!(
            same_origin.get("Cookie").map(String::as_str),
            Some("session=current")
        );
        assert!(same_origin.contains_key("Authorization"));

        let cross_origin = hls_target_headers(
            "https://cdn.example/live/master.m3u8",
            "https://external.example/segment.ts",
            headers,
        );
        assert!(!cross_origin.contains_key("Cookie"));
        assert!(!cross_origin.contains_key("Authorization"));
        assert!(!cross_origin.contains_key("Proxy-Authorization"));
        assert_eq!(
            cross_origin.get("Referer").map(String::as_str),
            Some("https://live.example/")
        );
        assert_eq!(
            cross_origin.get("Origin").map(String::as_str),
            Some("https://live.example")
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
                provider: crate::models::SourceProvider::DirectUrl,
                provider_instance_name: None,
                duration_seconds: None,
                playback_kind: Some(crate::models::PlaybackKind::Regular),
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
                provider: crate::models::SourceProvider::DirectUrl,
                provider_instance_name: None,
                duration_seconds: None,
                playback_kind: Some(crate::models::PlaybackKind::Regular),
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
