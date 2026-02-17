//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    provider_client::{
        create_remote_bilibili_client, load_local_bilibili_client, BilibiliClientArc,
    },
    DashAudioStream, DashManifestData, DashSegmentBase, DashVideoStream, MediaProvider,
    PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SubtitleTrack,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use crate::service::RemoteProviderManager;

/// Bilibili `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct BilibiliProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
}

/// Bilibili video info
#[derive(Debug, Clone, Serialize)]
pub struct BilibiliVideoInfo {
    pub bvid: String,
    pub cid: u64,
    pub epid: u64,
    pub name: String,
    pub cover_image: String,
    pub r#live: bool,
}

/// Bilibili page info response
#[derive(Debug, Clone, Serialize)]
pub struct BilibiliPageInfo {
    pub title: String,
    pub actors: Vec<String>,
    pub videos: Vec<BilibiliVideoInfo>,
}

impl BilibiliProvider {
    /// Create a new `BilibiliProvider` with `RemoteProviderManager`
    #[must_use] 
    pub const fn new(provider_instance_manager: Arc<RemoteProviderManager>) -> Self {
        Self {
            provider_instance_manager,
        }
    }

    /// Get Bilibili client for the given instance name (remote if available, local fallback)
    async fn get_client(&self, instance_name: Option<&str>) -> BilibiliClientArc {
        self.provider_instance_manager
            .resolve_client(instance_name, create_remote_bilibili_client, load_local_bilibili_client)
            .await
    }

    // ========== Provider API Methods ==========

    /// Match URL to determine type and ID
    pub async fn r#match(
        &self,
        url: String,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::MatchResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        let req = synctv_media_providers::grpc::bilibili::MatchReq { url };
        client.r#match(req).await.map_err(std::convert::Into::into)
    }

    /// Parse video page
    pub async fn parse_video_page(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseVideoPageReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.parse_video_page(req).await.map_err(std::convert::Into::into)
    }

    /// Parse PGC page
    pub async fn parse_pgc_page(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParsePgcPageReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.parse_pgc_page(req).await.map_err(std::convert::Into::into)
    }

    /// Parse live page
    pub async fn parse_live_page(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseLivePageReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.parse_live_page(req).await.map_err(std::convert::Into::into)
    }

    /// Generate QR code for login
    pub async fn new_qr_code(
        &self,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewQrCodeResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client
            .new_qr_code(synctv_media_providers::grpc::bilibili::Empty {})
            .await
            .map_err(std::convert::Into::into)
    }

    /// Check QR code login status
    pub async fn login_with_qr_code(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithQrCodeResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.login_with_qr_code(req).await.map_err(std::convert::Into::into)
    }

    /// Get new captcha
    pub async fn new_captcha(
        &self,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewCaptchaResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client
            .new_captcha(synctv_media_providers::grpc::bilibili::Empty {})
            .await
            .map_err(std::convert::Into::into)
    }

    /// Send SMS verification code
    pub async fn new_sms(
        &self,
        req: synctv_media_providers::grpc::bilibili::NewSmsReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewSmsResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.new_sms(req).await.map_err(std::convert::Into::into)
    }

    /// Login with SMS code
    pub async fn login_with_sms(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithSmsReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithSmsResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.login_with_sms(req).await.map_err(std::convert::Into::into)
    }

    /// Get user info
    pub async fn user_info(
        &self,
        req: synctv_media_providers::grpc::bilibili::UserInfoReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::UserInfoResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client.user_info(req).await.map_err(std::convert::Into::into)
    }

    /// Get live danmu (弹幕) server info for WebSocket connection
    pub async fn get_live_danmu_info(
        &self,
        room_id: u64,
        cookies: HashMap<String, String>,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        let req = synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq {
            cookies,
            room_id,
        };
        client
            .get_live_danmu_info(req)
            .await
            .map_err(std::convert::Into::into)
    }
}

// Note: Default implementation removed as it requires RemoteProviderManager

/// Bilibili source configuration structs
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BilibiliSourceConfig {
    Video {
        bvid: Option<String>,
        aid: Option<u64>,
        cid: u64,
        #[serde(default)]
        cookies: HashMap<String, String>,
        #[serde(default)]
        provider_instance_name: Option<String>,
    },
    Pgc {
        epid: u64,
        cid: u64,
        #[serde(default)]
        cookies: HashMap<String, String>,
        #[serde(default)]
        provider_instance_name: Option<String>,
    },
    Live {
        room_id: u64,
        #[serde(default)]
        cookies: HashMap<String, String>,
        #[serde(default)]
        provider_instance_name: Option<String>,
    },
}

impl BilibiliSourceConfig {
    /// Get `provider_instance_name` from any variant
    fn provider_instance_name(&self) -> Option<&str> {
        match self {
            Self::Video {
                provider_instance_name,
                ..
            } => provider_instance_name.as_deref(),
            Self::Pgc {
                provider_instance_name,
                ..
            } => provider_instance_name.as_deref(),
            Self::Live {
                provider_instance_name,
                ..
            } => provider_instance_name.as_deref(),
        }
    }
}

impl TryFrom<&Value> for BilibiliSourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::parse_source_config(value, "Bilibili")
    }
}

#[async_trait]
impl MediaProvider for BilibiliProvider {
    fn name(&self) -> &'static str {
        "bilibili"
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        // Parse source_config first
        let config = BilibiliSourceConfig::try_from(source_config)?;

        // Get appropriate client based on instance_name from config
        let client = self.get_client(config.provider_instance_name()).await;

        match config {
            BilibiliSourceConfig::Video {
                bvid,
                aid,
                cid,
                cookies,
                ..
            } => {
                let bvid = bvid.unwrap_or_default();
                let aid = aid.unwrap_or(0);

                let request = synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq {
                    aid,
                    bvid: bvid.clone(),
                    cid,
                    cookies: cookies.clone(),
                };
                let dash_resp = client.get_dash_video_url(request).await?;

                let mut metadata = HashMap::new();
                let mut subtitles = Vec::new();

                // Fetch subtitles
                let subtitle_request = synctv_media_providers::grpc::bilibili::GetSubtitlesReq {
                    aid,
                    bvid: bvid.clone(),
                    cid,
                    cookies,
                };
                if let Ok(subtitle_resp) = client.get_subtitles(subtitle_request).await {
                    subtitles = subtitle_resp
                        .subtitles
                        .into_iter()
                        .map(|(name, url)| SubtitleTrack {
                            language: name.clone(),
                            name,
                            url,
                            format: "json".to_string(),
                        })
                        .collect();
                }

                // Convert proto DashInfo → DashManifestData
                let dash = dash_resp.dash.map(|d| {
                    metadata.insert("duration".to_string(), json!(d.duration));
                    metadata.insert("min_buffer_time".to_string(), json!(d.min_buffer_time));
                    proto_dash_to_manifest(&d)
                });
                let hevc_dash = dash_resp
                    .hevc_dash
                    .filter(|d| !d.video_streams.is_empty())
                    .map(|d| proto_dash_to_manifest(&d));

                metadata.insert("content_type".to_string(), json!("video"));
                metadata.insert("bvid".to_string(), json!(bvid));
                metadata.insert("aid".to_string(), json!(aid));
                metadata.insert("cid".to_string(), json!(cid));

                // Bilibili CDN URLs are typically valid for ~2 hours
                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);

                // Keep a "dash" PlaybackInfo with headers for proxy layer
                let mut playback_infos = HashMap::new();
                playback_infos.insert(
                    "dash".to_string(),
                    PlaybackInfo {
                        urls: Vec::new(),
                        format: "mpd".to_string(),
                        headers: bilibili_headers(),
                        subtitles,
                        expires_at,
                    },
                );

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode: "dash".to_string(),
                    metadata,
                    dash,
                    hevc_dash,
                })
            }

            BilibiliSourceConfig::Pgc {
                epid, cid, cookies, ..
            } => {
                let request = synctv_media_providers::grpc::bilibili::GetDashPgcurlReq {
                    epid,
                    cid,
                    cookies: cookies.clone(),
                };
                let dash_resp = client.get_dash_pgcurl(request).await?;

                let mut metadata = HashMap::new();

                let dash = dash_resp.dash.map(|d| {
                    metadata.insert("duration".to_string(), json!(d.duration));
                    proto_dash_to_manifest(&d)
                });
                let hevc_dash = dash_resp
                    .hevc_dash
                    .filter(|d| !d.video_streams.is_empty())
                    .map(|d| proto_dash_to_manifest(&d));

                metadata.insert("content_type".to_string(), json!("pgc"));
                metadata.insert("epid".to_string(), json!(epid));
                metadata.insert("cid".to_string(), json!(cid));

                // Bilibili CDN URLs are typically valid for ~2 hours
                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);

                let mut playback_infos = HashMap::new();
                playback_infos.insert(
                    "dash".to_string(),
                    PlaybackInfo {
                        urls: Vec::new(),
                        format: "mpd".to_string(),
                        headers: bilibili_headers(),
                        subtitles: Vec::new(),
                        expires_at,
                    },
                );

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode: "dash".to_string(),
                    metadata,
                    dash,
                    hevc_dash,
                })
            }

            BilibiliSourceConfig::Live {
                room_id, cookies, ..
            } => {
                // Live streams use HLS — no DASH
                let request = synctv_media_providers::grpc::bilibili::GetLiveStreamsReq {
                    cid: room_id,
                    hls: true,
                    cookies,
                };
                let live_resp = client.get_live_streams(request).await?;

                let mut playback_infos = HashMap::new();
                let mut metadata = HashMap::new();

                let live_expires_at = Some(Utc::now().timestamp() + 120);

                for stream in live_resp.live_streams {
                    let quality_name = if stream.desc.is_empty() {
                        format!("quality_{}", stream.quality)
                    } else {
                        stream.desc
                    };
                    playback_infos.insert(
                        quality_name,
                        PlaybackInfo {
                            urls: stream.urls,
                            format: "hls".to_string(),
                            headers: {
                                let mut h = HashMap::new();
                                h.insert(
                                    "Referer".to_string(),
                                    "https://live.bilibili.com".to_string(),
                                );
                                h
                            },
                            subtitles: Vec::new(),
                            expires_at: live_expires_at,
                        },
                    );
                }

                metadata.insert("content_type".to_string(), json!("live"));
                metadata.insert("room_id".to_string(), json!(room_id));
                metadata.insert("is_live".to_string(), json!(true));

                let default_mode = playback_infos
                    .keys()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "direct".to_string());

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    metadata,
                    dash: None,
                    hevc_dash: None,
                })
            }
        }
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        // Validate that source_config parses to a known variant
        let config = BilibiliSourceConfig::try_from(source_config)?;

        match &config {
            BilibiliSourceConfig::Video { bvid, aid, cid, .. } => {
                // Must have at least one of bvid or aid
                let has_bvid = bvid.as_ref().is_some_and(|s| !s.is_empty());
                let has_aid = aid.is_some_and(|a| a > 0);
                if !has_bvid && !has_aid {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili video requires either bvid or aid".to_string(),
                    ));
                }
                if *cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili video cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Pgc { epid, cid, .. } => {
                if *epid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC epid must be non-zero".to_string(),
                    ));
                }
                if *cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Live { room_id, .. } => {
                if *room_id == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili live room_id must be non-zero".to_string(),
                    ));
                }
            }
        }

        Ok(())
    }

    fn cache_key(&self, _ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        // Hash only video identifiers, not the full config (which contains cookies)
        if let Ok(config) = BilibiliSourceConfig::try_from(source_config) {
            use sha2::{Sha256, Digest};
            let identifier = match &config {
                BilibiliSourceConfig::Video { bvid, aid, cid, .. } => {
                    format!("video:{}:{}:{}", bvid.as_deref().unwrap_or(""), aid.unwrap_or(0), cid)
                }
                BilibiliSourceConfig::Pgc { epid, cid, .. } => {
                    format!("pgc:{epid}:{cid}")
                }
                BilibiliSourceConfig::Live { room_id, .. } => {
                    format!("live:{room_id}")
                }
            };
            format!("bilibili:{:x}", Sha256::digest(identifier.as_bytes()))
        } else {
            "bilibili:unknown".to_string()
        }
    }
}

/// Standard Bilibili HTTP headers for proxy requests
fn bilibili_headers() -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert(
        "Referer".to_string(),
        "https://www.bilibili.com".to_string(),
    );
    headers.insert(
        "User-Agent".to_string(),
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/91.0.4472.124 Safari/537.36".to_string(),
    );
    headers
}

/// Convert proto `DashInfo` → provider-agnostic `DashManifestData`
fn proto_dash_to_manifest(
    dash: &synctv_media_providers::grpc::bilibili::DashInfo,
) -> DashManifestData {
    let video_streams = dash
        .video_streams
        .iter()
        .map(|v| {
            let seg = v.segment_base.as_ref();
            DashVideoStream {
                id: format!("{}P", v.height),
                base_url: v.base_url.clone(),
                backup_urls: Vec::new(),
                mime_type: v.mime_type.clone(),
                codecs: v.codecs.clone(),
                width: v.width,
                height: v.height,
                frame_rate: v.frame_rate.clone(),
                bandwidth: v.bandwidth,
                sar: "1:1".to_string(),
                start_with_sap: v.start_with_sap,
                segment_base: DashSegmentBase {
                    initialization: seg
                        .map(|s| s.initialization_range.clone())
                        .unwrap_or_default(),
                    index_range: seg
                        .map(|s| s.index_range.clone())
                        .unwrap_or_default(),
                },
            }
        })
        .collect();

    let audio_streams = dash
        .audio_streams
        .iter()
        .map(|a| {
            let seg = a.segment_base.as_ref();
            DashAudioStream {
                id: format!("audio_{}", a.id),
                base_url: a.base_url.clone(),
                backup_urls: Vec::new(),
                mime_type: a.mime_type.clone(),
                codecs: a.codecs.clone(),
                bandwidth: a.bandwidth,
                audio_sampling_rate: if a.audio_sampling_rate > 0 { a.audio_sampling_rate } else { 44100 },
                start_with_sap: a.start_with_sap,
                segment_base: DashSegmentBase {
                    initialization: seg
                        .map(|s| s.initialization_range.clone())
                        .unwrap_or_default(),
                    index_range: seg
                        .map(|s| s.index_range.clone())
                        .unwrap_or_default(),
                },
            }
        })
        .collect();

    DashManifestData {
        duration: dash.duration,
        min_buffer_time: dash.min_buffer_time,
        video_streams,
        audio_streams,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate_bilibili(config: Value) -> Result<(), ProviderError> {
        // Replicate the validation logic without needing a full provider instance
        let config = BilibiliSourceConfig::try_from(&config)?;

        match &config {
            BilibiliSourceConfig::Video { bvid, aid, cid, .. } => {
                let has_bvid = bvid.as_ref().is_some_and(|s| !s.is_empty());
                let has_aid = aid.is_some_and(|a| a > 0);
                if !has_bvid && !has_aid {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili video requires either bvid or aid".to_string(),
                    ));
                }
                if *cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili video cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Pgc { epid, cid, .. } => {
                if *epid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC epid must be non-zero".to_string(),
                    ));
                }
                if *cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Live { room_id, .. } => {
                if *room_id == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili live room_id must be non-zero".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn test_valid_video_config_with_bvid() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_valid_video_config_with_aid() {
        let config = json!({
            "type": "video",
            "aid": 12345,
            "cid": 67890
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_video_config_missing_bvid_and_aid() {
        let config = json!({
            "type": "video",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_zero_cid() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 0
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_valid_pgc_config() {
        let config = json!({
            "type": "pgc",
            "epid": 12345,
            "cid": 67890
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_pgc_config_zero_epid() {
        let config = json!({
            "type": "pgc",
            "epid": 0,
            "cid": 67890
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_valid_live_config() {
        let config = json!({
            "type": "live",
            "room_id": 12345
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_live_config_zero_room_id() {
        let config = json!({
            "type": "live",
            "room_id": 0
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_invalid_type() {
        let config = json!({
            "type": "unknown_type",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }
}
