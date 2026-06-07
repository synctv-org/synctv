//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    provider_client::{create_remote_bilibili_client, BilibiliClientArc, ProviderClientManager},
    store::{ProviderStoreExt, VersionedPlayback},
    MediaProvider, PlaybackInfo, PlaybackResult, ProviderContext, ProviderCredentialDependency,
    ProviderError, SourceConfig, SubtitleTrack,
};
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use crate::models::UserId;
use crate::service::RemoteProviderManager;

/// Bilibili `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct BilibiliProvider {
    provider_instance_manager: Option<Arc<RemoteProviderManager>>,
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
    pub fn new(
        provider_instance_manager: Arc<RemoteProviderManager>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager: Some(provider_instance_manager),
            client_manager: Arc::new(ProviderClientManager::new()?),
        })
    }

    #[must_use]
    pub fn with_client_manager(
        provider_instance_manager: Arc<RemoteProviderManager>,
        client_manager: Arc<ProviderClientManager>,
    ) -> Self {
        Self::with_optional_manager_and_client_manager(
            Some(provider_instance_manager),
            client_manager,
        )
    }

    #[must_use]
    pub fn with_optional_manager_and_client_manager(
        provider_instance_manager: Option<Arc<RemoteProviderManager>>,
        client_manager: Arc<ProviderClientManager>,
    ) -> Self {
        Self {
            provider_instance_manager,
            client_manager,
        }
    }

    pub fn new_local_only() -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager: None,
            client_manager: Arc::new(ProviderClientManager::new()?),
        })
    }

    async fn get_client_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliClientArc, ProviderError> {
        match (instance_name, self.provider_instance_manager.as_ref()) {
            (None, _) => Ok(self.client_manager.local_bilibili_client()),
            (Some(_), Some(manager)) => {
                manager
                    .resolve_client_required_with_context(
                        instance_name,
                        request_context,
                        create_remote_bilibili_client,
                        || self.client_manager.local_bilibili_client(),
                    )
                    .await
            }
            (Some(_), None) => Err(ProviderError::Internal(
                "provider instance manager is required for remote Bilibili playback resolution"
                    .to_string(),
            )),
        }
    }

    /// Match URL to determine type and ID
    pub async fn r#match(
        &self,
        url: String,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::MatchResp, ProviderError> {
        self.r#match_with_context(url, instance_name, None).await
    }

    pub async fn r#match_with_context(
        &self,
        url: String,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::MatchResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let req = synctv_media_providers::grpc::bilibili::MatchReq { url };
        client.r#match(req).await.map_err(std::convert::Into::into)
    }

    /// Parse video page
    pub async fn parse_video_page(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseVideoPageReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        self.parse_video_page_with_context(req, instance_name, None)
            .await
    }

    pub async fn parse_video_page_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseVideoPageReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
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
        self.parse_pgc_page_with_context(req, instance_name, None)
            .await
    }

    pub async fn parse_pgc_page_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParsePgcPageReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
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
        self.parse_live_page_with_context(req, instance_name, None)
            .await
    }

    pub async fn parse_live_page_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::ParseLivePageReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::VideoPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
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
        self.new_qr_code_with_context(instance_name, None).await
    }

    pub async fn new_qr_code_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewQrCodeResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
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
        self.login_with_qr_code_with_context(req, instance_name, None)
            .await
    }

    pub async fn login_with_qr_code_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithQrCodeReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithQrCodeResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
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
        self.new_captcha_with_context(instance_name, None).await
    }

    pub async fn new_captcha_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewCaptchaResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
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
        self.new_sms_with_context(req, instance_name, None).await
    }

    pub async fn new_sms_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::NewSmsReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::NewSmsResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.new_sms(req).await.map_err(std::convert::Into::into)
    }

    /// Login with SMS code
    pub async fn login_with_sms(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithSmsReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithSmsResp, ProviderError> {
        self.login_with_sms_with_context(req, instance_name, None)
            .await
    }

    pub async fn login_with_sms_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::LoginWithSmsReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::LoginWithSmsResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
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
        self.user_info_with_context(req, instance_name, None).await
    }

    pub async fn user_info_with_context(
        &self,
        req: synctv_media_providers::grpc::bilibili::UserInfoReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::UserInfoResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .user_info(req)
            .await
            .map_err(std::convert::Into::into)
    }

    /// Get live danmaku server info for the WebSocket connection
    pub async fn get_live_danmu_info(
        &self,
        room_id: u64,
        cookies: HashMap<String, String>,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp, ProviderError> {
        self.get_live_danmu_info_with_context(room_id, cookies, instance_name, None)
            .await
    }

    pub async fn get_live_danmu_info_with_context(
        &self,
        room_id: u64,
        cookies: HashMap<String, String>,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let req = synctv_media_providers::grpc::bilibili::GetLiveDanmuInfoReq { cookies, room_id };
        client
            .get_live_danmu_info(req)
            .await
            .map_err(std::convert::Into::into)
    }
}

/// Bilibili source configuration structs
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum BilibiliSourceConfig {
    Video {
        bvid: Option<String>,
        aid: Option<u64>,
        cid: u64,
        /// Whether playback should use the media creator's Bilibili login.
        #[serde(default)]
        shared: bool,
    },
    Pgc {
        epid: u64,
        cid: u64,
        /// Whether playback should use the media creator's Bilibili login.
        #[serde(default)]
        shared: bool,
    },
    Live {
        room_id: u64,
        /// Whether playback should use the media creator's Bilibili login.
        #[serde(default)]
        shared: bool,
    },
}

impl BilibiliSourceConfig {
    const fn shared(&self) -> bool {
        match self {
            Self::Video { shared, .. } | Self::Pgc { shared, .. } | Self::Live { shared, .. } => {
                *shared
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BilibiliVideoIdentifier {
    bvid: String,
    aid: u64,
}

impl BilibiliVideoIdentifier {
    fn parse(bvid: Option<&str>, aid: Option<u64>) -> Result<Self, ProviderError> {
        let bvid = bvid
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let aid = match aid {
            Some(value) if value > 0 => value,
            _ => 0,
        };
        if bvid.is_none() && aid == 0 {
            return Err(ProviderError::InvalidConfig(
                "Bilibili video requires either bvid or aid".to_string(),
            ));
        }
        if let Some(bvid) = bvid.as_deref() {
            if !bvid.starts_with("BV") {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili bvid must start with 'BV'".to_string(),
                ));
            }
            if bvid.len() != 12 {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili bvid must be exactly 12 characters long".to_string(),
                ));
            }
            if !bvid.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili bvid must contain only alphanumeric characters".to_string(),
                ));
            }
        }
        Ok(Self {
            bvid: bvid.unwrap_or_default(),
            aid,
        })
    }

    fn cache_key_part(&self) -> String {
        match (self.bvid.is_empty(), self.aid) {
            (false, 0) => format!("bvid:{}", self.bvid),
            (true, aid) => format!("aid:{aid}"),
            (false, aid) => format!("bvid:{}:aid:{aid}", self.bvid),
        }
    }
}

fn resolve_bilibili_video_identifier(
    bvid: Option<&str>,
    aid: Option<u64>,
) -> Result<(String, u64), ProviderError> {
    let identifier = BilibiliVideoIdentifier::parse(bvid, aid)?;
    Ok((identifier.bvid, identifier.aid))
}

fn non_empty_playback_urls<I>(urls: I, context: &str) -> Result<Vec<String>, ProviderError>
where
    I: IntoIterator<Item = String>,
{
    let urls = urls
        .into_iter()
        .filter(|url| !url.trim().is_empty())
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return Err(ProviderError::ApiError(format!(
            "Bilibili {context} playback response did not include playable URLs"
        )));
    }
    Ok(urls)
}

fn bilibili_credential_server_id() -> String {
    crate::models::UserProviderCredential::bilibili_server_id()
}

fn is_bilibili_pgc_dash_unavailable(error: &synctv_media_providers::ProviderClientError) -> bool {
    matches!(
        error,
        synctv_media_providers::ProviderClientError::Api { message, .. }
            if message.contains("DASH")
    )
}

fn playback_cache_entry(
    config: &BilibiliSourceConfig,
    credential_cache_partition: &str,
) -> Result<(String, Duration), ProviderError> {
    match config {
        BilibiliSourceConfig::Video { bvid, aid, cid, .. } => {
            let video_key = BilibiliVideoIdentifier::parse(bvid.as_deref(), *aid)?.cache_key_part();
            Ok((
                format!("playback:video:{video_key}:{cid}:{credential_cache_partition}"),
                Duration::from_hours(2),
            ))
        }
        BilibiliSourceConfig::Pgc { epid, cid, .. } => Ok((
            format!("playback:pgc:{epid}:{cid}:{credential_cache_partition}"),
            Duration::from_hours(2),
        )),
        BilibiliSourceConfig::Live { room_id, .. } => Ok((
            format!("playback:live:{room_id}:{credential_cache_partition}"),
            Duration::from_mins(2),
        )),
    }
}

async fn resolve_optional_bilibili_cookies(
    ctx: &ProviderContext<'_>,
    credential_owner_id: UserId,
) -> Result<(HashMap<String, String>, String), ProviderError> {
    if let Some(access_service) = &ctx.provider_access_service {
        let access = access_service
            .bilibili_access(credential_owner_id, ctx.request_context())
            .await?;
        return Ok((access.cookies, access.credential_cache_partition));
    }

    let Some(repo) = ctx.credential_repo else {
        return Ok((HashMap::new(), "anon".to_string()));
    };

    let server_id = bilibili_credential_server_id();
    let credential = repo
        .get_by_provider_and_server(credential_owner_id, BilibiliProvider::NAME, &server_id)
        .await
        .map_err(|e| {
            ProviderError::Internal(format!("Failed to query bilibili credential: {e}"))
        })?;

    let Some(credential) = credential else {
        return Ok((HashMap::new(), "anon".to_string()));
    };
    if credential.is_expired() {
        return Ok((HashMap::new(), "anon".to_string()));
    }

    match credential.get_credential() {
        Ok(crate::models::ProviderCredential::Bilibili { cookies }) => Ok((
            cookies,
            format!(
                "auth:{credential_owner_id}:{server_id}:{}",
                super::credential_resolver::credential_revision(
                    credential.id,
                    credential.updated_at
                )
            ),
        )),
        Ok(_) => Err(ProviderError::InvalidCredentialType),
        Err(e) => Err(ProviderError::Internal(format!(
            "Failed to parse bilibili credential: {e}"
        ))),
    }
}

impl TryFrom<&Value> for BilibiliSourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::reject_source_config_provider_instance_name(value, "Bilibili")?;
        super::reject_source_config_credential_ref(value, "Bilibili")?;
        super::parse_source_config(value, "Bilibili")
    }
}

#[async_trait]
impl MediaProvider for BilibiliProvider {
    #[cfg(test)]
    fn test_client_manager_marker(&self) -> Option<usize> {
        Some(self.client_manager.marker())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = BilibiliSourceConfig::try_from(source_config)?;

        // Resolve cookies from DB. Shared Bilibili media uses the creator's
        // login; non-shared media uses the requesting user's own login. Missing
        // credentials are valid and fall back to anonymous playback.
        let credential_owner_id = if config.shared() {
            _ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?
        } else {
            _ctx.user_id().ok_or_else(|| {
                ProviderError::Internal("user_id not available in ProviderContext".to_string())
            })?
        };
        let (cookies, credential_cache_partition) =
            resolve_optional_bilibili_cookies(_ctx, *credential_owner_id).await?;

        let (cache_key, cache_ttl) = playback_cache_entry(&config, &credential_cache_partition)?;

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
            .resolve_from_api_with_cookies(
                &config,
                &cookies,
                super::bound_provider_instance_name(_ctx),
                _ctx.request_context(),
            )
            .await?;

        // Generate version and store result
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        // Validate that source_config parses to a known variant
        let config = BilibiliSourceConfig::try_from(source_config.value())?;

        match &config {
            BilibiliSourceConfig::Video { bvid, aid, cid, .. } => {
                BilibiliVideoIdentifier::parse(bvid.as_deref(), *aid)?;
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

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let config = BilibiliSourceConfig::try_from(source_config)?;
        let credential_owner_id = if config.shared() {
            ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?
        } else {
            ctx.user_id().ok_or_else(|| {
                ProviderError::Internal("user_id not available in ProviderContext".to_string())
            })?
        };

        Ok(vec![ProviderCredentialDependency::new(
            Self::NAME,
            credential_owner_id.to_string(),
            bilibili_credential_server_id(),
        )])
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: Value,
    ) -> Result<Value, ProviderError> {
        super::reject_source_config_provider_instance_name(&source_config, "Bilibili")?;
        super::reject_source_config_credential_ref(&source_config, "Bilibili")?;
        Ok(source_config)
    }

    fn as_provider_proxy(&self) -> Option<&dyn super::proxy::ProviderProxy> {
        Some(self)
    }
}

// ProviderProxy implementation for Bilibili
// Supported sub_paths:
// - `{version}/subtitle/{name}` — proxy a specific subtitle track
// - `{version}/subtitle/{mode}/{index}` — proxy a subtitle track for a mode
// - `{version}/m3u8` — proxy M3U8 playlist with URL rewriting
// - `{version}/m3u8/{mode}/{index}` — proxy a specific M3U8 playlist URL
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

        // Try `{version}` segment targets generated by M3U8 playlist rewriting.
        let (version, maybe_rest) = sub_path
            .split_once('/')
            .map_or((sub_path, None), |(version, rest)| (version, Some(rest)));
        if version.is_empty() {
            return Err(ProviderError::NotFound);
        }

        if let Some(url) = super::proxy::signed_target_url(ctx) {
            let versioned =
                super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
            let headers = versioned
                .result
                .playback_infos
                .get(&versioned.result.default_mode)
                .map_or_else(bilibili_headers, |info| {
                    if info.headers.is_empty() {
                        bilibili_headers()
                    } else {
                        info.headers.clone()
                    }
                });
            return super::proxy::action_for_signed_target_url(ctx, version, url, headers);
        }

        if let Some(rest) = maybe_rest {
            if rest == "stream" || rest.starts_with("stream/") {
                let stream_path = rest.strip_prefix("stream/").unwrap_or("0");
                let versioned =
                    super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
                let (playback_info, index_str) =
                    if let Some((mode_name, index_str)) = stream_path.split_once('/') {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(mode_name)
                                .ok_or(ProviderError::NotFound)?,
                            index_str,
                        )
                    } else {
                        (
                            versioned
                                .result
                                .playback_infos
                                .get(&versioned.result.default_mode)
                                .ok_or(ProviderError::NotFound)?,
                            stream_path,
                        )
                    };
                let Ok(index) = index_str.parse::<usize>() else {
                    return Err(ProviderError::NotFound);
                };
                let url = playback_info
                    .urls
                    .get(index)
                    .ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url: url.clone(),
                    headers: if playback_info.headers.is_empty() {
                        bilibili_headers()
                    } else {
                        playback_info.headers.clone()
                    },
                    range_header: super::proxy::selected_range_header(ctx)?,
                });
            }

            if let Some(subtitle_path) = rest.strip_prefix("subtitle/") {
                let versioned =
                    super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
                let (subtitle_url, subtitle_headers) =
                    if let Some((mode_name, index_str)) = subtitle_path.split_once('/') {
                        let playback_info = versioned
                            .result
                            .playback_infos
                            .get(mode_name)
                            .ok_or(ProviderError::NotFound)?;
                        let index: usize = index_str.parse().map_err(|_| {
                            ProviderError::ApiError("Invalid subtitle index".into())
                        })?;
                        playback_info.subtitles.get(index).map(|subtitle| {
                            (
                                subtitle.url.clone(),
                                super::subtitle_headers_for_proxy(&playback_info.headers, subtitle),
                            )
                        })
                    } else {
                        let default_playback_info = versioned
                            .result
                            .playback_infos
                            .get(&versioned.result.default_mode)
                            .ok_or(ProviderError::NotFound)?;
                        subtitle_path
                            .parse::<usize>()
                            .ok()
                            .and_then(|index| default_playback_info.subtitles.get(index))
                            .or_else(|| {
                                default_playback_info
                                    .subtitles
                                    .iter()
                                    .find(|subtitle| subtitle.name == subtitle_path)
                            })
                            .map(|subtitle| {
                                (
                                    subtitle.url.clone(),
                                    super::subtitle_headers_for_proxy(
                                        &default_playback_info.headers,
                                        subtitle,
                                    ),
                                )
                            })
                    }
                    .ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url: subtitle_url,
                    headers: if subtitle_headers.is_empty() {
                        bilibili_headers()
                    } else {
                        subtitle_headers
                    },
                    range_header: None,
                });
            }

            if let Some(m3u8_path) = rest.strip_prefix("m3u8/") {
                let versioned =
                    super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
                let (mode_name, index_str) =
                    m3u8_path.split_once('/').ok_or(ProviderError::NotFound)?;
                let playback_info = versioned
                    .result
                    .playback_infos
                    .get(mode_name)
                    .ok_or(ProviderError::NotFound)?;
                let index = index_str
                    .parse::<usize>()
                    .map_err(|_| ProviderError::NotFound)?;
                let url = playback_info
                    .urls
                    .get(index)
                    .ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::M3u8Rewrite {
                    url: url.clone(),
                    headers: playback_info.headers.clone(),
                    proxy_base: signed_m3u8_segment_proxy_base(ctx, version),
                    proxy_url_claims: ctx.verified_claims.cloned(),
                });
            }

            // Try `{version}/m3u8`
            if rest == "m3u8" {
                let versioned =
                    super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;
                let default_info = versioned
                    .result
                    .playback_infos
                    .get(&versioned.result.default_mode)
                    .ok_or(ProviderError::NotFound)?;
                let url = default_info.urls.first().ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::M3u8Rewrite {
                    url: url.clone(),
                    headers: default_info.headers.clone(),
                    proxy_base: signed_m3u8_segment_proxy_base(ctx, version),
                    proxy_url_claims: ctx.verified_claims.cloned(),
                });
            }
        }

        Err(ProviderError::NotFound)
    }
}

// Use the shared bilibili_headers() from the parent module.
use super::bilibili_headers;

fn signed_m3u8_segment_proxy_base(
    ctx: &super::proxy::ProxyRequestContext<'_>,
    version: &str,
) -> String {
    format!("{}/{version}", ctx.proxy_base)
}

fn bilibili_subtitle_track(name: String, url: String) -> SubtitleTrack {
    SubtitleTrack {
        language: name.clone(),
        name,
        url,
        headers: bilibili_headers(),
        format: "json".to_string(),
    }
}

fn bilibili_live_headers() -> HashMap<String, String> {
    let mut headers = bilibili_headers();
    headers.insert(
        "Referer".to_string(),
        "https://live.bilibili.com".to_string(),
    );
    headers
}

impl BilibiliProvider {
    /// Resolve danmaku connection info from a media item's source config.
    ///
    /// Parses sub_path as `{room_id}/{media_id}/danmu`, resolves the media,
    /// fetches danmu info from Bilibili, and returns a JSON response.
    async fn resolve_danmu(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        // Parse `{room_id}/{media_id}/danmu`
        let parts: Vec<&str> = ctx.sub_path.splitn(3, '/').collect();
        let (room_id_str, media_id_str) = match parts.as_slice() {
            [room, media, "danmu"] => (*room, *media),
            _ => return Err(ProviderError::NotFound),
        };

        let services = ctx.services()?;
        let room_id = super::proxy::parse_proxy_room_id(
            &services.public_id_codec,
            room_id_str,
            "danmaku proxy path",
        )?;
        let media_id = super::proxy::parse_proxy_media_id(
            &services.public_id_codec,
            media_id_str,
            "danmaku proxy path",
        )?;

        let media = services
            .room_service
            .media_service()
            .get_room_media(&room_id, &media_id)
            .await
            .map_err(|e| ProviderError::ApiError(format!("Failed to get media: {e}")))?
            .ok_or(ProviderError::NotFound)?;

        // Parse source_config to extract live stream info
        let config = BilibiliSourceConfig::try_from(&media.source_config)
            .map_err(|e| ProviderError::ApiError(format!("Failed to parse source config: {e}")))?;

        match &config {
            BilibiliSourceConfig::Live {
                room_id: bilibili_room_id,
                shared,
                ..
            } => {
                // Resolve cookies from credential store
                let cookies = {
                    let credential_owner_id = if *shared {
                        media.creator_id.as_ref().copied().ok_or_else(|| {
                            ProviderError::Internal(
                                "media creator_id is required for shared Bilibili danmaku"
                                    .to_string(),
                            )
                        })?
                    } else {
                        ctx.verified_claims
                            .as_ref()
                            .ok_or_else(|| {
                                ProviderError::Internal(
                                    "verified proxy claims are required for Bilibili danmaku"
                                        .to_string(),
                                )
                            })
                            .and_then(|claims| {
                                super::proxy::parse_proxy_user_id(
                                    &services.public_id_codec,
                                    &claims.user_id,
                                    "danmaku proxy claims",
                                )
                            })?
                    };

                    if let Some(access_service) = &services.provider_access_service {
                        access_service
                            .bilibili_access(credential_owner_id, ctx.request_context)
                            .await?
                            .cookies
                    } else {
                        let repo = &services.credential_repo;
                        let server_id = bilibili_credential_server_id();
                        let credential = repo
                            .get_by_provider_and_server(credential_owner_id, Self::NAME, &server_id)
                            .await
                            .map_err(|e| {
                                ProviderError::Internal(format!(
                                    "Failed to query bilibili credential: {e}"
                                ))
                            })?;
                        match credential {
                            Some(credential) if !credential.is_expired() => {
                                match credential.get_credential() {
                                    Ok(crate::models::ProviderCredential::Bilibili { cookies }) => {
                                        cookies
                                    }
                                    Ok(_) => return Err(ProviderError::InvalidCredentialType),
                                    Err(e) => {
                                        return Err(ProviderError::Internal(format!(
                                            "Failed to parse bilibili credential: {e}"
                                        )));
                                    }
                                }
                            }
                            _ => HashMap::new(),
                        }
                    }
                };

                let danmu_resp = self
                    .get_live_danmu_info(
                        *bilibili_room_id,
                        cookies,
                        media.provider_instance_name.as_deref(),
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
                    body: serde_json::to_vec(&event_data)?,
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
        provider_instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackResult, ProviderError> {
        let sanitized_cookies = cookies.clone();
        let client = self
            .get_client_with_context(provider_instance_name, request_context)
            .await?;

        match config {
            BilibiliSourceConfig::Video { bvid, aid, cid, .. } => {
                let (bvid, aid) = resolve_bilibili_video_identifier(bvid.as_deref(), *aid)?;
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
                            .map(|(name, url)| bilibili_subtitle_track(name, url))
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

                let dash_urls = non_empty_playback_urls(
                    dash_resp.dash.as_ref().into_iter().flat_map(|dash| {
                        dash.video_streams
                            .iter()
                            .map(|stream| stream.base_url.clone())
                    }),
                    "video DASH",
                )?;

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
                let dash_resp = match client.get_dash_pgcurl(request).await {
                    Ok(dash_resp) if dash_resp.dash.is_some() => Some(dash_resp),
                    Ok(_) => None,
                    Err(error) if is_bilibili_pgc_dash_unavailable(&error) => None,
                    Err(error) => return Err(error.into()),
                };

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
                            .map(|(name, url)| bilibili_subtitle_track(name, url))
                            .collect();
                    }
                    Err(e) => {
                        tracing::warn!(
                            epid = %epid, cid = %cid, error = %e,
                            "Failed to fetch Bilibili subtitles for PGC content, continuing without subtitles"
                        );
                    }
                }

                if let Some(d) = dash_resp.as_ref().and_then(|resp| resp.dash.as_ref()) {
                    metadata.insert("duration".to_string(), json!(d.duration));
                }

                metadata.insert("content_type".to_string(), json!("pgc"));
                metadata.insert("epid".to_string(), json!(epid));
                metadata.insert("cid".to_string(), json!(cid));

                let expires_at = Some(Utc::now().timestamp() + 2 * 3600);

                let mut playback_infos = HashMap::new();
                let default_mode = if let Some(dash_resp) = dash_resp {
                    let pgc_urls = non_empty_playback_urls(
                        dash_resp.dash.as_ref().into_iter().flat_map(|dash| {
                            dash.video_streams
                                .iter()
                                .map(|stream| stream.base_url.clone())
                        }),
                        "PGC DASH",
                    )?;

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
                    "dash".to_string()
                } else {
                    let request = synctv_media_providers::grpc::bilibili::GetPgcurlReq {
                        epid,
                        cid,
                        quality: 80,
                        cookies: sanitized_cookies.clone(),
                    };
                    let pgc_resp = client.get_pgcurl(request).await?;
                    let pgc_urls = non_empty_playback_urls(
                        pgc_resp.segments.iter().map(|segment| segment.url.clone()),
                        "PGC durl",
                    )?;

                    metadata.insert("fallback_format".to_string(), json!("durl"));
                    metadata.insert("quality".to_string(), json!(pgc_resp.current_quality));
                    playback_infos.insert(
                        "mp4".to_string(),
                        PlaybackInfo {
                            urls: pgc_urls,
                            format: "mp4".to_string(),
                            headers: bilibili_headers(),
                            subtitles,
                            expires_at,
                            cors_proxy_required: true,
                        },
                    );
                    "mp4".to_string()
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
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
                            headers: bilibili_live_headers(),
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
    use crate::models::UserId;
    use crate::provider::{MediaProvider, ProviderClientManager, ProviderContext};
    use async_trait::async_trait;
    use std::sync::Arc;
    use synctv_media_providers::bilibili::{BilibiliError, BilibiliInterface};
    use synctv_media_providers::grpc::bilibili as proto;
    fn validate_bilibili(config: &Value) -> Result<(), ProviderError> {
        tokio::runtime::Runtime::new()
            .expect("runtime")
            .block_on(async {
                let provider = BilibiliProvider::new_local_only().expect("provider should build");
                provider
                    .validate_source_config(
                        &ProviderContext::new("test"),
                        SourceConfig::media(config),
                    )
                    .await
            })
    }

    #[test]
    fn video_identifier_requires_bvid_or_aid() {
        let err = resolve_bilibili_video_identifier(None, None)
            .expect_err("missing bvid and aid must fail");

        assert!(matches!(
            err,
            ProviderError::InvalidConfig(message)
                if message.contains("requires either bvid or aid")
        ));
    }

    #[test]
    fn video_identifier_rejects_zero_aid_without_bvid() {
        let err = BilibiliVideoIdentifier::parse(None, Some(0))
            .expect_err("zero aid without bvid must fail");

        assert!(matches!(
            err,
            ProviderError::InvalidConfig(message)
                if message.contains("requires either bvid or aid")
        ));
    }

    #[test]
    fn video_identifier_accepts_bvid_or_aid() {
        assert_eq!(
            BilibiliVideoIdentifier::parse(Some(" BV1xx411c7mD "), None)
                .expect("bvid should identify video"),
            BilibiliVideoIdentifier {
                bvid: "BV1xx411c7mD".to_string(),
                aid: 0,
            }
        );
        assert_eq!(
            BilibiliVideoIdentifier::parse(None, Some(42)).expect("aid should identify video"),
            BilibiliVideoIdentifier {
                bvid: String::new(),
                aid: 42,
            }
        );
    }

    #[test]
    fn video_identifier_rejects_malformed_bvid() {
        for bvid in ["av123", "BVshort", "BV1GJ411x7g!"] {
            assert!(
                BilibiliVideoIdentifier::parse(Some(bvid), None).is_err(),
                "malformed bvid should fail: {bvid}"
            );
        }
    }

    #[test]
    fn playback_urls_reject_empty_responses() {
        let err = non_empty_playback_urls(vec![String::new(), "   ".to_string()], "video DASH")
            .expect_err("empty playback URLs must fail");

        assert!(matches!(
            err,
            ProviderError::ApiError(message)
                if message.contains("did not include playable URLs")
        ));
    }

    #[test]
    fn playback_urls_filter_blank_entries() {
        assert_eq!(
            non_empty_playback_urls(
                vec![String::new(), "https://upos.example/video.m4s".to_string()],
                "video DASH",
            )
            .expect("non-empty URL should be kept"),
            vec!["https://upos.example/video.m4s".to_string()]
        );
    }

    struct TestBilibiliClient;
    struct TestBilibiliClientWithPgcDurlFallback;

    fn unconfigured_test_response() -> BilibiliError {
        BilibiliError::NotImplemented("test bilibili method is not configured".to_string())
    }

    #[async_trait]
    impl BilibiliInterface for TestBilibiliClient {
        async fn new_qr_code(
            &self,
            _request: proto::Empty,
        ) -> Result<proto::NewQrCodeResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn login_with_qr_code(
            &self,
            _request: proto::LoginWithQrCodeReq,
        ) -> Result<proto::LoginWithQrCodeResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn new_captcha(
            &self,
            _request: proto::Empty,
        ) -> Result<proto::NewCaptchaResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn new_sms(
            &self,
            _request: proto::NewSmsReq,
        ) -> Result<proto::NewSmsResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn login_with_sms(
            &self,
            _request: proto::LoginWithSmsReq,
        ) -> Result<proto::LoginWithSmsResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn parse_video_page(
            &self,
            _request: proto::ParseVideoPageReq,
        ) -> Result<proto::VideoPageInfo, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_video_url(
            &self,
            _request: proto::GetVideoUrlReq,
        ) -> Result<proto::VideoUrl, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_dash_video_url(
            &self,
            _request: proto::GetDashVideoUrlReq,
        ) -> Result<proto::GetDashVideoUrlResp, BilibiliError> {
            Ok(test_dash_response("https://upos.example/video.m4s"))
        }

        async fn get_subtitles(
            &self,
            _request: proto::GetSubtitlesReq,
        ) -> Result<proto::GetSubtitlesResp, BilibiliError> {
            Ok(proto::GetSubtitlesResp {
                subtitles: HashMap::from([(
                    "zh-CN".to_string(),
                    "https://subtitle.example/zh.json".to_string(),
                )]),
            })
        }

        async fn parse_pgc_page(
            &self,
            _request: proto::ParsePgcPageReq,
        ) -> Result<proto::VideoPageInfo, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_pgcurl(
            &self,
            _request: proto::GetPgcurlReq,
        ) -> Result<proto::VideoUrl, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_dash_pgcurl(
            &self,
            _request: proto::GetDashPgcurlReq,
        ) -> Result<proto::GetDashPgcurlResp, BilibiliError> {
            Ok(proto::GetDashPgcurlResp {
                dash: test_dash_response("https://upos.example/pgc.m4s").dash,
                hevc_dash: None,
            })
        }

        async fn user_info(
            &self,
            _request: proto::UserInfoReq,
        ) -> Result<proto::UserInfoResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn r#match(
            &self,
            _request: proto::MatchReq,
        ) -> Result<proto::MatchResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_live_streams(
            &self,
            _request: proto::GetLiveStreamsReq,
        ) -> Result<proto::GetLiveStreamsResp, BilibiliError> {
            Ok(proto::GetLiveStreamsResp {
                live_streams: vec![proto::LiveStream {
                    quality: 10000,
                    urls: vec!["https://live.example/stream.m3u8".to_string()],
                    desc: "origin".to_string(),
                }],
            })
        }

        async fn parse_live_page(
            &self,
            _request: proto::ParseLivePageReq,
        ) -> Result<proto::VideoPageInfo, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_live_danmu_info(
            &self,
            _request: proto::GetLiveDanmuInfoReq,
        ) -> Result<proto::GetLiveDanmuInfoResp, BilibiliError> {
            Err(unconfigured_test_response())
        }
    }

    #[async_trait]
    impl BilibiliInterface for TestBilibiliClientWithPgcDurlFallback {
        async fn new_qr_code(
            &self,
            _request: proto::Empty,
        ) -> Result<proto::NewQrCodeResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn login_with_qr_code(
            &self,
            _request: proto::LoginWithQrCodeReq,
        ) -> Result<proto::LoginWithQrCodeResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn new_captcha(
            &self,
            _request: proto::Empty,
        ) -> Result<proto::NewCaptchaResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn new_sms(
            &self,
            _request: proto::NewSmsReq,
        ) -> Result<proto::NewSmsResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn login_with_sms(
            &self,
            _request: proto::LoginWithSmsReq,
        ) -> Result<proto::LoginWithSmsResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn parse_video_page(
            &self,
            _request: proto::ParseVideoPageReq,
        ) -> Result<proto::VideoPageInfo, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_video_url(
            &self,
            _request: proto::GetVideoUrlReq,
        ) -> Result<proto::VideoUrl, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_dash_video_url(
            &self,
            _request: proto::GetDashVideoUrlReq,
        ) -> Result<proto::GetDashVideoUrlResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_subtitles(
            &self,
            _request: proto::GetSubtitlesReq,
        ) -> Result<proto::GetSubtitlesResp, BilibiliError> {
            Ok(proto::GetSubtitlesResp {
                subtitles: HashMap::new(),
            })
        }

        async fn parse_pgc_page(
            &self,
            _request: proto::ParsePgcPageReq,
        ) -> Result<proto::VideoPageInfo, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_pgcurl(
            &self,
            _request: proto::GetPgcurlReq,
        ) -> Result<proto::VideoUrl, BilibiliError> {
            Ok(proto::VideoUrl {
                accept_quality: vec![80],
                accept_description: vec!["1080P".to_string()],
                current_quality: 80,
                url: "https://upos.example/pgc.mp4".to_string(),
                segments: vec![proto::VideoSegment {
                    url: "https://upos.example/pgc.mp4".to_string(),
                    size: 12345,
                }],
            })
        }

        async fn get_dash_pgcurl(
            &self,
            _request: proto::GetDashPgcurlReq,
        ) -> Result<proto::GetDashPgcurlResp, BilibiliError> {
            Err(BilibiliError::Api {
                code: 0,
                message: "PGC playurl response did not include DASH streams".to_string(),
            })
        }

        async fn user_info(
            &self,
            _request: proto::UserInfoReq,
        ) -> Result<proto::UserInfoResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn r#match(
            &self,
            _request: proto::MatchReq,
        ) -> Result<proto::MatchResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_live_streams(
            &self,
            _request: proto::GetLiveStreamsReq,
        ) -> Result<proto::GetLiveStreamsResp, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn parse_live_page(
            &self,
            _request: proto::ParseLivePageReq,
        ) -> Result<proto::VideoPageInfo, BilibiliError> {
            Err(unconfigured_test_response())
        }

        async fn get_live_danmu_info(
            &self,
            _request: proto::GetLiveDanmuInfoReq,
        ) -> Result<proto::GetLiveDanmuInfoResp, BilibiliError> {
            Err(unconfigured_test_response())
        }
    }

    fn test_dash_response(url: &str) -> proto::GetDashVideoUrlResp {
        proto::GetDashVideoUrlResp {
            dash: Some(proto::DashInfo {
                duration: 120.0,
                min_buffer_time: 1.5,
                video_streams: vec![proto::VideoStream {
                    id: 80,
                    base_url: url.to_string(),
                    mime_type: "video/mp4".to_string(),
                    codecs: "avc1.640028".to_string(),
                    width: 1920,
                    height: 1080,
                    frame_rate: "60".to_string(),
                    bandwidth: 1_000_000,
                    start_with_sap: 1,
                    segment_base: None,
                }],
                audio_streams: Vec::new(),
            }),
            hevc_dash: None,
        }
    }

    fn provider_with_test_bilibili_client(client: Arc<dyn BilibiliInterface>) -> BilibiliProvider {
        let default_clients = ProviderClientManager::new_for_tests()
            .expect("default provider HTTP client should build");
        let client_manager = Arc::new(ProviderClientManager::with_custom_clients(
            default_clients.local_alist_client(),
            client,
            default_clients.local_emby_client(),
        ));
        BilibiliProvider::with_optional_manager_and_client_manager(None, client_manager)
    }

    fn provider_with_default_test_bilibili_client() -> BilibiliProvider {
        provider_with_test_bilibili_client(Arc::new(TestBilibiliClient))
    }

    fn assert_bilibili_cdn_headers(headers: &HashMap<String, String>, expected_referer: &str) {
        assert_eq!(
            headers.get("Referer"),
            Some(&expected_referer.to_string()),
            "Bilibili direct playback must return the required Referer"
        );
        assert_eq!(
            headers.get("User-Agent"),
            Some(&synctv_media_providers::error::PROVIDER_USER_AGENT.to_string()),
            "Bilibili direct playback must return the provider User-Agent"
        );
    }

    #[tokio::test]
    async fn test_video_direct_playback_returns_stream_and_subtitle_headers() {
        let provider = provider_with_default_test_bilibili_client();
        let result = provider
            .generate_playback(
                &ProviderContext::new("test").with_user_id(UserId::expect_positive(1)),
                &json!({
                    "type": "video",
                    "bvid": "BV1GJ411x7gL",
                    "cid": 12345
                }),
            )
            .await
            .expect("mock video playback should resolve");

        let dash = &result.playback_infos["dash"];
        assert_bilibili_cdn_headers(&dash.headers, "https://www.bilibili.com");
        assert_eq!(dash.subtitles.len(), 1);
        assert_bilibili_cdn_headers(&dash.subtitles[0].headers, "https://www.bilibili.com");
    }

    #[tokio::test]
    async fn test_pgc_direct_playback_returns_stream_and_subtitle_headers() {
        let provider = provider_with_default_test_bilibili_client();
        let result = provider
            .generate_playback(
                &ProviderContext::new("test").with_user_id(UserId::expect_positive(1)),
                &json!({
                    "type": "pgc",
                    "epid": 98765,
                    "cid": 12345
                }),
            )
            .await
            .expect("mock PGC playback should resolve");

        let dash = &result.playback_infos["dash"];
        assert_bilibili_cdn_headers(&dash.headers, "https://www.bilibili.com");
        assert_eq!(dash.subtitles.len(), 1);
        assert_bilibili_cdn_headers(&dash.subtitles[0].headers, "https://www.bilibili.com");
    }

    #[tokio::test]
    async fn test_pgc_direct_playback_falls_back_to_durl_when_dash_is_unavailable() {
        let provider =
            provider_with_test_bilibili_client(Arc::new(TestBilibiliClientWithPgcDurlFallback));
        let result = provider
            .generate_playback(
                &ProviderContext::new("test").with_user_id(UserId::expect_positive(1)),
                &json!({
                    "type": "pgc",
                    "epid": 98765,
                    "cid": 12345
                }),
            )
            .await
            .expect("PGC durl fallback playback should resolve");

        assert_eq!(result.default_mode, "mp4");
        let mp4 = &result.playback_infos["mp4"];
        assert_eq!(mp4.format, "mp4");
        assert_eq!(mp4.urls, vec!["https://upos.example/pgc.mp4"]);
        assert_bilibili_cdn_headers(&mp4.headers, "https://www.bilibili.com");
        assert_eq!(
            result.metadata.get("fallback_format"),
            Some(&json!("durl")),
            "fallback metadata should explain why PGC did not use DASH"
        );
    }

    #[tokio::test]
    async fn test_live_direct_playback_returns_live_headers() {
        let provider = provider_with_default_test_bilibili_client();
        let result = provider
            .generate_playback(
                &ProviderContext::new("test").with_user_id(UserId::expect_positive(1)),
                &json!({
                    "type": "live",
                    "room_id": 12345
                }),
            )
            .await
            .expect("mock live playback should resolve");

        let playback = &result.playback_infos[&result.default_mode];
        assert_bilibili_cdn_headers(&playback.headers, "https://live.bilibili.com");
    }

    #[test]
    fn test_valid_video_config_with_bvid() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_ok());
    }

    #[test]
    fn test_valid_video_config_with_aid() {
        let config = json!({
            "type": "video",
            "aid": 12345,
            "cid": 67890
        });
        assert!(validate_bilibili(&config).is_ok());
    }

    #[test]
    fn test_bilibili_config_with_provider_instance_name() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "provider_instance_name": "remote-bili-1"
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[tokio::test]
    async fn test_bilibili_shared_credential_dependency_uses_creator() {
        let provider = BilibiliProvider::new_local_only().expect("provider should build");
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_credential_owner_id(UserId::expect_positive(2));
        let dependencies = provider
            .credential_dependencies(
                &ctx,
                &json!({
                    "type": "video",
                    "bvid": "BV1xx411c7mD",
                    "cid": 12345,
                    "shared": true
                }),
            )
            .expect("Bilibili shared dependency extraction should succeed");

        assert_eq!(
            dependencies,
            vec![ProviderCredentialDependency::new(
                BilibiliProvider::NAME,
                "2",
                bilibili_credential_server_id()
            )]
        );
    }

    #[tokio::test]
    async fn test_bilibili_shared_credential_dependency_requires_explicit_creator() {
        let provider = BilibiliProvider::new_local_only().expect("provider should build");
        let ctx = ProviderContext::new("test").with_user_id(UserId::expect_positive(1));
        let err = provider
            .credential_dependencies(
                &ctx,
                &json!({
                    "type": "video",
                    "bvid": "BV1xx411c7mD",
                    "cid": 12345,
                    "shared": true
                }),
            )
            .expect_err("shared Bilibili media must not fall back to viewer credentials");

        assert!(
            err.to_string().contains("credential_owner_id"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_bilibili_non_shared_credential_dependency_uses_viewer() {
        let provider = BilibiliProvider::new_local_only().expect("provider should build");
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::expect_positive(1))
            .with_credential_owner_id(UserId::expect_positive(2));
        let dependencies = provider
            .credential_dependencies(
                &ctx,
                &json!({
                    "type": "video",
                    "bvid": "BV1xx411c7mD",
                    "cid": 12345
                }),
            )
            .expect("Bilibili non-shared dependency extraction should succeed");

        assert_eq!(
            dependencies,
            vec![ProviderCredentialDependency::new(
                BilibiliProvider::NAME,
                "1",
                bilibili_credential_server_id()
            )]
        );
    }

    #[tokio::test]
    async fn test_prepare_bilibili_config_rejects_provider_instance_name() {
        let provider = BilibiliProvider::new_local_only().expect("provider should build");
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "provider_instance_name": "remote-bili-1"
        });

        let result = provider
            .prepare_source_config(&ProviderContext::new("test"), config)
            .await;

        assert!(matches!(result, Err(ProviderError::InvalidConfig(_))));
    }

    #[test]
    fn test_video_config_missing_bvid_and_aid() {
        let config = json!({
            "type": "video",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_zero_cid() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 0
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_valid_pgc_config() {
        let config = json!({
            "type": "pgc",
            "epid": 12345,
            "cid": 67890
        });
        assert!(validate_bilibili(&config).is_ok());
    }

    #[test]
    fn test_pgc_config_zero_epid() {
        let config = json!({
            "type": "pgc",
            "epid": 0,
            "cid": 67890
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_valid_live_config() {
        let config = json!({
            "type": "live",
            "room_id": 12345
        });
        assert!(validate_bilibili(&config).is_ok());
    }

    #[test]
    fn test_live_config_zero_room_id() {
        let config = json!({
            "type": "live",
            "room_id": 0
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_invalid_type() {
        let config = json!({
            "type": "unknown_type",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_invalid_bvid_rejects_injection() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx/../../../etc/passwd",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_invalid_bvid_rejects_special_chars() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx;DROP TABLE",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_bvid_without_bv_prefix_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "1xx411c7mD12",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_bvid_with_lowercase_bv_prefix_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "bv1xx411c7mD",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_bvid_too_short_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7m",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_bvid_too_long_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mDxx",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_video_config_bvid_exactly_12_chars_accepted() {
        let config = json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_ok());
    }

    #[test]
    fn test_video_config_empty_bvid_uses_aid() {
        let config = json!({
            "type": "video",
            "bvid": "",
            "aid": 12345,
            "cid": 67890
        });
        assert!(validate_bilibili(&config).is_ok());
    }

    #[test]
    fn test_missing_credential_ref_allowed_for_anonymous_fallback() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345
        });
        assert!(validate_bilibili(&config).is_ok());
    }

    #[test]
    fn test_credential_ref_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1xx411c7mD",
            "cid": 12345,
            "credential_ref": {
                "credential_owner_id": "user456",
                "server_id": "bilibili"
            }
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_server_id_in_source_config_is_rejected() {
        let config = json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345,
            "server_id": "client-provided-value"
        });
        assert!(validate_bilibili(&config).is_err());
    }

    #[test]
    fn test_playback_cache_entry_isolated_by_credential_partition() {
        let config = BilibiliSourceConfig::try_from(&json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345
        }))
        .expect("config should parse");

        let (anon_key, anon_ttl) =
            playback_cache_entry(&config, "anon").expect("cache entry should build");
        let (auth_key, auth_ttl) = playback_cache_entry(&config, "auth:user-alpha:global-bilibili")
            .expect("cache entry should build");

        assert_eq!(anon_ttl, Duration::from_hours(2));
        assert_eq!(auth_ttl, Duration::from_hours(2));
        assert!(anon_key.contains("bvid:BV1GJ411x7gL"));
        assert_ne!(
            anon_key, auth_key,
            "Bilibili playback cache must not collide between anonymous and authenticated playback"
        );
    }

    #[test]
    fn test_playback_cache_entry_isolated_by_credential_owner() {
        let config = BilibiliSourceConfig::try_from(&json!({
            "type": "video",
            "bvid": "BV1GJ411x7gL",
            "cid": 12345
        }))
        .expect("config should parse");

        let (first_key, first_ttl) =
            playback_cache_entry(&config, "auth:user-alpha:global-bilibili")
                .expect("cache entry should build");
        let (second_key, second_ttl) =
            playback_cache_entry(&config, "auth:user-beta:global-bilibili")
                .expect("cache entry should build");

        assert_eq!(first_ttl, Duration::from_hours(2));
        assert_eq!(second_ttl, Duration::from_hours(2));
        assert_ne!(
            first_key, second_key,
            "Bilibili playback cache must not collide across distinct credential owners"
        );
    }

    #[test]
    fn test_pgc_and_live_cache_entries_are_isolated_by_credential_revision() {
        let pgc = BilibiliSourceConfig::try_from(&json!({
            "type": "pgc",
            "epid": 98765,
            "cid": 12345
        }))
        .expect("PGC config should parse");
        let live = BilibiliSourceConfig::try_from(&json!({
            "type": "live",
            "room_id": 76
        }))
        .expect("live config should parse");

        let (pgc_old_key, pgc_ttl) = playback_cache_entry(&pgc, "auth:7:bilibili:42:1000")
            .expect("cache entry should build");
        let (pgc_new_key, pgc_new_ttl) = playback_cache_entry(&pgc, "auth:7:bilibili:42:2000")
            .expect("cache entry should build");
        let (live_old_key, live_ttl) = playback_cache_entry(&live, "auth:7:bilibili:42:1000")
            .expect("cache entry should build");
        let (live_new_key, live_new_ttl) = playback_cache_entry(&live, "auth:7:bilibili:42:2000")
            .expect("cache entry should build");

        assert_eq!(pgc_ttl, Duration::from_hours(2));
        assert_eq!(pgc_new_ttl, Duration::from_hours(2));
        assert_eq!(live_ttl, Duration::from_mins(2));
        assert_eq!(live_new_ttl, Duration::from_mins(2));
        assert_ne!(
            pgc_old_key, pgc_new_key,
            "Bilibili PGC playback cache must move when credential revision changes"
        );
        assert_ne!(
            live_old_key, live_new_key,
            "Bilibili live playback cache must move when credential revision changes"
        );
    }
}
