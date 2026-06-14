// Media Provider Traits
// Core interfaces for the provider system

use super::{ProviderContext, ProviderError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Subtitle track
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleTrack {
    /// Language code (e.g., "zh-CN", "en-US")
    pub language: String,
    /// Subtitle name
    pub name: String,
    /// Subtitle URL
    pub url: String,
    /// Request headers required for subtitle fetching
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Format (srt, vtt, ass)
    pub format: String,
}

/// Playback information for a single mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackInfo {
    /// Video URLs (supports adaptive streaming with multiple URLs)
    pub urls: Vec<String>,

    /// Video format (mp4, m3u8, flv, mpd)
    pub format: String,

    /// HTTP headers required for playback
    #[serde(default)]
    pub headers: HashMap<String, String>,

    /// Available subtitle tracks
    #[serde(default)]
    pub subtitles: Vec<SubtitleTrack>,

    /// URL expiration time (Unix timestamp in seconds, optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,

    /// Whether this playback source requires CORS proxying
    ///
    /// When `true`, the client should route requests through the `SyncTV` server's
    /// CORS proxy endpoint instead of fetching the URLs directly. This is needed
    /// for providers whose CDNs do not set permissive CORS headers (e.g., Bilibili).
    #[serde(default)]
    pub cors_proxy_required: bool,
}

/// Complete playback result with multiple modes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackResult {
    /// Multiple playback modes (e.g., "direct", "proxied", "high", "low")
    pub playback_infos: HashMap<String, PlaybackInfo>,

    /// Default playback mode to use
    pub default_mode: String,

    /// Backend-owned source duration in seconds when the provider knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,

    /// Additional provider metadata for display-only fields.
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
}

/// Provider credential binding that a media or dynamic playlist playback depends on.
///
/// The provider owns source-config parsing and credential policy decisions. Callers
/// can compare this value against a credential mutation event without knowing
/// provider-specific fields such as Bilibili's shared/non-shared flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCredentialDependency {
    pub provider: String,
    pub user_id: String,
    pub server_id: String,
}

impl ProviderCredentialDependency {
    #[must_use]
    pub fn new(
        provider: impl Into<String>,
        user_id: impl Into<String>,
        server_id: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            user_id: user_id.into(),
            server_id: server_id.into(),
        }
    }

    #[must_use]
    pub fn matches(&self, provider: &str, user_id: &str, server_id: &str) -> bool {
        self.provider == provider && self.user_id == user_id && self.server_id == server_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceConfigKind {
    Media,
    DynamicPlaylist,
}

impl std::fmt::Display for SourceConfigKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Media => f.write_str("media"),
            Self::DynamicPlaylist => f.write_str("dynamic_playlist"),
        }
    }
}

/// Provider source-config plus the domain object it belongs to.
///
/// Media and dynamic playlists can share the same provider and JSON shape, but
/// they are different domain objects with different lifecycle constraints. This
/// wrapper keeps the JSON value and its usage together at the provider boundary.
#[derive(Debug, Clone, Copy)]
pub struct SourceConfig<'a>(SourceConfigKind, &'a Value);

impl<'a> SourceConfig<'a> {
    #[must_use]
    pub const fn media(value: &'a Value) -> Self {
        Self(SourceConfigKind::Media, value)
    }

    #[must_use]
    pub const fn dynamic_playlist(value: &'a Value) -> Self {
        Self(SourceConfigKind::DynamicPlaylist, value)
    }

    #[must_use]
    pub const fn kind(self) -> SourceConfigKind {
        self.0
    }

    #[must_use]
    pub const fn value(self) -> &'a Value {
        self.1
    }

    #[must_use]
    pub const fn is_media(self) -> bool {
        matches!(self.0, SourceConfigKind::Media)
    }

    #[must_use]
    pub const fn is_dynamic_playlist(self) -> bool {
        matches!(self.0, SourceConfigKind::DynamicPlaylist)
    }
}

impl AsRef<Value> for SourceConfig<'_> {
    fn as_ref(&self) -> &Value {
        self.1
    }
}

/// Item type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Playlist, // Folder/directory (playlist)
    Media,    // File (video/audio/live stream)
}

/// Directory item (file or folder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryItem {
    /// Item name
    pub name: String,

    /// Item type
    pub item_type: ItemType,

    /// Provider-facing target payload for this item
    pub target: Vec<u8>,

    /// File size in bytes (for files)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,

    /// Thumbnail URL (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,

    /// Upstream item description or summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Modified time (Unix timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<i64>,
}

/// Query options for browsing provider-backed dynamic playlists.
#[derive(Debug, Clone, Default)]
pub struct DynamicListQuery {
    /// Page number, 1-indexed at the provider boundary.
    pub page: usize,
    /// Maximum items per page.
    pub page_size: usize,
    /// Optional provider-side search term.
    pub search: Option<String>,
    /// Force the upstream provider to refresh its directory cache when supported.
    pub refresh: bool,
}

/// Media provider trait
///
/// Core interface that all providers must implement.
/// Only `generate_playback()` is mandatory.
///
/// Note: `MediaProvider` is a provider-type adapter, not necessarily a concrete backend.
/// It may route through a top-level provider instance binding via `RemoteProviderManager`.
#[async_trait]
pub trait MediaProvider: Send + Sync {
    /// Provider type name (e.g., "bilibili", "alist", "emby")
    fn name(&self) -> &'static str;

    /// Generate playback information from `source_config`
    ///
    /// This is the ONLY mandatory method. Called when user plays media.
    ///
    /// # Flow
    /// 1. Read media from database (includes `source_config`)
    /// 2. Call `generate_playback(source_config)`
    /// 3. Return `PlaybackResult` to client
    ///
    /// # Caching
    /// Results are cached in Redis by each provider's implementation
    ///
    /// # Returns
    /// `PlaybackResult` with multiple modes:
    /// - "direct": Direct URLs from provider API
    /// - "proxied": URLs proxied through `SyncTV` server
    /// - Custom modes: Provider-specific (e.g., "cdn1", "cdn2")
    ///
    /// # Example
    /// ```rust
    /// // Bilibili video:
    /// // source_config = {"type": "video", "bvid": "BV1xx", "cid": 123, "shared": false}
    /// // Returns: {
    /// //   playback_infos: {"direct": {...}, "proxied": {...}},
    /// //   default_mode: "direct"
    /// // }
    /// ```
    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError>;

    /// Cast to `DynamicFolder` trait if supported
    ///
    /// Providers that implement `DynamicFolder` trait should override this
    /// to return `Some(self)` for dynamic folder listing capability.
    ///
    /// # Returns
    /// - `Some(&dyn DynamicFolder)` if provider supports dynamic folders
    /// - `None` if provider doesn't support this capability
    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        None
    }

    /// Cast to `ProviderProxy` trait if supported
    ///
    /// Providers that support HTTP proxy routes should override this
    /// to return `Some(self)` for proxy resolution capability.
    ///
    /// # Returns
    /// - `Some(&dyn ProviderProxy)` if provider supports proxy routes
    /// - `None` if provider doesn't support this capability (e.g., DirectUrl)
    fn as_provider_proxy(&self) -> Option<&dyn super::proxy::ProviderProxy> {
        None
    }

    #[cfg(test)]
    fn test_client_manager_marker(&self) -> Option<usize> {
        None
    }

    /// Validate `source_config` before saving to database.
    ///
    /// Called when user adds media or creates/updates a dynamic playlist.
    /// `source_config.kind()` identifies which domain object is being validated.
    ///
    /// # Flow
    /// 1. Client constructs `source_config` from a provider parse/browse result
    /// 2. Client calls the media or playlist API with `source_config`
    /// 3. Server calls `validate_source_config()`
    /// 4. If valid, save to database
    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        Ok(()) // Default: no validation
    }

    /// Return provider credentials that this playback source depends on.
    ///
    /// This is intentionally provider-owned so generic real-time code never has
    /// to parse provider-specific `source_config` fields. Providers that do not
    /// use persisted user credentials should keep the default empty result.
    fn credential_dependencies(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: &Value,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        Ok(Vec::new())
    }

    /// Prepare `source_config` for storage after validation.
    ///
    /// Providers may override this to normalize provider-specific fields before
    /// persistence. The default implementation returns the `source_config`
    /// unchanged.
    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: Value,
    ) -> Result<Value, ProviderError> {
        Ok(source_config) // Default: no transformation
    }

    /// Called when playback starts
    ///
    /// Use cases:
    /// - Emby: Notify server to start transcoding
    /// - Statistics: Record playback event
    async fn on_playback_start(
        &self,
        _ctx: &ProviderContext<'_>,
        _session_id: &str,
        _source_config: &Value,
    ) -> Result<(), ProviderError> {
        Ok(()) // Default: no-op
    }

    /// Called when playback stops
    ///
    /// Use cases:
    /// - Emby: Notify server to stop transcoding
    /// - Statistics: Record watch duration
    async fn on_playback_stop(
        &self,
        _ctx: &ProviderContext<'_>,
        _session_id: &str,
        _source_config: &Value,
        _position: f64,
    ) -> Result<(), ProviderError> {
        Ok(()) // Default: no-op
    }

    /// Called periodically during playback (every 10s)
    ///
    /// Use cases:
    /// - Emby: Update playback progress on server
    /// - Statistics: Track viewing progress
    async fn on_playback_progress(
        &self,
        _ctx: &ProviderContext<'_>,
        _session_id: &str,
        _source_config: &Value,
        _position: f64,
        _is_paused: bool,
    ) -> Result<(), ProviderError> {
        Ok(()) // Default: no-op
    }

    /// Return the provider-owned playback session id from a generated playback result.
    ///
    /// Providers that allocate server-side playback/transcoding sessions should
    /// expose the opaque provider session id here so the API layer can report
    /// progress and release provider resources when room playback changes.
    fn playback_lifecycle_session_id(&self, _result: &PlaybackResult) -> Option<String> {
        None
    }
}

/// Optional trait for providers that support dynamic folders
///
/// Implemented by: Alist, Emby
/// Not implemented by: Bilibili, `DirectUrl`, RTMP
///
/// This trait enables providers to:
/// 1. List contents of dynamic folders (playlists)
/// 2. Provide next item for auto-play
#[async_trait]
pub trait DynamicFolder: MediaProvider {
    /// List playlist contents
    ///
    /// Used to browse dynamic folders and load their contents.
    ///
    /// # Arguments
    /// - `ctx`: Provider context (includes `user_id`, `room_id`, etc.)
    /// - `playlist`: The dynamic folder (playlist object)
    /// - `target`: Provider-facing target payload within the dynamic folder
    /// - `query`: Provider-side list/search options. Page numbers are 1-indexed at this
    ///   boundary; callers must pass `1` for the first page.
    /// # Returns
    /// List of items (videos, folders) in the dynamic folder
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&[u8]>,
        query: DynamicListQuery,
    ) -> Result<Vec<DirectoryItem>, ProviderError>;

    /// Resolve a single playable media item inside a dynamic playlist.
    ///
    /// This is the canonical lookup used when the playback state stores only
    /// `playlist_id + target` for dynamic playback targets.
    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &[u8],
    ) -> Result<Option<NextPlayItem>, ProviderError>;

    /// Get next item for auto-play
    ///
    /// Used by the auto-play system to get the next item when current media finishes.
    ///
    /// # Arguments
    /// - `ctx`: Provider context (includes `user_id`, `room_id`, etc.)
    /// - `playlist`: The dynamic folder (playlist object)
    /// - `playing_media`: Currently playing media object
    /// - `target`: Current provider-facing target payload in the dynamic folder
    /// - `play_mode`: Play mode (sequential, repeat one, repeat all, shuffle)
    ///
    /// # Returns
    /// - `Some(NextPlayItem)`: Next item to play
    /// - `None`: No more items (end of playlist for sequential mode)
    ///
    /// # Implementation Notes
    /// - **Sequential**: Return next item in order, None at end
    /// - **`RepeatOne`**: Return `playing_media` again
    /// - **`RepeatAll`**: Wrap around to first item
    /// - **Shuffle**: Return random item from playlist
    ///
    /// # Example
    /// ```rust
    /// // Emby playlist scenario
    /// // playing_media.source_config = {"playlist_id": "123", "current_index": 5}
    /// // Returns item at index 6
    ///
    /// // Alist folder scenario
    /// // target = provider-defined folder cursor
    /// // Returns next playable item in that dynamic folder
    /// ```
    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        playing_media: &crate::models::Media,
        target: &[u8],
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError>;

    /// Build provider-specific browse path segments for the current target.
    ///
    /// The returned segments are appended after the persisted playlist path.
    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        _target: Option<&[u8]>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicBrowsePathSegment {
    pub name: String,
    pub target: Vec<u8>,
}

/// Next play item for dynamic playback and auto-play.
///
/// Contains server-side provider data needed to resolve playback for a dynamic
/// playlist item. `source_config` may contain provider credentials and must not
/// be serialized into client-facing API responses. If a future API needs to
/// expose it, the owner is the dynamic playlist creator and the caller must be
/// that creator.
#[derive(Debug, Clone)]
pub struct NextPlayItem {
    /// Item name
    pub name: String,

    /// Item type
    pub item_type: ItemType,

    /// Provider `source_config` (to be stored in `Media.source_config`)
    pub source_config: serde_json::Value,

    /// Metadata (duration, thumbnail, etc.)
    pub metadata: serde_json::Value,

    /// Provider-specific data for `next()` calls
    /// e.g., Emby playlist index, Alist folder current path
    pub provider_data: serde_json::Value,

    /// Provider-facing target payload for this playable item
    pub target: Vec<u8>,
}
