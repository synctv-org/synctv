//! DASH manifest data types shared across crates.
//!
//! These types provide a provider-agnostic representation of MPEG-DASH manifest
//! data. They live in `synctv-proto` so that both `synctv-core` (which produces
//! them) and `synctv-proxy` (which consumes them for MPD XML generation) can
//! depend on the same definition without either crate depending on the other.

use serde::{Deserialize, Serialize};

/// DASH manifest data -- structured representation for MPD generation.
/// Provider-agnostic: reusable by any DASH provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashManifestData {
    pub duration: f64,
    pub min_buffer_time: f64,
    pub video_streams: Vec<DashVideoStream>,
    pub audio_streams: Vec<DashAudioStream>,
}

/// A single video representation in a DASH manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashVideoStream {
    /// Quality name used as Representation ID (e.g. "480P")
    pub id: String,
    /// Original CDN URL
    pub base_url: String,
    /// Backup CDN URLs
    #[serde(default)]
    pub backup_urls: Vec<String>,
    pub mime_type: String,
    pub codecs: String,
    pub width: u64,
    pub height: u64,
    pub frame_rate: String,
    pub bandwidth: u64,
    pub sar: String,
    pub start_with_sap: u64,
    pub segment_base: DashSegmentBase,
}

/// A single audio representation in a DASH manifest
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashAudioStream {
    pub id: String,
    pub base_url: String,
    #[serde(default)]
    pub backup_urls: Vec<String>,
    pub mime_type: String,
    pub codecs: String,
    pub bandwidth: u64,
    pub audio_sampling_rate: u32,
    pub start_with_sap: u64,
    pub segment_base: DashSegmentBase,
}

/// `SegmentBase` for DASH byte-range addressing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashSegmentBase {
    /// Initialization byte range (e.g. "0-926")
    pub initialization: String,
    /// Index byte range (e.g. "927-9286")
    pub index_range: String,
}
