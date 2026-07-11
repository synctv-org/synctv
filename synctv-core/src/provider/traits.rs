// Media Provider Traits
// Core interfaces for the provider system

use super::{ProviderContext, ProviderError};
use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::pin::Pin;

/// Playback information for a single mode
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackInfo {
    /// Thumbnail URL for this playback mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,

    /// Concrete media playback behaviors for this mode.
    pub medias: Vec<crate::models::media::PlaybackMedia>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_media_index: Option<usize>,

    /// Available subtitle playback behaviors.
    #[serde(default)]
    pub subtitles: Vec<crate::models::media::PlaybackSubtitle>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_subtitle_index: Option<usize>,

    /// Available danmaku playback behaviors.
    #[serde(default)]
    pub danmakus: Vec<crate::models::media::PlaybackDanmaku>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_danmaku_index: Option<usize>,
}

/// Complete playback result with multiple modes
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlaybackResult {
    /// Multiple playback modes (e.g., "direct", "proxied", "high", "low")
    pub playback_infos: HashMap<String, PlaybackInfo>,

    /// Default playback mode to use
    pub default_mode: String,

    /// Provider that generated this playback result.
    pub provider: String,

    /// Provider instance selected for this playback result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_name: Option<String>,

    /// Backend-owned source duration in seconds when the provider knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<f64>,

    /// Backend-owned source liveness when the provider can determine it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_live: Option<bool>,

    /// Additional provider metadata for display-only fields.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<crate::models::PlaybackMetadata>,
}

/// Bilibili-owned live danmaku event stream.
///
/// Bilibili live danmaku connects to Bilibili's upstream WebSocket service, so
/// the Bilibili adapter owns source-config parsing, credential policy,
/// remote-provider dispatch, and upstream reconnect behavior. Transport
/// adapters map this stream to their streaming response formats.
pub type BilibiliLiveDanmakuStream =
    Pin<Box<dyn Stream<Item = Result<BilibiliLiveDanmakuEvent, ProviderError>> + Send + 'static>>;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliLiveDanmakuEvent {
    pub format: String,
    pub event_type: String,
    pub kind: BilibiliLiveDanmakuEventKind,
    pub user: String,
    pub message: String,
    pub timestamp: u64,
    pub gift_name: String,
    pub gift_count: u32,
    pub online_count: u32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BilibiliLiveDanmakuEventKind {
    Unspecified,
    Chat,
    UserEnter,
    Gift,
    Heartbeat,
    Unknown,
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
    #[serde(default = "default_required_provider_credential_dependency")]
    pub required: bool,
}

const fn default_required_provider_credential_dependency() -> bool {
    true
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
            required: true,
        }
    }

    #[must_use]
    pub fn optional(
        provider: impl Into<String>,
        user_id: impl Into<String>,
        server_id: impl Into<String>,
    ) -> Self {
        Self {
            provider: provider.into(),
            user_id: user_id.into(),
            server_id: server_id.into(),
            required: false,
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
#[derive(Debug, Clone, Copy)]
pub enum SourceConfig<'a> {
    Media(&'a crate::models::MediaSourceConfig),
    DynamicPlaylist(&'a crate::models::PlaylistSourceConfig),
}

#[derive(Debug, Clone)]
pub enum PreparedSourceConfig {
    Media(crate::models::MediaSourceConfig),
    DynamicPlaylist(crate::models::PlaylistSourceConfig),
}

impl<'a> SourceConfig<'a> {
    #[must_use]
    pub const fn media(value: &'a crate::models::MediaSourceConfig) -> Self {
        Self::Media(value)
    }

    #[must_use]
    pub const fn dynamic_playlist(value: &'a crate::models::PlaylistSourceConfig) -> Self {
        Self::DynamicPlaylist(value)
    }

    #[must_use]
    pub const fn kind(self) -> SourceConfigKind {
        match self {
            Self::Media(_) => SourceConfigKind::Media,
            Self::DynamicPlaylist(_) => SourceConfigKind::DynamicPlaylist,
        }
    }

    #[must_use]
    pub const fn is_media(self) -> bool {
        matches!(self, Self::Media(_))
    }

    #[must_use]
    pub const fn is_dynamic_playlist(self) -> bool {
        matches!(self, Self::DynamicPlaylist(_))
    }
}

impl From<SourceConfig<'_>> for PreparedSourceConfig {
    fn from(value: SourceConfig<'_>) -> Self {
        match value {
            SourceConfig::Media(config) => Self::Media(config.clone()),
            SourceConfig::DynamicPlaylist(config) => Self::DynamicPlaylist(config.clone()),
        }
    }
}

impl PreparedSourceConfig {
    pub fn into_media(self) -> Result<crate::models::MediaSourceConfig, ProviderError> {
        match self {
            Self::Media(config) => Ok(config),
            Self::DynamicPlaylist(_) => Err(ProviderError::InvalidConfig(
                "expected media source_config".to_string(),
            )),
        }
    }

    pub fn into_dynamic_playlist(
        self,
    ) -> Result<crate::models::PlaylistSourceConfig, ProviderError> {
        match self {
            Self::DynamicPlaylist(config) => Ok(config),
            Self::Media(_) => Err(ProviderError::InvalidConfig(
                "expected dynamic playlist source_config".to_string(),
            )),
        }
    }
}

/// Item type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemType {
    Playlist, // Folder/directory (playlist)
    Media,    // File (video/audio/live stream)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum DirectoryItemThumbnail {
    Url(String),
    Emby {
        server_id: String,
        credential_owner_id: crate::models::UserId,
        item_id: String,
    },
}

/// Provider-owned cover candidate for a persisted media or dynamic playlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum SourceCover {
    Url {
        url: String,
    },
    Emby {
        server_id: String,
        credential_owner_id: crate::models::UserId,
        item_id: String,
    },
}

/// Directory item (file or folder)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryItem {
    /// Item name
    pub name: String,

    /// Item type
    pub item_type: ItemType,

    /// Provider-facing target payload for this item
    pub target: crate::models::ProviderTarget,

    /// File size in bytes (for files)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,

    /// Thumbnail metadata (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<DirectoryItemThumbnail>,

    /// Upstream item description or summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    /// Modified time (Unix timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<i64>,
}

/// Query options for browsing provider-backed dynamic playlists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DynamicPagination {
    Page { page: usize },
    Cursor { cursor: Option<String> },
}

impl Default for DynamicPagination {
    fn default() -> Self {
        Self::Page { page: 1 }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DynamicListQuery {
    pub pagination: DynamicPagination,
    /// Maximum items per page.
    pub page_size: usize,
    /// Optional provider-side search term.
    pub search: Option<String>,
    /// Force the upstream provider to refresh its directory cache when supported.
    pub refresh: bool,
}

impl DynamicListQuery {
    #[must_use]
    pub const fn page(&self) -> usize {
        match self.pagination {
            DynamicPagination::Page { page } => page,
            DynamicPagination::Cursor { .. } => 1,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct DynamicListResult {
    pub items: Vec<DirectoryItem>,
    pub pagination: DynamicPagination,
    pub has_more: bool,
}

impl std::ops::Deref for DynamicListResult {
    type Target = [DirectoryItem];

    fn deref(&self) -> &Self::Target {
        &self.items
    }
}

impl IntoIterator for DynamicListResult {
    type Item = DirectoryItem;
    type IntoIter = std::vec::IntoIter<DirectoryItem>;

    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

/// Media provider trait.
///
/// Core interface that all providers must implement. The maintenance contract
/// for playback mode selection, proxy siblings, and verification lives in
/// `docs/src/content/docs/en/develop/implementation-contracts.mdx`.
/// Keep that document updated when a provider adds modes, headers, proxy
/// actions, manifest metadata, or live lifecycle behavior.
///
/// Note: `MediaProvider` is a provider-type adapter, not necessarily a concrete backend.
/// It may route through a top-level provider instance binding via `RemoteProviderManager`.
#[async_trait]
pub trait MediaProvider: Send + Sync {
    /// Provider type name (e.g., "bilibili", "alist", "emby")
    fn name(&self) -> &'static str;

    /// Generate playback information from `source_config`
    ///
    /// This is the mandatory playback decision boundary. Called when user plays media.
    ///
    /// Provider-specific stream decisions happen here. A provider returns
    /// every playback mode that is valid for the source, typically an
    /// upstream/direct mode plus a `proxy_*` sibling when SyncTV can serve the
    /// same content through playback provider transport. Default-mode selection,
    /// header exposure, manifest rewriting, subtitle rewriting, and live resource
    /// lifecycle metadata belong in this provider-owned generation path because
    /// those rules differ by provider and by source type.
    ///
    /// The matching playback transport resolver must accept every versioned
    /// transport target produced here, including HLS/DASH manifests, indexed segments, subtitles, danmaku,
    /// thumbnails, FLV, and live resource cleanup hooks. Provider changes need
    /// CLI plus curl end-to-end evidence for every returned mode and auxiliary
    /// URL, including cached playback and expiry behavior.
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
    /// - Provider-native modes, such as `direct`, `dash`, `hls`, or quality names
    /// - Proxy sibling modes, such as `proxy_direct` or `proxy_dash`
    /// - Provider-specific modes, such as transcoding qualities or live formats
    ///
    /// # Example
    /// ```rust
    /// // Bilibili video:
    /// // source_config = {"kind": "video", "bvid": "BV1xx", "cid": 123, "shared": false}
    /// // Returns provider-owned modes, for example:
    /// // {
    /// //   playback_infos: {"dash": {...}, "proxy_dash": {...}},
    /// //   default_mode: "proxy_dash"
    /// // }
    /// ```
    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &crate::models::MediaSourceConfig,
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

    /// Cast to `BilibiliLiveDanmakuProvider` for Bilibili live media.
    fn as_bilibili_live_danmaku_provider(&self) -> Option<&dyn BilibiliLiveDanmakuProvider> {
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
    /// 2. Caller submits `source_config` for media or playlist creation
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
        _source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        Ok(Vec::new())
    }

    /// Return a single cover URL candidate for persisted resource listings.
    async fn source_cover(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        Ok(None)
    }

    /// Prepare `source_config` for storage after validation.
    ///
    /// Providers may override this to normalize provider-specific fields before
    /// persistence. The default implementation returns the `source_config`
    /// unchanged.
    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<PreparedSourceConfig, ProviderError> {
        Ok(source_config.into()) // Default: no transformation
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
        _source_config: &crate::models::MediaSourceConfig,
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
        _source_config: &crate::models::MediaSourceConfig,
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
        _source_config: &crate::models::MediaSourceConfig,
        _position: f64,
        _is_paused: bool,
    ) -> Result<(), ProviderError> {
        Ok(()) // Default: no-op
    }

    /// Return the provider-owned playback session id from a generated playback result.
    ///
    /// Providers that allocate server-side playback/transcoding sessions should
    /// expose the opaque provider session id here so playback lifecycle logic can
    /// report progress and release provider resources when room playback changes.
    fn playback_lifecycle_session_id(&self, _result: &PlaybackResult) -> Option<String> {
        None
    }
}

#[async_trait]
pub trait BilibiliLiveDanmakuProvider: Send + Sync {
    async fn watch_bilibili_live_danmaku(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &crate::models::MediaSourceConfig,
    ) -> Result<BilibiliLiveDanmakuStream, ProviderError>;
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
        target: Option<&crate::models::ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError>;

    /// Resolve a single playable media item inside a dynamic playlist.
    ///
    /// This is the canonical lookup used when the playback state stores only
    /// `playlist_id + target` for dynamic playback targets.
    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &crate::models::ProviderTarget,
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
        target: &crate::models::ProviderTarget,
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError>;

    /// Build provider-specific browse path segments for the current target.
    ///
    /// The returned segments are appended after the persisted playlist path.
    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        _target: Option<&crate::models::ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DynamicBrowsePathSegment {
    pub name: String,
    pub target: crate::models::ProviderTarget,
}

/// Next play item for dynamic playback and auto-play.
///
/// Contains server-side source config needed to resolve playback for a dynamic
/// playlist item.
#[derive(Debug, Clone)]
pub struct NextPlayItem {
    /// Item name
    pub name: String,

    /// Item type
    pub item_type: ItemType,

    pub source_config: crate::models::MediaSourceConfig,

    /// Provider-facing target payload for this playable item
    pub target: crate::models::ProviderTarget,
}
