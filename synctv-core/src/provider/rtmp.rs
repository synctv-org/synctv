//! RTMP `MediaProvider`
//!
//! Provides playback URLs for RTMP live streams.
//! URLs point to synctv's own HTTP-FLV and HLS endpoints.

use super::{
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;

/// RTMP `MediaProvider`
pub struct RtmpProvider {
    base_url: String,
}

impl RtmpProvider {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
        }
    }
}

impl Default for RtmpProvider {
    fn default() -> Self {
        Self::new("https://localhost:8080")
    }
}

#[async_trait]
impl MediaProvider for RtmpProvider {
    fn name(&self) -> &'static str {
        "rtmp"
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        let media_id = source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing media_id".to_string()))?;

        let room_id = source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing room_id".to_string()))?;

        let mut playback_infos = HashMap::new();
        let live_expires_at = Some(Utc::now().timestamp() + 30);

        // HLS URL — matches actual HTTP route: /api/room/movie/live/hls/list/:media_id?room_id=:room_id
        playback_infos.insert(
            "hls".to_string(),
            PlaybackInfo {
                urls: vec![format!(
                    "{}/api/room/movie/live/hls/list/{}?room_id={}",
                    self.base_url, media_id, room_id
                )],
                format: "m3u8".to_string(),
                headers: HashMap::new(),
                subtitles: Vec::new(),
                expires_at: live_expires_at,
                cors_proxy_required: false,
            },
        );

        // FLV URL — matches actual HTTP route: /api/room/movie/live/flv/:media_id.flv?room_id=:room_id
        playback_infos.insert(
            "flv".to_string(),
            PlaybackInfo {
                urls: vec![format!(
                    "{}/api/room/movie/live/flv/{}.flv?room_id={}",
                    self.base_url, media_id, room_id
                )],
                format: "flv".to_string(),
                headers: HashMap::new(),
                subtitles: Vec::new(),
                expires_at: live_expires_at,
                cors_proxy_required: false,
            },
        );

        let mut metadata = HashMap::new();
        metadata.insert("is_live".to_string(), json!(true));
        metadata.insert("media_id".to_string(), json!(media_id));
        metadata.insert("room_id".to_string(), json!(room_id));

        Ok(PlaybackResult {
            playback_infos,
            default_mode: "hls".to_string(),
            metadata,
            dash: None,
            hevc_dash: None,
        })
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        // Validate required fields
        source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing room_id".to_string()))?;

        source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidConfig("Missing media_id".to_string()))?;

        Ok(())
    }

    fn cache_key(&self, _ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        let room_id = source_config
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let media_id = source_config
            .get("media_id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        format!("rtmp:{room_id}:{media_id}")
    }
}
