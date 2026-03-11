//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    provider_client::{create_remote_bilibili_client, BilibiliClientArc, ProviderClientManager},
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
    client_manager: Arc<ProviderClientManager>,
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
    /// Provider type name constant.
    pub const NAME: &'static str = "bilibili";

    /// Create a new `BilibiliProvider` with `RemoteProviderManager`
    #[must_use]
    pub fn new(provider_instance_manager: Arc<RemoteProviderManager>) -> Self {
        Self {
            provider_instance_manager,
            client_manager: Arc::new(ProviderClientManager::new()),
        }
    }

    #[must_use]
    pub const fn with_client_manager(
        provider_instance_manager: Arc<RemoteProviderManager>,
        client_manager: Arc<ProviderClientManager>,
    ) -> Self {
        Self {
            provider_instance_manager,
            client_manager,
        }
    }

    /// Get Bilibili client for the given instance name (remote if available, local fallback)
    async fn get_client(
        &self,
        instance_name: Option<&str>,
    ) -> Result<BilibiliClientArc, ProviderError> {
        self.provider_instance_manager
            .resolve_client_required(instance_name, create_remote_bilibili_client, || {
                self.client_manager.local_bilibili_client()
            })
            .await
    }

    // ========== Provider API Methods ==========

    /// Match URL to determine type and ID
    pub async fn r#match(
        &self,
        url: String,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::MatchResp, ProviderError> {
        let client = self.get_client(instance_name).await?;
        let req = synctv_media_providers::grpc::bilibili::MatchReq { url };
        client.r#match(req).await.map_err(std::convert::Into::into)
    }

    /// Parse video page
    pub async fn parse_video_page(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseVideoPageReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
        client.new_sms(req).await.map_err(std::convert::Into::into)
    }

    /// Login with SMS code
    pub async fn login_with_sms(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithSmsReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithSmsResp, ProviderError> {
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        let client = self.get_client(instance_name).await?;
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
        provider_instance_name: Option<String>,
        /// Reference to stored credentials (server-side)
        credential_ref: super::credential_resolver::CredentialRef,
    },
    Pgc {
        epid: u64,
        cid: u64,
        #[serde(default)]
        provider_instance_name: Option<String>,
        /// Reference to stored credentials (server-side)
        credential_ref: super::credential_resolver::CredentialRef,
    },
    Live {
        room_id: u64,
        #[serde(default)]
        provider_instance_name: Option<String>,
        /// Reference to stored credentials (server-side)
        credential_ref: super::credential_resolver::CredentialRef,
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

    /// Get a reference to the credential_ref from any variant
    const fn credential_ref(&self) -> &super::credential_resolver::CredentialRef {
        match self {
            Self::Video { credential_ref, .. }
            | Self::Pgc { credential_ref, .. }
            | Self::Live { credential_ref, .. } => credential_ref,
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
        Self::NAME
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        // Parse source_config
        let config = BilibiliSourceConfig::try_from(source_config)?;

        // Resolve cookies from DB using credential_ref
        let repo = _ctx.credential_repo.ok_or_else(|| {
            ProviderError::Internal("credential_repo not available in ProviderContext".to_string())
        })?;

        let cred_ref = config.credential_ref();
        let credential =
            super::credential_resolver::resolve_credential(repo, Self::NAME, cred_ref).await?;

        let cookies = match credential {
            crate::models::ProviderCredential::Bilibili { cookies } => cookies,
            _ => return Err(ProviderError::InvalidCredentialType),
        };

        // Build cache key based on content identity + server_id (user-specific)
        let (cache_key, cache_ttl) = match &config {
            BilibiliSourceConfig::Video {
                bvid,
                aid,
                cid,
                credential_ref,
                ..
            } => (
                format!(
                    "playback:video:{}:{}:{}:{}",
                    bvid.as_deref().unwrap_or(""),
                    aid.unwrap_or(0),
                    cid,
                    credential_ref.credential_owner_id
                ),
                Duration::from_hours(2), // 2 hours
            ),
            BilibiliSourceConfig::Pgc {
                epid,
                cid,
                credential_ref,
                ..
            } => (
                format!(
                    "playback:pgc:{epid}:{cid}:{}",
                    credential_ref.credential_owner_id
                ),
                Duration::from_hours(2),
            ),
            BilibiliSourceConfig::Live {
                room_id,
                credential_ref,
                ..
            } => (
                format!(
                    "playback:live:{room_id}:{}",
                    credential_ref.credential_owner_id
                ),
                Duration::from_mins(2), // Live streams expire quickly
            ),
        };

        let store = _ctx.store.as_ref();

        // Check cache
        if let Some(store) = store {
            if let Ok(Some(cached)) = store.get::<VersionedPlayback>(&cache_key).await {
                if !cached.is_expired() {
                    return super::maybe_sign_cached_versioned_playback(cached, Self::NAME, _ctx)
                        .await;
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
                    return super::maybe_sign_cached_versioned_playback(cached, Self::NAME, _ctx)
                        .await;
                }
            }
        }

        // Call provider API with resolved cookies
        let result = self
            .resolve_from_api_with_cookies(&config, &cookies)
            .await?;

        // Generate version and store result
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
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

        // Verify credential_ref points to an existing credential
        if let Some(repo) = _ctx.credential_repo {
            let cred_ref = config.credential_ref();
            repo.get_by_provider_and_server(
                &cred_ref.credential_owner_id,
                Self::NAME,
                &cred_ref.server_id,
            )
            .await
            .map_err(|e| {
                ProviderError::Internal(format!("Failed to verify credential reference: {e}"))
            })?
            .ok_or_else(|| {
                ProviderError::CredentialNotFound(
                    "Referenced bilibili credential does not exist".to_string(),
                )
            })?;
        }

        Ok(())
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: Value,
    ) -> Result<Value, ProviderError> {
        // Inject credential_owner_id from context (server-side, not trusting client)
        let mut config = source_config;
        if let Some(user_id) = _ctx.user_id {
            if let Some(cred_ref) = config.get_mut("credential_ref") {
                cred_ref["credential_owner_id"] = serde_json::Value::String(user_id.to_string());
            }
        }
        Ok(config)
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
// - `{room_id}/{media_id}/danmu` — danmaku server connection info (JSON)
#[async_trait]
impl super::proxy::ProviderProxy for BilibiliProvider {
    async fn resolve_proxy(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;

        // Try `{room_id}/{media_id}/danmu`
        if sub_path.ends_with("/danmu") {
            return self.resolve_danmu(ctx).await;
        }

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

                // Propagate HMAC signature into M3U8 segment URLs
                let proxy_base = if let Some(claims) = ctx.verified_claims {
                    let signed_query = ctx.services.signing_key.build_signed_query(claims);
                    format!("{}/{version}?{signed_query}", ctx.proxy_base)
                } else {
                    format!("{}/{version}", ctx.proxy_base)
                };
                return Ok(super::proxy::ProxyAction::M3u8Rewrite {
                    url: url.clone(),
                    headers: default_info.headers.clone(),
                    proxy_base,
                });
            }
        }

        Err(ProviderError::NotFound)
    }
}

// Use the shared bilibili_headers() from the parent module.
use super::bilibili_headers;

impl BilibiliProvider {
    /// Resolve danmaku connection info from a media item's source config.
    ///
    /// Parses sub_path as `{room_id}/{media_id}/danmu`, resolves the media,
    /// fetches danmu info from Bilibili, and returns a JSON response.
    async fn resolve_danmu(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        use crate::models::{MediaId, RoomId};

        // Parse `{room_id}/{media_id}/danmu`
        let parts: Vec<&str> = ctx.sub_path.splitn(3, '/').collect();
        let (room_id_str, media_id_str) = match parts.as_slice() {
            [room, media, "danmu"] => (*room, *media),
            _ => return Err(ProviderError::NotFound),
        };

        let room_id = RoomId::from_string(room_id_str.to_string());
        let media_id = MediaId::from_string(media_id_str.to_string());

        // Check room membership and get media
        let media = ctx
            .services
            .room_service
            .media_service()
            .get_media(&media_id)
            .await
            .map_err(|e| ProviderError::ApiError(format!("Failed to get media: {e}")))?
            .ok_or(ProviderError::NotFound)?;

        if media.room_id != room_id {
            return Err(ProviderError::NotFound);
        }

        // Parse source_config to extract live stream info
        let config = BilibiliSourceConfig::try_from(&media.source_config)
            .map_err(|e| ProviderError::ApiError(format!("Failed to parse source config: {e}")))?;

        match &config {
            BilibiliSourceConfig::Live {
                room_id: bilibili_room_id,
                provider_instance_name,
                credential_ref,
                ..
            } => {
                // Resolve cookies from credential store
                let cookies = {
                    let repo = &ctx.services.credential_repo;
                    let credential = super::credential_resolver::resolve_credential(
                        repo,
                        Self::NAME,
                        credential_ref,
                    )
                    .await?;
                    match credential {
                        crate::models::ProviderCredential::Bilibili { cookies } => cookies,
                        _ => return Err(ProviderError::InvalidCredentialType),
                    }
                };

                let danmu_resp = self
                    .get_live_danmu_info(
                        *bilibili_room_id,
                        cookies,
                        provider_instance_name.as_deref(),
                    )
                    .await?;

                let event_data = serde_json::json!({
                    "token": danmu_resp.token,
                    "host_list": danmu_resp.host_list.iter().map(|h| {
                        serde_json::json!({
                            "host": h.host,
                            "port": h.port,
                            "wss_port": h.wss_port,
                            "ws_port": h.ws_port,
                        })
                    }).collect::<Vec<_>>(),
                });

                Ok(super::proxy::ProxyAction::DirectBody {
                    body: serde_json::to_vec(&event_data).unwrap_or_default(),
                    content_type: "application/json".to_string(),
                    status: 200,
                })
            }
            _ => Err(ProviderError::ApiError(
                "Danmaku is only available for Bilibili live streams".to_string(),
            )),
        }
    }

    /// Resolve playback result from Bilibili API (no caching).
    /// Cookies are resolved from the credential store, not from source_config.
    async fn resolve_from_api_with_cookies(
        &self,
        config: &BilibiliSourceConfig,
        cookies: &HashMap<String, String>,
    ) -> Result<PlaybackResult, ProviderError> {
        let sanitized_cookies = cookies.clone();
        let client = self.get_client(config.provider_instance_name()).await?;

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

    fn test_cred_ref() -> serde_json::Value {
        json!({
            "credential_owner_id": "user123",
            "server_id": "bilibili"
        })
    }

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
        Ok(())
    }

    #[test]
    fn test_valid_video_config_with_bvid() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_valid_video_config_with_aid() {
        let config = json!({
            "type": "video",
            "aid": 12345,
            "cid": 67890,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_video_config_missing_bvid_and_aid() {
        let config = json!({
            "type": "video",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_zero_cid() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 0,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_valid_pgc_config() {
        let config = json!({
            "type": "pgc",
            "epid": 12345,
            "cid": 67890,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_pgc_config_zero_epid() {
        let config = json!({
            "type": "pgc",
            "epid": 0,
            "cid": 67890,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_valid_live_config() {
        let config = json!({
            "type": "live",
            "room_id": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_live_config_zero_room_id() {
        let config = json!({
            "type": "live",
            "room_id": 0,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_invalid_type() {
        let config = json!({
            "type": "unknown_type",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_invalid_bvid_rejects_injection() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx/../../../etc/passwd",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_invalid_bvid_rejects_special_chars() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx;DROP TABLE",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_without_bv_prefix_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "1xx411c7mD12",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_with_lowercase_bv_prefix_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "bv1xx411c7mD",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_too_short_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7m",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_too_long_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mDxx",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_video_config_bvid_exactly_12_chars_accepted() {
        let config = json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_video_config_empty_bvid_uses_aid() {
        let config = json!({
            "type": "video",
            "bvid": "",
            "aid": 12345,
            "cid": 67890,
            "credential_ref": test_cred_ref()
        });
        assert!(validate_bilibili(config).is_ok());
    }

    #[test]
    fn test_missing_credential_ref_rejected() {
        // credential_ref is required, config without it should fail to parse
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345
        });
        assert!(validate_bilibili(config).is_err());
    }

    #[test]
    fn test_credential_ref_fields() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "credential_ref": {
                "credential_owner_id": "user456",
                "server_id": "bilibili"
            }
        });
        let parsed = BilibiliSourceConfig::try_from(&config).unwrap();
        let cred_ref = parsed.credential_ref();
        assert_eq!(cred_ref.credential_owner_id, "user456");
        assert_eq!(cred_ref.server_id, "bilibili");
    }
}
