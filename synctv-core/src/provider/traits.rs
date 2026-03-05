// Media Provider Traits
//
// Core interfaces for the provider system

use super::{ProviderContext, ProviderError};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Video quality option
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QualityOption {
    /// Quality name (e.g., "1080P", "720P")
    pub name: String,
    /// Quality code for provider API
    pub code: String,
    /// Bitrate in kbps (optional)
    pub bitrate: Option<u32>,
}

/// Subtitle track
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleTrack {
    /// Language code (e.g., "zh-CN", "en-US")
    pub language: String,
    /// Subtitle name
    pub name: String,
    /// Subtitle URL
    pub url: String,
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

    /// Additional metadata (duration, thumbnail, etc.)
    #[serde(default)]
    pub metadata: HashMap<String, Value>,
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

    /// Full path from root
    pub path: String,

    /// File size in bytes (for files)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,

    /// Thumbnail URL (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<String>,

    /// Modified time (Unix timestamp)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<i64>,
}

/// Media provider trait
///
/// Core interface that all providers must implement.
/// Only `generate_playback()` is mandatory.
///
/// Note: `MediaProvider` is a capability provider, not a concrete instance.
/// It may use different `provider_instances` internally via `RemoteProviderManager`.
#[async_trait]
pub trait MediaProvider: Send + Sync {
    // ========== Basic Information ==========

    /// Provider type name (e.g., "bilibili", "alist", "emby")
    fn name(&self) -> &'static str;

    // ========== Core Method (MANDATORY) ==========

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
    /// // Bilibili: source_config = {"bvid": "BV1xx", "cid": 123, "prefer_proxy": false}
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

    // ========== Optional Capabilities ==========

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

    // ========== Validation ==========

    /// Validate `source_config` before saving to database
    ///
    /// Called when user adds media via `add_media` API.
    ///
    /// # Flow
    /// 1. User calls parse endpoint → gets `ParseResult`
    /// 2. Client constructs `source_config` from `ParseResult`
    /// 3. Client calls `add_media` API with `source_config`
    /// 4. Server calls `validate_source_config()`
    /// 5. If valid, save to database
    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: &Value,
    ) -> Result<(), ProviderError> {
        Ok(()) // Default: no validation
    }

    /// Prepare `source_config` for storage by encrypting sensitive fields.
    ///
    /// Called after validation, before the `source_config` is persisted to the database.
    /// Providers that store sensitive data (cookies, tokens) in `source_config` should
    /// override this to encrypt those fields using `ctx.credential_encryption`.
    ///
    /// The default implementation returns the `source_config` unchanged.
    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: Value,
    ) -> Result<Value, ProviderError> {
        Ok(source_config) // Default: no transformation
    }

    // ========== Lifecycle Hooks (Optional) ==========

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
    ) -> Result<(), ProviderError> {
        Ok(()) // Default: no-op
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
    /// - `relative_path`: Relative path within the dynamic folder (e.g., "subfolder/video.mp4")
    /// - `page`: Page number (0-indexed)
    /// - `page_size`: Items per page
    ///
    /// # Returns
    /// List of items (videos, folders) in the dynamic folder
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        relative_path: Option<&str>,
        page: usize,
        page_size: usize,
    ) -> Result<Vec<DirectoryItem>, ProviderError>;

    /// Get next item for auto-play
    ///
    /// Used by the auto-play system to get the next item when current media finishes.
    ///
    /// # Arguments
    /// - `ctx`: Provider context (includes `user_id`, `room_id`, etc.)
    /// - `playlist`: The dynamic folder (playlist object)
    /// - `playing_media`: Currently playing media object
    /// - `relative_path`: Current relative path in the dynamic folder
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
    /// // relative_path = "/movies/action/"
    /// // Returns next video file in the folder
    /// ```
    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        playing_media: &crate::models::Media,
        relative_path: &str,
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError>;
}

/// Next play item for auto-play
///
/// Contains all information needed to play the next item in a playlist.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    #[serde(skip_serializing_if = "serde_json::Value::is_null")]
    pub provider_data: serde_json::Value,

    /// Relative path within the dynamic folder
    pub relative_path: String,
}

impl NextPlayItem {
    /// Return a copy with credentials stripped from `source_config`.
    ///
    /// This **must** be called before serializing the item to any client-facing
    /// API response so that tokens, passwords, cookies, etc. are never leaked.
    #[must_use]
    pub fn strip_credentials(mut self) -> Self {
        self.source_config = super::strip_source_config_credentials(&self.source_config);
        self
    }
}
