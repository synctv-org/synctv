//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    provider_client::{
        create_remote_bilibili_client, load_local_bilibili_client, BilibiliClientArc,
    },
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SubtitleTrack,
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

    /// Get a reference to the cookies from any variant
    const fn cookies(&self) -> &HashMap<String, String> {
        match self {
            Self::Video { cookies, .. }
            | Self::Pgc { cookies, .. }
            | Self::Live { cookies, .. } => cookies,
        }
    }

    /// Validate that cookie keys and values do not contain control characters
    /// or HTTP header-unsafe characters that could lead to header injection.
    fn validate_cookies(&self) -> Result<(), ProviderError> {
        for (key, value) in self.cookies() {
            if key.is_empty() {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili cookie key must not be empty".to_string(),
                ));
            }
            if key.chars().any(|c| c.is_control() || c == ';' || c == '=' || c == ' ') {
                return Err(ProviderError::InvalidConfig(format!(
                    "Bilibili cookie key '{key}' contains invalid characters (control chars, ';', '=', or spaces)"
                )));
            }
            if value.chars().any(|c| c.is_control() || c == ';') {
                return Err(ProviderError::InvalidConfig(format!(
                    "Bilibili cookie value for key '{key}' contains invalid characters (control chars or ';')"
                )));
            }
        }
        Ok(())
    }

    /// Return a sanitized copy of the cookies with any unsafe characters removed.
    /// This is a defense-in-depth measure applied at request time.
    fn sanitized_cookies(&self) -> HashMap<String, String> {
        self.cookies()
            .iter()
            .filter(|(k, _)| !k.is_empty())
            .map(|(k, v)| {
                let clean_key: String = k.chars()
                    .filter(|c| !c.is_control() && *c != ';' && *c != '=' && *c != ' ')
                    .collect();
                let clean_value: String = v.chars()
                    .filter(|c| !c.is_control() && *c != ';')
                    .collect();
                (clean_key, clean_value)
            })
            .collect()
    }
}

impl BilibiliSourceConfig {
    /// Encrypt the cookies field in a `source_config` JSON value using the provided encryption.
    ///
    /// Replaces the plaintext `cookies` map with an encrypted string value.
    /// If cookies are empty or encryption is not available, returns the value unchanged.
    fn encrypt_cookies_in_value(
        source_config: &Value,
        encryption: &crate::service::CredentialEncryption,
    ) -> Result<Value, ProviderError> {
        let mut config = source_config.clone();
        if let Some(obj) = config.as_object_mut() {
            if let Some(cookies_value) = obj.get("cookies") {
                // Only encrypt if cookies is a non-empty object (not already encrypted string)
                if let Some(cookies_map) = cookies_value.as_object() {
                    if !cookies_map.is_empty() {
                        let encrypted = encryption.encrypt(cookies_value)
                            .map_err(|e| ProviderError::ApiError(format!("Failed to encrypt Bilibili cookies: {e}")))?;
                        obj.insert("cookies".to_string(), Value::String(encrypted));
                    }
                }
            }
        }
        Ok(config)
    }

    /// Decrypt the cookies field in a `source_config` JSON value if it was encrypted.
    ///
    /// If the `cookies` field is a string starting with `enc:`, decrypt it back to
    /// a map. Otherwise, return the value unchanged (backward compatible with plaintext).
    fn decrypt_cookies_in_value(
        source_config: &Value,
        encryption: &crate::service::CredentialEncryption,
    ) -> Result<Value, ProviderError> {
        let mut config = source_config.clone();
        if let Some(obj) = config.as_object_mut() {
            if let Some(cookies_value) = obj.get("cookies") {
                if let Some(encrypted_str) = cookies_value.as_str() {
                    if encrypted_str.starts_with("enc:") {
                        let decrypted = encryption.decrypt(encrypted_str)
                            .map_err(|e| ProviderError::ApiError(format!("Failed to decrypt Bilibili cookies: {e}")))?;
                        obj.insert("cookies".to_string(), decrypted);
                    }
                }
            }
        }
        Ok(config)
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
        // Decrypt cookies if encryption is configured (handles both encrypted and plaintext)
        let decrypted_config = if let Some(enc) = _ctx.credential_encryption {
            BilibiliSourceConfig::decrypt_cookies_in_value(source_config, enc)?
        } else {
            source_config.clone()
        };

        // Parse source_config first
        let config = BilibiliSourceConfig::try_from(&decrypted_config)?;

        // Sanitize cookies at request time as defense-in-depth
        let sanitized_cookies = config.sanitized_cookies();

        // Get appropriate client based on instance_name from config
        let client = self.get_client(config.provider_instance_name()).await;

        match config {
            BilibiliSourceConfig::Video {
                bvid,
                aid,
                cid,
                ..
            } => {
                let bvid = bvid.unwrap_or_default();
                let aid = aid.unwrap_or(0);

                let request = synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq {
                    aid,
                    bvid: bvid.clone(),
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                let dash_resp = client.get_dash_video_url(request).await?;

                let mut metadata = HashMap::new();
                let mut subtitles = Vec::new();

                // Fetch subtitles
                let subtitle_request = synctv_media_providers::grpc::bilibili::GetSubtitlesReq {
                    aid,
                    bvid: bvid.clone(),
                    cid,
                    cookies: sanitized_cookies.clone(),
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

                // Store DASH duration metadata
                if let Some(d) = &dash_resp.dash {
                    metadata.insert("duration".to_string(), json!(d.duration));
                    metadata.insert("min_buffer_time".to_string(), json!(d.min_buffer_time));
                }

                metadata.insert("content_type".to_string(), json!("video"));
                metadata.insert("bvid".to_string(), json!(bvid));
                metadata.insert("aid".to_string(), json!(aid));
                metadata.insert("cid".to_string(), json!(cid));

                // Bilibili CDN URLs are typically valid for ~2 hours
                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);

                // Extract video stream URLs from DASH data so the proxy layer
                // has URLs to work with (e.g., for M3U8 proxy).
                let dash_urls: Vec<String> = dash_resp
                    .dash
                    .as_ref()
                    .map(|d| d.video_streams.iter().map(|s| s.base_url.clone()).collect())
                    .unwrap_or_default();

                // Keep a "dash" PlaybackInfo with headers for proxy layer
                let mut playback_infos = HashMap::new();
                playback_infos.insert(
                    "dash".to_string(),
                    PlaybackInfo {
                        urls: dash_urls,
                        format: "mpd".to_string(),
                        headers: bilibili_headers(),
                        subtitles,
                        expires_at,
                        cors_proxy_required: true,
                    },
                );

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode: "dash".to_string(),
                    metadata,
                })
            }

            BilibiliSourceConfig::Pgc {
                epid, cid, ..
            } => {
                let request = synctv_media_providers::grpc::bilibili::GetDashPgcurlReq {
                    epid,
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                let dash_resp = client.get_dash_pgcurl(request).await?;

                let mut metadata = HashMap::new();
                let mut subtitles = Vec::new();

                // Fetch subtitles for PGC content (uses cid-based lookup)
                let subtitle_request = synctv_media_providers::grpc::bilibili::GetSubtitlesReq {
                    aid: 0,
                    bvid: String::new(),
                    cid,
                    cookies: sanitized_cookies.clone(),
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

                // Store DASH duration metadata
                if let Some(d) = &dash_resp.dash {
                    metadata.insert("duration".to_string(), json!(d.duration));
                }

                metadata.insert("content_type".to_string(), json!("pgc"));
                metadata.insert("epid".to_string(), json!(epid));
                metadata.insert("cid".to_string(), json!(cid));

                // Bilibili CDN URLs are typically valid for ~2 hours
                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);

                // Extract video stream URLs from DASH data
                let pgc_urls: Vec<String> = dash_resp
                    .dash
                    .as_ref()
                    .map(|d| d.video_streams.iter().map(|s| s.base_url.clone()).collect())
                    .unwrap_or_default();

                let mut playback_infos = HashMap::new();
                playback_infos.insert(
                    "dash".to_string(),
                    PlaybackInfo {
                        urls: pgc_urls,
                        format: "mpd".to_string(),
                        headers: bilibili_headers(),
                        subtitles,
                        expires_at,
                        cors_proxy_required: true,
                    },
                );

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode: "dash".to_string(),
                    metadata,
                })
            }

            BilibiliSourceConfig::Live {
                room_id, ..
            } => {
                // Live streams use HLS — no DASH
                //
                // Note: `GetLiveStreamsReq.cid` is named `cid` in the protobuf definition
                // for historical reasons, but for live streams this field carries the
                // live **room_id**, not a video content-ID (cid). The Bilibili live API
                // identifies rooms by room_id, so `room_id` is assigned here.
                let request = synctv_media_providers::grpc::bilibili::GetLiveStreamsReq {
                    cid: room_id, // semantically room_id — see comment above
                    hls: true,
                    cookies: sanitized_cookies,
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
                            cors_proxy_required: true,
                        },
                    );
                }

                metadata.insert("content_type".to_string(), json!("live"));
                metadata.insert("room_id".to_string(), json!(room_id));
                metadata.insert("is_live".to_string(), json!(true));

                // Sort keys so the default mode is deterministic across server
                // restarts and replicas.  HashMap iteration order is randomised
                // per-process in Rust, so we must sort before picking the first key.
                let default_mode = {
                    let mut keys: Vec<&String> = playback_infos.keys().collect();
                    keys.sort();
                    keys.into_iter().next().cloned().unwrap_or_else(|| "direct".to_string())
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    metadata,
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
                // Validate bvid format to prevent injection via crafted identifiers.
                // Valid BV IDs are alphanumeric (e.g. "BV1xx411c7mD").
                if let Some(bv) = bvid.as_ref() {
                    if !bv.is_empty() && !bv.chars().all(|c| c.is_ascii_alphanumeric()) {
                        return Err(ProviderError::InvalidConfig(
                            "Bilibili bvid must contain only alphanumeric characters".to_string(),
                        ));
                    }
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

        // Validate cookie keys/values don't contain control characters or
        // HTTP header-unsafe characters that could lead to header injection.
        config.validate_cookies()?;

        Ok(())
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: Value,
    ) -> Result<Value, ProviderError> {
        // Encrypt cookies in source_config before storage if encryption is available
        if let Some(enc) = _ctx.credential_encryption {
            BilibiliSourceConfig::encrypt_cookies_in_value(&source_config, enc)
        } else {
            Ok(source_config)
        }
    }

    fn cache_key(&self, ctx: &ProviderContext<'_>, source_config: &Value) -> String {
        // Decrypt cookies if encrypted before hashing for consistent cache keys
        let decrypted = if let Some(enc) = ctx.credential_encryption {
            BilibiliSourceConfig::decrypt_cookies_in_value(source_config, enc)
                .unwrap_or_else(|_| source_config.clone())
        } else {
            source_config.clone()
        };
        // Include a hash of the user's cookies in the cache key to prevent
        // cross-user cache pollution (VIP vs non-VIP get different results).
        if let Ok(config) = BilibiliSourceConfig::try_from(&decrypted) {
            use sha2::{Sha256, Digest};
            let (identifier, cookies) = match &config {
                BilibiliSourceConfig::Video { bvid, aid, cid, cookies, .. } => {
                    (format!("video:{}:{}:{}", bvid.as_deref().unwrap_or(""), aid.unwrap_or(0), cid), cookies)
                }
                BilibiliSourceConfig::Pgc { epid, cid, cookies, .. } => {
                    (format!("pgc:{epid}:{cid}"), cookies)
                }
                BilibiliSourceConfig::Live { room_id, cookies, .. } => {
                    (format!("live:{room_id}"), cookies)
                }
            };
            // Build a stable string from cookies for hashing
            let mut cookie_parts: Vec<_> = cookies.iter().collect();
            cookie_parts.sort_by_key(|(k, _)| k.as_str());
            let cookies_str: String = cookie_parts.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(";");
            let user_hash = if cookies_str.is_empty() {
                "anon".to_string()
            } else {
                format!("{:x}", Sha256::digest(cookies_str.as_bytes()))
                    .chars().take(16).collect::<String>()
            };
            let full_id = format!("{identifier}:{user_hash}");
            format!("{}:playback:bilibili:{:x}", ctx.key_prefix, Sha256::digest(full_id.as_bytes()))
        } else {
            format!("{}:playback:bilibili:unknown", ctx.key_prefix)
        }
    }
}

// Use the shared bilibili_headers() from the parent module.
use super::bilibili_headers;

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
                if let Some(bv) = bvid.as_ref() {
                    if !bv.is_empty() && !bv.chars().all(|c| c.is_ascii_alphanumeric()) {
                        return Err(ProviderError::InvalidConfig(
                            "Bilibili bvid must contain only alphanumeric characters".to_string(),
                        ));
                    }
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
        config.validate_cookies()?;
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

    #[test]
    fn test_video_config_invalid_bvid_rejects_injection() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx/../../../etc/passwd",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_invalid_bvid_rejects_special_chars() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx;DROP TABLE",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_with_control_chars_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value\r\nInjected-Header: evil"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_key_with_semicolon_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"key;inject": "value"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_valid_cookies_accepted() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "abc123def456"}
        });
        assert!(validate_bilibili(config).is_ok());
    }
}
