// Provider Configuration Types
// Each provider has different source_config structure stored in the database

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::credential_resolver::CredentialRef;

/// Bilibili Provider Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BilibiliSourceConfig {
    /// Video BV ID
    pub bvid: String,

    /// Part ID (cid)
    #[serde(default)]
    pub cid: u64,

    /// Episode ID for anime (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub epid: Option<u64>,

    /// Quality (0=auto, 80=1080P+, 64=720P60, 32=480P)
    #[serde(default)]
    pub quality: u32,

    /// Whether to use proxy mode
    #[serde(default)]
    pub prefer_proxy: bool,

    /// Whether to use shared cache
    #[serde(default)]
    pub shared: bool,

    /// Reference to stored credentials (server-side)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<CredentialRef>,
}

/// Alist Provider Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AlistSourceConfig {
    /// File path (format: "{`server_id/{path`}")
    pub path: String,

    /// Folder password (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,

    /// Whether to use proxy mode (default: true)
    #[serde(default = "default_true")]
    pub prefer_proxy: bool,

    /// Reference to stored credentials (server-side)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<CredentialRef>,
}

/// Emby Provider Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbySourceConfig {
    /// Media item ID
    pub item_id: String,

    /// Whether to enable transcoding
    #[serde(default)]
    pub transcode: bool,

    /// Maximum bitrate in kbps (0=unlimited)
    #[serde(default)]
    pub max_bitrate: u32,

    /// Whether to use proxy mode
    #[serde(default)]
    pub prefer_proxy: bool,

    /// Reference to stored credentials (server-side)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential_ref: Option<CredentialRef>,
}

/// Direct URL Provider Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectUrlSourceConfig {
    /// Video URL
    pub url: String,

    /// Custom HTTP headers (optional)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,

    /// Whether to use proxy
    #[serde(default)]
    pub proxy: bool,
}

/// RTMP Provider Configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RtmpSourceConfig {}

/// Live proxy provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveProxySourceConfig {
    /// External RTMP or HTTP-FLV source URL
    pub url: String,
}

const fn default_true() -> bool {
    true
}
