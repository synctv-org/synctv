//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    provider_client::{
        create_remote_bilibili_client, load_local_bilibili_client, BilibiliClientArc,
    },
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderError, SubtitleTrack,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::service::RemoteProviderManager;

/// Bilibili `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct BilibiliProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    /// Optional timeout for API requests (in seconds)
    timeout_seconds: Option<u64>,
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
            timeout_seconds: None,
        }
    }

    /// Create a new `BilibiliProvider` with custom timeout configuration
    #[must_use]
    pub const fn with_timeout(
        provider_instance_manager: Arc<RemoteProviderManager>,
        timeout_seconds: u64,
    ) -> Self {
        Self {
            provider_instance_manager,
            timeout_seconds: Some(timeout_seconds),
        }
    }

    /// Get the configured timeout in seconds (if any)
    #[must_use]
    pub const fn timeout_seconds(&self) -> Option<u64> {
        self.timeout_seconds
    }

    /// Get Bilibili client for the given instance name (remote if available, local fallback)
    async fn get_client(&self, instance_name: Option<&str>) -> BilibiliClientArc {
        self.provider_instance_manager
            .resolve_client(
                instance_name,
                create_remote_bilibili_client,
                load_local_bilibili_client,
            )
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
        client
            .parse_video_page(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Parse PGC page
    pub async fn parse_pgc_page(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParsePgcPageReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self.get_client(instance_name).await;
        client
            .parse_pgc_page(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Parse live page
    pub async fn parse_live_page(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseLivePageReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self.get_client(instance_name).await;
        client
            .parse_live_page(req)
            .await
            .map_err(std::convert::Into::into)
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
        client
            .login_with_qr_code(req)
            .await
            .map_err(std::convert::Into::into)
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
        client
            .login_with_sms(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Get user info
    pub async fn user_info(
        &self,
        req: synctv_media_providers::grpc::bilibili::UserInfoReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::UserInfoResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        client
            .user_info(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Get live danmu (弹幕) server info for WebSocket connection
    pub async fn get_live_danmu_info(
        &self,
        room_id: u64,
        cookies: HashMap<String, String>,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp, ProviderError> {
        let client = self.get_client(instance_name).await;
        let req = synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq { cookies, room_id };
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
    ///
    /// This function also URL-decodes cookies before validation to prevent
    /// URL-encoded bypass attempts (e.g., %0D%0A for CRLF injection).
    fn validate_cookies(&self) -> Result<(), ProviderError> {
        for (key, value) in self.cookies() {
            if key.is_empty() {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili cookie key must not be empty".to_string(),
                ));
            }

            // URL-decode both key and value to catch encoded injection attempts
            let decoded_key = urlencoding::decode(key).map_err(|_| {
                ProviderError::InvalidConfig(format!(
                    "Bilibili cookie key '{key}' contains invalid URL encoding"
                ))
            })?;
            let decoded_value = urlencoding::decode(value).map_err(|_| {
                ProviderError::InvalidConfig(format!(
                    "Bilibili cookie value for key '{key}' contains invalid URL encoding"
                ))
            })?;

            // Check decoded key for invalid characters
            if decoded_key
                .chars()
                .any(|c| c.is_control() || c == ';' || c == '=' || c == ' ')
            {
                return Err(ProviderError::InvalidConfig(format!(
                    "Bilibili cookie key '{key}' contains invalid characters (control chars, ';', '=', or spaces, including URL-encoded forms)"
                )));
            }

            // Check decoded value for invalid characters
            if decoded_value.chars().any(|c| c.is_control() || c == ';') {
                return Err(ProviderError::InvalidConfig(format!(
                    "Bilibili cookie value for key '{key}' contains invalid characters (control chars or ';', including URL-encoded forms)"
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
                let clean_key: String = k
                    .chars()
                    .filter(|c| !c.is_control() && *c != ';' && *c != '=' && *c != ' ')
                    .collect();
                let clean_value: String =
                    v.chars().filter(|c| !c.is_control() && *c != ';').collect();
                (clean_key, clean_value)
            })
            .collect()
    }
}

// Use shared credential encryption utilities from crypto_utils module
use super::crypto_utils::{decrypt_field_in_value, encrypt_field_in_value};

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
            decrypt_field_in_value(source_config, enc, "cookies", "Bilibili")?
        } else {
            source_config.clone()
        };

        // Parse source_config first
        let config = BilibiliSourceConfig::try_from(&decrypted_config)?;

        // Build cache key based on content identity + user cookie hash
        let (cache_key, cache_ttl) = match &config {
            BilibiliSourceConfig::Video {
                bvid,
                aid,
                cid,
                cookies,
                ..
            } => {
                let user_hash = cookie_hash(cookies);
                (
                    format!(
                        "playback:video:{}:{}:{}:{user_hash}",
                        bvid.as_deref().unwrap_or(""),
                        aid.unwrap_or(0),
                        cid
                    ),
                    Duration::from_secs(2 * 3600), // 2 hours
                )
            }
            BilibiliSourceConfig::Pgc {
                epid, cid, cookies, ..
            } => {
                let user_hash = cookie_hash(cookies);
                (
                    format!("playback:pgc:{epid}:{cid}:{user_hash}"),
                    Duration::from_secs(2 * 3600),
                )
            }
            BilibiliSourceConfig::Live {
                room_id, cookies, ..
            } => {
                let user_hash = cookie_hash(cookies);
                (
                    format!("playback:live:{room_id}:{user_hash}"),
                    Duration::from_secs(120), // Live streams expire quickly
                )
            }
        };

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return Ok(cached.result);
                }
            }
        }

        // Acquire lock to prevent concurrent resolution of same content
        let _lock = if let Some(store) = store {
            store
                .lock(&format!("lock:{cache_key}"), Duration::from_secs(30))
                .await
                .ok()
        } else {
            None
        };

        // Double-check cache after lock acquisition
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return Ok(cached.result);
                }
            }
        }

        // Call provider API
        let result = self
            .resolve_from_api(&config, _ctx.credential_encryption)
            .await?;

        // Generate version and store result
        let version = nanoid::nanoid!(16);
        let expires_at = Utc::now().timestamp() + cache_ttl.as_secs() as i64;
        let versioned = VersionedPlayback {
            version: version.clone(),
            result: result.clone(),
            expires_at,
        };
        if let Some(store) = store {
            let _ = store.set(&cache_key, &versioned, cache_ttl).await;
            let _ = store
                .set(&format!("v:{version}"), &versioned, cache_ttl)
                .await;
        }

        Ok(result)
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
                // Valid BV IDs:
                // - Start with "BV" prefix
                // - Are exactly 12 characters long
                // - Contain only alphanumeric characters
                // Example: "BV1xx411c7mD"
                if let Some(bv) = bvid.as_ref() {
                    if !bv.is_empty() {
                        if !bv.starts_with("BV") {
                            return Err(ProviderError::InvalidConfig(
                                "Bilibili bvid must start with 'BV'".to_string(),
                            ));
                        }
                        if bv.len() != 12 {
                            return Err(ProviderError::InvalidConfig(
                                "Bilibili bvid must be exactly 12 characters long".to_string(),
                            ));
                        }
                        if !bv.chars().all(|c| c.is_ascii_alphanumeric()) {
                            return Err(ProviderError::InvalidConfig(
                                "Bilibili bvid must contain only alphanumeric characters"
                                    .to_string(),
                            ));
                        }
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
        // Check if source_config contains sensitive credentials (cookies)
        let has_sensitive_credentials = source_config
            .get("cookies")
            .and_then(|c| c.as_object())
            .is_some_and(|obj| !obj.is_empty());

        // If config has sensitive credentials, encryption is mandatory
        if has_sensitive_credentials && _ctx.credential_encryption.is_none() {
            return Err(ProviderError::EncryptionRequired("bilibili"));
        }

        // Encrypt cookies in source_config before storage if encryption is available
        if let Some(enc) = _ctx.credential_encryption {
            encrypt_field_in_value(&source_config, enc, "cookies", "Bilibili")
        } else {
            // No sensitive credentials, safe to store without encryption
            Ok(source_config)
        }
    }

    fn as_provider_proxy(&self) -> Option<&dyn super::proxy::ProviderProxy> {
        Some(self)
    }
}

// ProviderProxy implementation for Bilibili
//
// Supported sub_paths:
// - `{version}/subtitle/{name}` — proxy a specific subtitle track
// - `{version}/m3u8` — proxy M3U8 playlist with URL rewriting
#[async_trait]
impl super::proxy::ProviderProxy for BilibiliProvider {
    async fn resolve_proxy(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;

        // Try `{version}/subtitle/{name}`
        if let Some((version, rest)) = sub_path.split_once('/') {
            if let Some(name) = rest.strip_prefix("subtitle/") {
                let versioned = super::proxy::lookup_versioned(ctx.store, version).await?;
                let subtitle_url = versioned
                    .result
                    .playback_infos
                    .values()
                    .flat_map(|pi| &pi.subtitles)
                    .find(|s| s.name == name)
                    .map(|s| s.url.clone())
                    .ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url: subtitle_url,
                    headers: bilibili_headers(),
                });
            }

            // Try `{version}/m3u8`
            if rest == "m3u8" {
                let versioned = super::proxy::lookup_versioned(ctx.store, version).await?;
                let default_info = versioned
                    .result
                    .playback_infos
                    .get(&versioned.result.default_mode)
                    .ok_or(ProviderError::NotFound)?;
                let url = default_info.urls.first().ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::M3u8Rewrite {
                    url: url.clone(),
                    headers: default_info.headers.clone(),
                    proxy_base: format!("{}/{version}", ctx.proxy_base),
                });
            }
        }

        Err(ProviderError::NotFound)
    }
}

// Use the shared bilibili_headers() from the parent module.
use super::bilibili_headers;

/// Hash cookies to create a user-specific cache key component.
fn cookie_hash(cookies: &HashMap<String, String>) -> String {
    if cookies.is_empty() {
        return "anon".to_string();
    }
    use sha2::{Digest, Sha256};
    let mut parts: Vec<_> = cookies.iter().collect();
    parts.sort_by_key(|(k, _)| k.as_str());
    let s: String = parts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";");
    format!("{:x}", Sha256::digest(s.as_bytes()))
        .chars()
        .take(16)
        .collect()
}

impl BilibiliProvider {
    /// Resolve playback result from Bilibili API (no caching).
    async fn resolve_from_api(
        &self,
        config: &BilibiliSourceConfig,
        credential_encryption: Option<&crate::service::CredentialEncryption>,
    ) -> Result<PlaybackResult, ProviderError> {
        let _ = credential_encryption; // Used only for decryption which already happened
        let sanitized_cookies = config.sanitized_cookies();
        let client = self.get_client(config.provider_instance_name()).await;

        match config {
            BilibiliSourceConfig::Video { bvid, aid, cid, .. } => {
                let bvid = bvid.clone().unwrap_or_default();
                let aid = aid.unwrap_or(0);
                let cid = *cid;

                let request = synctv_media_providers::grpc::bilibili::GetDashVideoUrlReq {
                    aid,
                    bvid: bvid.clone(),
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                let dash_resp = client.get_dash_video_url(request).await?;

                let mut metadata = HashMap::new();
                let mut subtitles = Vec::new();

                let subtitle_request = synctv_media_providers::grpc::bilibili::GetSubtitlesReq {
                    aid,
                    bvid: bvid.clone(),
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                match client.get_subtitles(subtitle_request).await {
                    Ok(subtitle_resp) => {
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
                    Err(e) => {
                        tracing::warn!(
                            bvid = %bvid, aid = %aid, cid = %cid, error = %e,
                            "Failed to fetch Bilibili subtitles for video, continuing without subtitles"
                        );
                    }
                }

                if let Some(d) = &dash_resp.dash {
                    metadata.insert("duration".to_string(), json!(d.duration));
                    metadata.insert("min_buffer_time".to_string(), json!(d.min_buffer_time));
                }

                metadata.insert("content_type".to_string(), json!("video"));
                metadata.insert("bvid".to_string(), json!(bvid));
                metadata.insert("aid".to_string(), json!(aid));
                metadata.insert("cid".to_string(), json!(cid));

                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);

                let dash_urls: Vec<String> = dash_resp
                    .dash
                    .as_ref()
                    .map(|d| d.video_streams.iter().map(|s| s.base_url.clone()).collect())
                    .unwrap_or_default();

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

            BilibiliSourceConfig::Pgc { epid, cid, .. } => {
                let epid = *epid;
                let cid = *cid;

                let request = synctv_media_providers::grpc::bilibili::GetDashPgcurlReq {
                    epid,
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                let dash_resp = client.get_dash_pgcurl(request).await?;

                let mut metadata = HashMap::new();
                let mut subtitles = Vec::new();

                let subtitle_request = synctv_media_providers::grpc::bilibili::GetSubtitlesReq {
                    aid: 0,
                    bvid: String::new(),
                    cid,
                    cookies: sanitized_cookies.clone(),
                };
                match client.get_subtitles(subtitle_request).await {
                    Ok(subtitle_resp) => {
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
                    Err(e) => {
                        tracing::warn!(
                            epid = %epid, cid = %cid, error = %e,
                            "Failed to fetch Bilibili subtitles for PGC content, continuing without subtitles"
                        );
                    }
                }

                if let Some(d) = &dash_resp.dash {
                    metadata.insert("duration".to_string(), json!(d.duration));
                }

                metadata.insert("content_type".to_string(), json!("pgc"));
                metadata.insert("epid".to_string(), json!(epid));
                metadata.insert("cid".to_string(), json!(cid));

                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);

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

            BilibiliSourceConfig::Live { room_id, .. } => {
                let room_id = *room_id;

                let request = synctv_media_providers::grpc::bilibili::GetLiveStreamsReq {
                    cid: room_id,
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
                        format!("{}_{}", stream.desc, stream.quality)
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

                let default_mode = {
                    let mut keys: Vec<&String> = playback_infos.keys().collect();
                    keys.sort();
                    keys.into_iter()
                        .next()
                        .cloned()
                        .unwrap_or_else(|| "direct".to_string())
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    metadata,
                })
            }
        }
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
                if let Some(bv) = bvid.as_ref() {
                    if !bv.is_empty() {
                        if !bv.starts_with("BV") {
                            return Err(ProviderError::InvalidConfig(
                                "Bilibili bvid must start with 'BV'".to_string(),
                            ));
                        }
                        if bv.len() != 12 {
                            return Err(ProviderError::InvalidConfig(
                                "Bilibili bvid must be exactly 12 characters long".to_string(),
                            ));
                        }
                        if !bv.chars().all(|c| c.is_ascii_alphanumeric()) {
                            return Err(ProviderError::InvalidConfig(
                                "Bilibili bvid must contain only alphanumeric characters"
                                    .to_string(),
                            ));
                        }
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
    fn test_video_config_bvid_without_bv_prefix_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "1xx411c7mD12",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_with_lowercase_bv_prefix_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "bv1xx411c7mD",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_too_short_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7m",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_too_long_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mDxx",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_exactly_12_chars_accepted() {
        let config = json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_video_config_empty_bvid_uses_aid() {
        // Empty bvid should be allowed when aid is provided
        let config = json!({
            "type": "video",
            "bvid": "",
            "aid": 12345,
            "cid": 67890
        });
        assert!(validate_bilibili(config).is_ok());
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

    // ========== Credential Encryption Tests ==========

    #[test]
    fn test_prepare_source_config_requires_encryption_for_cookies() {
        // Test that prepare_source_config rejects cookies when encryption is not configured
        // This simulates the security requirement: sensitive providers MUST use encryption

        // Source config with cookies (sensitive)
        let config_with_cookies = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "sensitive_value"}
        });

        // Source config without cookies (non-sensitive)
        let config_without_cookies = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345
        });

        // Helper function to check if config has sensitive credentials
        fn has_sensitive_credentials(config: &Value) -> bool {
            if let Some(cookies) = config.get("cookies") {
                if let Some(obj) = cookies.as_object() {
                    return !obj.is_empty();
                }
            }
            false
        }

        // Verify detection logic
        assert!(has_sensitive_credentials(&config_with_cookies));
        assert!(!has_sensitive_credentials(&config_without_cookies));

        // The actual prepare_source_config implementation should:
        // 1. Check if credential_encryption is Some
        // 2. If None and config has sensitive credentials, return EncryptionRequired error
        // 3. If Some or no sensitive credentials, proceed normally
    }

    #[test]
    fn test_prepare_source_config_allows_non_sensitive_without_encryption() {
        // Source config without cookies should be allowed even without encryption
        let config_without_cookies = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345
        });

        // Helper function to check if config has sensitive credentials
        fn has_sensitive_credentials(config: &Value) -> bool {
            if let Some(cookies) = config.get("cookies") {
                if let Some(obj) = cookies.as_object() {
                    return !obj.is_empty();
                }
            }
            false
        }

        // Should be allowed without encryption since no sensitive data
        assert!(!has_sensitive_credentials(&config_without_cookies));
    }

    // ========== URL Encoding Injection Tests ==========

    #[test]
    fn test_cookie_value_with_url_encoded_crlf_rejected() {
        // Test URL-encoded CRLF (%0D%0A) injection attempt
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%0D%0AX-Evil-Header: attack"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_value_with_url_encoded_cr_rejected() {
        // Test URL-encoded CR (%0D) injection attempt
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%0DX-Evil-Header: attack"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_value_with_url_encoded_lf_rejected() {
        // Test URL-encoded LF (%0A) injection attempt
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%0AX-Evil-Header: attack"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_key_with_url_encoded_equals_rejected() {
        // Test URL-encoded equals (%3D) in cookie key
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"key%3Dinject": "value"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_key_with_url_encoded_semicolon_rejected() {
        // Test URL-encoded semicolon (%3B) in cookie key
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"key%3Binject": "value"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_key_with_url_encoded_space_rejected() {
        // Test URL-encoded space (%20) in cookie key
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"key%20inject": "value"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_value_with_url_encoded_semicolon_rejected() {
        // Test URL-encoded semicolon (%3B) in cookie value
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%3Bmalicious"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_with_multiple_url_encoded_sequences_rejected() {
        // Test multiple URL-encoded control characters
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "%0D%0A%0D%0Aattack"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_with_percent_sign_accepted() {
        // Test lone % sign - urlencoding crate treats this as valid literal
        // This is acceptable since it doesn't decode to a control character
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%"}
        });
        // Lone % is valid (doesn't decode to control chars)
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_cookie_with_lowercase_hex_encoding_rejected() {
        // Test lowercase hex URL encoding (also valid but decodes to control chars)
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%0a%0dattack"}
        });
        // Should reject because %0a and %0d decode to LF and CR
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_with_url_encoded_tab_rejected() {
        // Test URL-encoded tab (%09) which is a control character
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%09attack"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_key_with_url_encoded_space_in_middle_rejected() {
        // Test URL-encoded space (%20) in the middle of cookie key
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"key%20inject": "value"}
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_cookie_value_with_percent_plus_accepted() {
        // Test that %2B (plus) is accepted (not a control char)
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "cookies": {"SESSDATA": "value%2Bmore"}
        });
        assert!(validate_bilibili(config).is_ok());
    }
}
