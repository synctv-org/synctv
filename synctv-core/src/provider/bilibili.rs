//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    access::BilibiliAccess,
    provider_client::{create_remote_bilibili_client, BilibiliClientArc, ProviderClientManager},
    DynamicListQuery, DynamicListResult, DynamicPagination, DynamicPlaylistItem,
    DynamicPlaylistItemSourceConfig, DynamicPlaylistItemThumbnail, DynamicPlaylistProvider,
    ItemType, MediaProvider, NextPlayItem, PlaybackInfo, PlaybackProxyAutoPolicy,
    PlaybackProxyAutoReason, PlaybackProxyPolicy, PlaybackResult, PreparedSourceConfig,
    ProviderContext, ProviderCredentialDependency, ProviderCredentialPolicy, ProviderError,
    SourceConfig, SourceCover,
};
use aes_gcm::{
    aead::{Aead, AeadCore, Generate, KeyInit as AeadKeyInit},
    Aes256Gcm, Key, Nonce,
};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use futures::StreamExt;
use hmac::{Hmac, Mac};
use rand::prelude::IndexedRandom;
use serde::{Deserialize, Serialize};
use sha2::Digest;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::Duration;

use crate::models::media::{
    BilibiliDashAudioStream, BilibiliDashManifest, BilibiliDashManifestSlot,
    BilibiliDashSegmentBase, BilibiliDashVideoStream, BilibiliDurlSegment, BilibiliPlaybackKind,
    BilibiliPlaybackMetadata, PlaybackBilibiliDanmaku, PlaybackBilibiliMedia,
    PlaybackBilibiliSubtitle, PlaybackDanmaku, PlaybackDanmakuProvider, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::models::{
    normalize_provider_instance_name, validate_provider_instance_name, BilibiliHistoryType,
    BilibiliMediaSourceConfig as BilibiliSourceConfig, BilibiliPgcTimelineType,
    BilibiliPlaylistSource, BilibiliPlaylistSourceConfig, BilibiliTarget, MediaSourceConfig,
    PlayMode, Playlist, PlaylistId, PlaylistSourceConfig, ProviderCredential, ProviderTarget,
    RoomId, SourceProvider, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use crate::service::RemoteProviderManager;

use super::upstream_transport::bilibili as bilibili_upstream;

pub const LIVE_DANMAKU_FORMAT: &str = "synctv-bilibili-live";
pub const LIVE_DANMAKU_TRACK_NAME: &str = "Bilibili Live Danmaku";
const SMS_LOGIN_SESSION_TTL_SECONDS: i64 = 10 * 60;
const SMS_LOGIN_SESSION_VERSION: &str = "v2";
const SMS_LOGIN_DOMAIN_SEPARATOR: &[u8] = b"synctv-bilibili-sms-login";
const SMS_LOGIN_TOKEN_NONCE_SIZE: usize = 12;
const BILIBILI_PLAYBACK_CACHE_SCHEMA_VERSION: &str = "v9";
const BILIBILI_DASH_SWARM_SCHEMA_VERSION: &str = "v1";
const BILIBILI_DASH_RETRY_DELAY: Duration = Duration::from_millis(200);
type HmacSha256 = Hmac<sha2::Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BilibiliHistoryCursor {
    max: u64,
    view_at: i64,
    business: String,
}

#[derive(Debug, Clone, Copy)]
pub struct BilibiliHlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

#[derive(Debug, Clone, Copy)]
pub struct BilibiliDashResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub scope_url: &'a str,
    pub resource_path: &'a str,
    pub resource_query: Option<&'a str>,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

/// Bilibili `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct BilibiliProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    client_manager: Arc<ProviderClientManager>,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

/// Bilibili video info
#[derive(Debug, Clone, Serialize)]
pub struct BilibiliVideoInfo {
    pub bvid: String,
    pub aid: u64,
    pub cid: u64,
    pub epid: u64,
    pub page: u32,
    pub name: String,
    pub cover_image: String,
    pub r#live: bool,
    pub duration_seconds: u64,
    pub width: u64,
    pub height: u64,
}

/// Bilibili page info response
#[derive(Debug, Clone, Serialize)]
pub struct BilibiliPageInfo {
    pub title: String,
    pub actors: Vec<String>,
    pub videos: Vec<BilibiliVideoInfo>,
    pub season_id: u64,
    pub cover: String,
    pub collection: Option<BilibiliCollectionInfo>,
    pub live_started_at: Option<i64>,
    pub is_currently_live: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliCollectionInfo {
    pub mid: u64,
    pub season_id: u64,
    pub title: String,
    pub cover: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliLiveArea {
    pub id: u64,
    pub parent_id: u64,
    pub name: String,
    pub parent_name: String,
    pub picture: String,
    pub hot: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliFavoriteFolder {
    pub media_id: u64,
    pub title: String,
    pub media_count: u64,
    pub private: bool,
    pub default_folder: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliFollowedPgcSeason {
    pub season_id: u64,
    pub title: String,
    pub cover: String,
    pub description: String,
    pub latest_episode: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliFollowedPgcPage {
    pub items: Vec<BilibiliFollowedPgcSeason>,
    pub total: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliHistoryItem {
    pub title: String,
    pub subtitle: String,
    pub cover: String,
    pub author: String,
    pub viewed_at: i64,
    pub progress_seconds: i64,
    pub duration_seconds: u64,
    pub source_config: MediaSourceConfig,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliHistoryPage {
    pub items: Vec<BilibiliHistoryItem>,
    pub cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliPgcTimelineItem {
    pub episode_id: u64,
    pub season_id: u64,
    pub title: String,
    pub episode_title: String,
    pub cover: String,
    pub episode_cover: String,
    pub publish_at: i64,
    pub published: bool,
    pub date: String,
    pub day_of_week: u32,
    pub delayed: bool,
    pub delay_reason: String,
    pub source_config: Option<MediaSourceConfig>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliPgcSeasonIndexItem {
    pub season_id: u64,
    pub media_id: u64,
    pub first_episode_id: u64,
    pub title: String,
    pub subtitle: String,
    pub cover: String,
    pub first_episode_cover: String,
    pub badge: String,
    pub progress: String,
    pub score: String,
    pub finished: bool,
    pub season_type: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct BilibiliPgcSeasonIndexPage {
    pub items: Vec<BilibiliPgcSeasonIndexItem>,
    pub total: u64,
    pub has_more: bool,
}

#[derive(Debug, Clone)]
pub struct BilibiliMatchRequest {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliMatchResponse {
    pub normalized_url: String,
    pub resource: BilibiliMatchedResource,
}

#[derive(Debug, Clone)]
pub enum BilibiliMatchedResource {
    Video { bvid: String, aid: u64, page: u32 },
    PgcEpisode { episode_id: u64 },
    PgcSeason { season_id: u64 },
    Live { room_id: u64 },
    LiveRecommended,
    LiveArea { parent_area_id: u64, area_id: u64 },
    UpVideos { mid: u64 },
    FavoriteVideos { media_id: u64 },
    CollectionVideos { mid: u64, season_id: u64 },
    SeriesVideos { mid: u64, series_id: u64 },
    WatchLater,
}

#[derive(Debug, Clone)]
pub struct BilibiliParseVideoPageRequest {
    pub cookies: HashMap<String, String>,
    pub aid: u64,
    pub bvid: String,
    pub sections: bool,
}

#[derive(Debug, Clone)]
pub struct BilibiliParsePgcPageRequest {
    pub cookies: HashMap<String, String>,
    pub ssid: u64,
    pub epid: u64,
}

#[derive(Debug, Clone)]
pub struct BilibiliParseLivePageRequest {
    pub cookies: HashMap<String, String>,
    pub room_id: u64,
}

#[derive(Debug, Clone)]
pub struct BilibiliQrCodeResponse {
    pub url: String,
    pub key: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliQrLoginRequest {
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BilibiliQrLoginStatus {
    Unknown,
    Expired,
    NotScanned,
    Scanned,
    Success,
}

#[derive(Debug, Clone)]
pub struct BilibiliQrLoginResponse {
    pub status: BilibiliQrLoginStatus,
    pub cookies: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct BilibiliPersistedQrLoginResponse {
    pub status: BilibiliQrLoginStatus,
    pub server_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BilibiliBind {
    pub id: i64,
    pub server_id: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BilibiliCaptchaResponse {
    pub token: String,
    pub gt: String,
    pub challenge: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliSmsRequest {
    pub phone: String,
    pub token: String,
    pub challenge: String,
    pub validate: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliSmsResponse {
    pub captcha_key: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliSmsLoginRequest {
    pub phone: String,
    pub code: String,
    pub captcha_key: String,
}

#[derive(Debug, Clone)]
pub struct BilibiliSmsLoginResponse {
    pub cookies: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct BilibiliSmsLoginStart {
    pub session_token: String,
    pub gt: String,
    pub challenge: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct BilibiliSmsSessionUpdate {
    pub session_token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct BilibiliSmsSessionLoginResponse {
    pub server_id: String,
    pub provider_instance_name: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
struct BilibiliSmsLoginSession {
    token: String,
    challenge: String,
    phone: Option<String>,
    captcha_key: Option<String>,
    instance_name: Option<String>,
    expires_at: i64,
}

pub struct BilibiliSmsLoginTokenCodec {
    cipher: Aes256Gcm,
}

impl BilibiliSmsLoginTokenCodec {
    pub fn derive_from(secret: &[u8]) -> Result<Self, ProviderError> {
        let mut derivation_mac = HmacSha256::new_from_slice(secret).map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to derive Bilibili SMS login token key: {error}"
            ))
        })?;
        derivation_mac.update(SMS_LOGIN_DOMAIN_SEPARATOR);
        let derived = derivation_mac.finalize().into_bytes();
        let key = Key::<Aes256Gcm>::try_from(derived.as_slice()).map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to derive Bilibili SMS login token key: {error}"
            ))
        })?;
        Ok(Self {
            cipher: Aes256Gcm::new(&key),
        })
    }

    fn encode(&self, session: &BilibiliSmsLoginSession) -> Result<String, ProviderError> {
        let payload = serde_json::to_vec(session).map_err(|error| {
            ProviderError::Internal(format!(
                "Failed to serialize Bilibili SMS login session: {error}"
            ))
        })?;
        let nonce = Nonce::<<Aes256Gcm as AeadCore>::NonceSize>::generate();
        let ciphertext = self.cipher.encrypt(&nonce, payload.as_ref()).map_err(|_| {
            ProviderError::Internal("Failed to encrypt Bilibili SMS login session".to_string())
        })?;
        let mut token = Vec::with_capacity(SMS_LOGIN_TOKEN_NONCE_SIZE + ciphertext.len());
        token.extend_from_slice(&nonce);
        token.extend_from_slice(&ciphertext);

        Ok(format!(
            "{SMS_LOGIN_SESSION_VERSION}.{}",
            URL_SAFE_NO_PAD.encode(token)
        ))
    }

    fn decode(&self, session_token: &str) -> Result<BilibiliSmsLoginSession, ProviderError> {
        let invalid = || {
            ProviderError::Authentication(
                "Bilibili SMS login session is invalid or expired".to_string(),
            )
        };
        let mut parts = session_token.split('.');
        let version = parts.next().ok_or_else(invalid)?;
        let token = parts.next().ok_or_else(invalid)?;
        if version != SMS_LOGIN_SESSION_VERSION || parts.next().is_some() {
            return Err(invalid());
        }

        let token = URL_SAFE_NO_PAD.decode(token).map_err(|_| invalid())?;
        if token.len() <= SMS_LOGIN_TOKEN_NONCE_SIZE {
            return Err(invalid());
        }
        let (nonce_bytes, ciphertext) = token.split_at(SMS_LOGIN_TOKEN_NONCE_SIZE);
        let nonce = Nonce::<<Aes256Gcm as AeadCore>::NonceSize>::try_from(nonce_bytes)
            .map_err(|_| invalid())?;
        let payload = self
            .cipher
            .decrypt(&nonce, ciphertext)
            .map_err(|_| invalid())?;
        let session: BilibiliSmsLoginSession =
            serde_json::from_slice(&payload).map_err(|_| invalid())?;
        if crate::SystemClock.now().timestamp() >= session.expires_at {
            return Err(invalid());
        }
        Ok(session)
    }
}

#[derive(Debug, Clone)]
pub struct BilibiliUserInfoRequest {
    pub cookies: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct BilibiliUserInfoResponse {
    pub is_login: bool,
    pub user_id: u64,
    pub username: String,
    pub face: String,
    pub is_vip: bool,
}

fn bilibili_match_request(req: BilibiliMatchRequest) -> bilibili_upstream::MatchReq {
    bilibili_upstream::MatchReq { url: req.url }
}

fn bilibili_parse_video_page_request(
    req: BilibiliParseVideoPageRequest,
) -> bilibili_upstream::ParseVideoPageReq {
    bilibili_upstream::ParseVideoPageReq {
        cookies: req.cookies,
        aid: req.aid,
        bvid: req.bvid,
        sections: req.sections,
    }
}

fn bilibili_parse_pgc_page_request(
    req: BilibiliParsePgcPageRequest,
) -> bilibili_upstream::ParsePgcPageReq {
    bilibili_upstream::ParsePgcPageReq {
        cookies: req.cookies,
        ssid: req.ssid,
        epid: req.epid,
    }
}

fn bilibili_parse_live_page_request(
    req: BilibiliParseLivePageRequest,
) -> bilibili_upstream::ParseLivePageReq {
    bilibili_upstream::ParseLivePageReq {
        cookies: req.cookies,
        room_id: req.room_id,
    }
}

fn bilibili_empty_request() -> bilibili_upstream::Empty {
    bilibili_upstream::Empty {}
}

fn bilibili_qr_login_request(req: BilibiliQrLoginRequest) -> bilibili_upstream::LoginWithQrCodeReq {
    bilibili_upstream::LoginWithQrCodeReq { key: req.key }
}

fn bilibili_sms_request(req: BilibiliSmsRequest) -> bilibili_upstream::NewSmsReq {
    bilibili_upstream::NewSmsReq {
        phone: req.phone,
        token: req.token,
        challenge: req.challenge,
        validate: req.validate,
    }
}

fn bilibili_sms_login_request(req: BilibiliSmsLoginRequest) -> bilibili_upstream::LoginWithSmsReq {
    bilibili_upstream::LoginWithSmsReq {
        phone: req.phone,
        code: req.code,
        captcha_key: req.captcha_key,
    }
}

fn bilibili_user_info_request(req: BilibiliUserInfoRequest) -> bilibili_upstream::UserInfoReq {
    bilibili_upstream::UserInfoReq {
        cookies: req.cookies,
    }
}

impl BilibiliProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "bilibili";

    #[must_use]
    pub fn credential_server_id() -> String {
        hex::encode(sha2::Sha256::digest(Self::NAME.as_bytes()))
    }

    pub fn sms_login_token_codec_from_secret(
        secret: &[u8],
    ) -> Result<BilibiliSmsLoginTokenCodec, ProviderError> {
        BilibiliSmsLoginTokenCodec::derive_from(secret)
    }

    fn provider_instance_name_for_provider(
        value: Option<&str>,
    ) -> Result<Option<&str>, ProviderError> {
        let Some(value) = normalize_provider_instance_name(value) else {
            return Ok(None);
        };
        validate_provider_instance_name(value).map_err(ProviderError::InvalidConfig)?;
        Ok(Some(value))
    }

    pub fn qr_login_status_cache_key(
        instance_name: Option<&str>,
        key: &str,
    ) -> Result<String, ProviderError> {
        let instance_name = Self::provider_instance_name_for_provider(instance_name)?.unwrap_or("");
        Ok(format!("{instance_name}:{key}"))
    }

    pub fn ensure_login_cookies_present(
        cookies: &HashMap<String, String>,
        method: &str,
    ) -> Result<(), ProviderError> {
        if cookies.is_empty() {
            return Err(ProviderError::Authentication(format!(
                "Bilibili {method} login did not return session cookies"
            )));
        }
        Ok(())
    }

    fn sanitize_sms_validate(validate: &str) -> Result<String, ProviderError> {
        let validate = validate.trim().to_string();
        if validate.is_empty() {
            return Err(ProviderError::Authentication(
                "Bilibili SMS verification result is empty".to_string(),
            ));
        }
        Ok(validate)
    }

    fn verify_sms_instance_name(
        requested: Option<&str>,
        session: &BilibiliSmsLoginSession,
    ) -> Result<(), ProviderError> {
        if requested.is_none() {
            return Ok(());
        }
        let expected = Self::provider_instance_name_for_provider(session.instance_name.as_deref())?;
        let requested = Self::provider_instance_name_for_provider(requested)?;
        if expected != requested {
            return Err(ProviderError::Authentication(
                "Bilibili SMS login session does not match the requested provider instance"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn require_sms_phone(session: &BilibiliSmsLoginSession) -> Result<String, ProviderError> {
        session.phone.clone().ok_or_else(|| {
            ProviderError::Authentication(
                "Request a Bilibili SMS code before logging in".to_string(),
            )
        })
    }

    fn require_sms_captcha_key(session: &BilibiliSmsLoginSession) -> Result<String, ProviderError> {
        session.captcha_key.clone().ok_or_else(|| {
            ProviderError::Authentication(
                "Request a Bilibili SMS code before logging in".to_string(),
            )
        })
    }

    /// Create a new `BilibiliProvider` with `RemoteProviderManager`
    pub fn new(
        provider_instance_manager: Arc<RemoteProviderManager>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager,
            client_manager: Arc::new(ProviderClientManager::new()?),
            credential_repo: None,
        })
    }

    #[must_use]
    pub fn with_client_manager(
        provider_instance_manager: Arc<RemoteProviderManager>,
        client_manager: Arc<ProviderClientManager>,
    ) -> Self {
        Self {
            provider_instance_manager,
            client_manager,
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            provider_instance_manager: self.provider_instance_manager.clone(),
            client_manager: self.client_manager.clone(),
            credential_repo: Some(credential_repo),
        }
    }

    fn credential_repo(&self) -> Result<&UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().ok_or_else(|| {
            ProviderError::Internal("Bilibili credential repository is not configured".to_string())
        })
    }

    #[cfg(test)]
    pub fn new_local_only() -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager:
                crate::service::remote_provider_manager::empty_provider_instance_manager(),
            client_manager: Arc::new(ProviderClientManager::new()?),
            credential_repo: None,
        })
    }

    async fn get_client_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliClientArc, ProviderError> {
        match instance_name {
            None => Ok(self.client_manager.local_bilibili_client()),
            Some(_) => {
                self.provider_instance_manager
                    .resolve_client_required_with_context(
                        instance_name,
                        request_context,
                        create_remote_bilibili_client,
                        || self.client_manager.local_bilibili_client(),
                    )
                    .await
            }
        }
    }

    /// Match URL to determine type and ID
    pub async fn r#match_with_context(
        &self,
        req: BilibiliMatchRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliMatchResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .r#match(bilibili_match_request(req))
            .await
            .map_err(ProviderError::from)?;
        let resource = match resp.resource.ok_or_else(|| {
            ProviderError::ParseError("Bilibili match response is missing resource".to_string())
        })? {
            bilibili_upstream::match_resp::Resource::Video(resource) => {
                BilibiliMatchedResource::Video {
                    bvid: resource.bvid,
                    aid: resource.aid,
                    page: resource.page,
                }
            }
            bilibili_upstream::match_resp::Resource::PgcEpisode(resource) => {
                BilibiliMatchedResource::PgcEpisode {
                    episode_id: resource.episode_id,
                }
            }
            bilibili_upstream::match_resp::Resource::PgcSeason(resource) => {
                BilibiliMatchedResource::PgcSeason {
                    season_id: resource.season_id,
                }
            }
            bilibili_upstream::match_resp::Resource::Live(resource) => {
                BilibiliMatchedResource::Live {
                    room_id: resource.room_id,
                }
            }
            bilibili_upstream::match_resp::Resource::LiveRecommended(_) => {
                BilibiliMatchedResource::LiveRecommended
            }
            bilibili_upstream::match_resp::Resource::LiveArea(resource) => {
                BilibiliMatchedResource::LiveArea {
                    parent_area_id: resource.parent_area_id,
                    area_id: resource.area_id,
                }
            }
            bilibili_upstream::match_resp::Resource::UpVideos(resource) => {
                BilibiliMatchedResource::UpVideos { mid: resource.mid }
            }
            bilibili_upstream::match_resp::Resource::FavoriteVideos(resource) => {
                BilibiliMatchedResource::FavoriteVideos {
                    media_id: resource.media_id,
                }
            }
            bilibili_upstream::match_resp::Resource::CollectionVideos(resource) => {
                BilibiliMatchedResource::CollectionVideos {
                    mid: resource.mid,
                    season_id: resource.season_id,
                }
            }
            bilibili_upstream::match_resp::Resource::SeriesVideos(resource) => {
                BilibiliMatchedResource::SeriesVideos {
                    mid: resource.mid,
                    series_id: resource.series_id,
                }
            }
            bilibili_upstream::match_resp::Resource::WatchLater(_) => {
                BilibiliMatchedResource::WatchLater
            }
        };
        Ok(BilibiliMatchResponse {
            normalized_url: resp.normalized_url,
            resource,
        })
    }

    /// Parse video page
    pub async fn parse_video_page_with_context(
        &self,
        req: BilibiliParseVideoPageRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .parse_video_page(bilibili_parse_video_page_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(Self::page_info_from_provider(resp))
    }

    /// Parse PGC page
    pub async fn parse_pgc_page_with_context(
        &self,
        req: BilibiliParsePgcPageRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .parse_pgc_page(bilibili_parse_pgc_page_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(Self::page_info_from_provider(resp))
    }

    /// Parse live page
    pub async fn parse_live_page_with_context(
        &self,
        req: BilibiliParseLivePageRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliPageInfo, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .parse_live_page(bilibili_parse_live_page_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(Self::page_info_from_provider(resp))
    }

    /// List the live-area hierarchy used by live-area dynamic playlists.
    pub async fn list_live_areas_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<Vec<BilibiliLiveArea>, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let response = client
            .list_live_areas(bilibili_upstream::ListLiveAreasReq {})
            .await
            .map_err(ProviderError::from)?;
        Ok(response
            .items
            .into_iter()
            .map(|area| BilibiliLiveArea {
                id: area.id,
                parent_id: area.parent_id,
                name: area.name,
                parent_name: area.parent_name,
                picture: area.picture,
                hot: area.hot,
            })
            .collect())
    }

    pub async fn list_favorite_folders_with_context(
        &self,
        cookies: HashMap<String, String>,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<Vec<BilibiliFavoriteFolder>, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let response = client
            .list_favorite_folders(bilibili_upstream::ListFavoriteFoldersReq { cookies })
            .await
            .map_err(ProviderError::from)?;
        Ok(response
            .items
            .into_iter()
            .map(|folder| BilibiliFavoriteFolder {
                media_id: folder.media_id,
                title: folder.title,
                media_count: folder.media_count,
                private: folder.private,
                default_folder: folder.default_folder,
            })
            .collect())
    }

    pub async fn list_followed_pgc_with_context(
        &self,
        cookies: HashMap<String, String>,
        season_type: u32,
        page: u64,
        page_size: u32,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliFollowedPgcPage, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let response = client
            .list_followed_pgc(bilibili_upstream::ListFollowedPgcReq {
                cookies,
                season_type,
                page,
                page_size,
            })
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliFollowedPgcPage {
            items: response
                .items
                .into_iter()
                .map(|season| BilibiliFollowedPgcSeason {
                    season_id: season.season_id,
                    title: season.title,
                    cover: season.cover,
                    description: season.description,
                    latest_episode: season.latest_episode,
                })
                .collect(),
            total: response.total,
            has_more: response.has_more,
        })
    }

    pub async fn list_history_with_context(
        &self,
        cookies: HashMap<String, String>,
        history_type: BilibiliHistoryType,
        cursor: Option<&str>,
        page_size: u32,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliHistoryPage, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let response = client
            .list_history(bilibili_upstream::ListHistoryReq {
                cookies,
                r#type: match history_type {
                    BilibiliHistoryType::All => bilibili_upstream::HistoryType::All as i32,
                    BilibiliHistoryType::Archive => bilibili_upstream::HistoryType::Archive as i32,
                    BilibiliHistoryType::Live => bilibili_upstream::HistoryType::Live as i32,
                },
                cursor: Self::decode_history_cursor(cursor)?,
                page_size: page_size.clamp(1, 30),
            })
            .await
            .map_err(ProviderError::from)?;
        let items = response
            .items
            .into_iter()
            .map(|item| {
                let target = Self::history_target(&item)?;
                let ProviderTarget::Bilibili(target) = target else {
                    unreachable!();
                };
                Ok(BilibiliHistoryItem {
                    title: item.title,
                    subtitle: item.subtitle,
                    cover: item.cover,
                    author: item.author,
                    viewed_at: item.viewed_at,
                    progress_seconds: item.progress_seconds,
                    duration_seconds: item.duration_seconds,
                    source_config: Self::media_config_for_target(
                        &BilibiliPlaylistSourceConfig {
                            source: BilibiliPlaylistSource::History { history_type },
                            shared: false,
                            proxy_mode: crate::models::PlaybackProxyMode::Auto,
                        },
                        &target,
                    )?,
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        Ok(BilibiliHistoryPage {
            items,
            cursor: Self::encode_history_cursor(response.cursor)?,
            has_more: response.has_more,
        })
    }

    pub async fn list_pgc_timeline_with_context(
        &self,
        cookies: HashMap<String, String>,
        timeline_type: BilibiliPgcTimelineType,
        before_days: u32,
        after_days: u32,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<Vec<BilibiliPgcTimelineItem>, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let response = client
            .list_pgc_timeline(bilibili_upstream::ListPgcTimelineReq {
                cookies,
                r#type: match timeline_type {
                    BilibiliPgcTimelineType::Anime => {
                        bilibili_upstream::PgcTimelineType::Anime as i32
                    }
                    BilibiliPgcTimelineType::Cinema => {
                        bilibili_upstream::PgcTimelineType::Cinema as i32
                    }
                    BilibiliPgcTimelineType::Guochuang => {
                        bilibili_upstream::PgcTimelineType::Guochuang as i32
                    }
                },
                before_days,
                after_days,
            })
            .await
            .map_err(ProviderError::from)?;
        Ok(response
            .items
            .into_iter()
            .map(|item| BilibiliPgcTimelineItem {
                source_config: (item.published && item.cid > 0).then_some({
                    MediaSourceConfig::Bilibili(BilibiliSourceConfig::Pgc(
                        crate::models::BilibiliPgcSourceConfig {
                            epid: item.episode_id,
                            cid: item.cid,
                            shared: false,
                            proxy_mode: crate::models::PlaybackProxyMode::Auto,
                        },
                    ))
                }),
                episode_id: item.episode_id,
                season_id: item.season_id,
                title: item.title,
                episode_title: item.episode_title,
                cover: item.cover,
                episode_cover: item.episode_cover,
                publish_at: item.publish_at,
                published: item.published,
                date: item.date,
                day_of_week: item.day_of_week,
                delayed: item.delayed,
                delay_reason: item.delay_reason,
            })
            .collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_pgc_seasons_with_context(
        &self,
        cookies: HashMap<String, String>,
        season_type: u32,
        page: u64,
        page_size: u32,
        order: u32,
        ascending: bool,
        finished: Option<bool>,
        area: Option<String>,
        year: Option<String>,
        style_id: Option<u64>,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliPgcSeasonIndexPage, ProviderError> {
        let season_type = i32::try_from(season_type).map_err(|_| {
            ProviderError::InvalidConfig("Bilibili season type exceeds i32".to_string())
        })?;
        let order = i32::try_from(order).map_err(|_| {
            ProviderError::InvalidConfig("Bilibili season order exceeds i32".to_string())
        })?;
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let response = client
            .list_pgc_seasons(bilibili_upstream::ListPgcSeasonsReq {
                cookies,
                r#type: season_type,
                page,
                page_size,
                order,
                ascending,
                finished,
                area,
                year,
                style_id,
            })
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliPgcSeasonIndexPage {
            items: response
                .items
                .into_iter()
                .map(|item| {
                    Ok(BilibiliPgcSeasonIndexItem {
                        season_id: item.season_id,
                        media_id: item.media_id,
                        first_episode_id: item.first_episode_id,
                        title: item.title,
                        subtitle: item.subtitle,
                        cover: item.cover,
                        first_episode_cover: item.first_episode_cover,
                        badge: item.badge,
                        progress: item.progress,
                        score: item.score,
                        finished: item.finished,
                        season_type: u32::try_from(item.r#type).map_err(|_| {
                            ProviderError::ApiError(
                                "Bilibili returned a negative season type".to_string(),
                            )
                        })?,
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?,
            total: response.total,
            has_more: response.has_more,
        })
    }

    /// Generate QR code for login
    pub async fn new_qr_code_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliQrCodeResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .new_qr_code(bilibili_empty_request())
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliQrCodeResponse {
            url: resp.url,
            key: resp.key,
        })
    }

    /// Check QR code login status
    pub async fn login_with_qr_code_with_context(
        &self,
        req: BilibiliQrLoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliQrLoginResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .login_with_qr_code(bilibili_qr_login_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliQrLoginResponse {
            status: Self::qr_login_status_from_provider(resp.status),
            cookies: resp.cookies,
        })
    }

    pub async fn check_qr_and_persist_with_context(
        &self,
        user_id: UserId,
        key: String,
        provider_instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliPersistedQrLoginResponse, ProviderError> {
        let resp = self
            .login_with_qr_code_with_context(
                BilibiliQrLoginRequest { key },
                provider_instance_name,
                request_context,
            )
            .await?;
        let server_id = if resp.status == BilibiliQrLoginStatus::Success {
            Self::ensure_login_cookies_present(&resp.cookies, "QR")?;
            Some(
                self.persist_cookies_credential(user_id, resp.cookies, provider_instance_name)
                    .await?,
            )
        } else {
            None
        };

        Ok(BilibiliPersistedQrLoginResponse {
            status: resp.status,
            server_id,
        })
    }

    /// Get new captcha
    pub async fn new_captcha_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliCaptchaResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .new_captcha(bilibili_empty_request())
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliCaptchaResponse {
            token: resp.token,
            gt: resp.gt,
            challenge: resp.challenge,
        })
    }

    pub async fn start_sms_login_session_with_context(
        &self,
        codec: &BilibiliSmsLoginTokenCodec,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliSmsLoginStart, ProviderError> {
        let resp = self
            .new_captcha_with_context(instance_name, request_context)
            .await?;

        let now = crate::SystemClock.now().timestamp();
        let expires_at = now + SMS_LOGIN_SESSION_TTL_SECONDS;
        let session = BilibiliSmsLoginSession {
            token: resp.token,
            challenge: resp.challenge.clone(),
            phone: None,
            captcha_key: None,
            instance_name: instance_name.map(ToString::to_string),
            expires_at,
        };

        Ok(BilibiliSmsLoginStart {
            session_token: codec.encode(&session)?,
            gt: resp.gt,
            challenge: resp.challenge,
            expires_at,
        })
    }

    /// Send SMS verification code
    pub async fn new_sms_with_context(
        &self,
        req: BilibiliSmsRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliSmsResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .new_sms(bilibili_sms_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliSmsResponse {
            captcha_key: resp.captcha_key,
        })
    }

    pub async fn send_sms_with_session_context(
        &self,
        codec: &BilibiliSmsLoginTokenCodec,
        session_token: &str,
        phone: String,
        validate: &str,
        requested_instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliSmsSessionUpdate, ProviderError> {
        let mut session = codec.decode(session_token)?;
        Self::verify_sms_instance_name(requested_instance_name, &session)?;
        let sms_req = BilibiliSmsRequest {
            phone: phone.clone(),
            token: session.token.clone(),
            challenge: session.challenge.clone(),
            validate: Self::sanitize_sms_validate(validate)?,
        };

        let resp = self
            .new_sms_with_context(sms_req, session.instance_name.as_deref(), request_context)
            .await?;

        session.phone = Some(phone);
        session.captcha_key = Some(resp.captcha_key);

        Ok(BilibiliSmsSessionUpdate {
            session_token: codec.encode(&session)?,
            expires_at: session.expires_at,
        })
    }

    /// Login with SMS code
    pub async fn login_with_sms_with_context(
        &self,
        req: BilibiliSmsLoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliSmsLoginResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .login_with_sms(bilibili_sms_login_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliSmsLoginResponse {
            cookies: resp.cookies,
        })
    }

    pub async fn login_with_sms_session_context(
        &self,
        user_id: UserId,
        codec: &BilibiliSmsLoginTokenCodec,
        session_token: &str,
        code: String,
        requested_instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliSmsSessionLoginResponse, ProviderError> {
        let session = codec.decode(session_token)?;
        Self::verify_sms_instance_name(requested_instance_name, &session)?;
        let login_req = BilibiliSmsLoginRequest {
            phone: Self::require_sms_phone(&session)?,
            code,
            captcha_key: Self::require_sms_captcha_key(&session)?,
        };

        let resp = self
            .login_with_sms_with_context(
                login_req,
                session.instance_name.as_deref(),
                request_context,
            )
            .await?;

        Self::ensure_login_cookies_present(&resp.cookies, "SMS")?;
        let provider_instance_name = session.instance_name;
        let server_id = self
            .persist_cookies_credential(user_id, resp.cookies, provider_instance_name.as_deref())
            .await?;
        Ok(BilibiliSmsSessionLoginResponse {
            server_id,
            provider_instance_name,
        })
    }

    pub async fn persist_cookies_credential(
        &self,
        user_id: UserId,
        cookies: HashMap<String, String>,
        provider_instance_name: Option<&str>,
    ) -> Result<String, ProviderError> {
        let server_id = Self::credential_server_id();
        let credential_data = ProviderCredential::Bilibili { cookies };
        let now = crate::SystemClock.now();
        let credential = UserProviderCredential {
            id: 0,
            user_id,
            provider: Self::NAME.to_string(),
            server_id: server_id.clone(),
            provider_instance_name: provider_instance_name.map(ToString::to_string),
            credential_data,
            expires_at: None,
            created_at: now,
            updated_at: now,
        };

        self.credential_repo()?
            .upsert_by_user_provider_server(&credential)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to persist bilibili credential: {error}"))
            })?;

        Ok(server_id)
    }

    pub async fn delete_credential(&self, user_id: UserId) -> Result<bool, ProviderError> {
        let server_id = Self::credential_server_id();
        let Some(existing) = self
            .credential_repo()?
            .get_by_provider_and_server(user_id, Self::NAME, &server_id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to query bilibili credential: {error}"))
            })?
        else {
            return Ok(false);
        };

        self.credential_repo()?
            .delete(existing.id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to delete bilibili credential: {error}"))
            })?;
        Ok(true)
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<BilibiliBind>, ProviderError> {
        let requested_instance_name =
            Self::provider_instance_name_for_provider(provider_instance_name)?;
        let server_id = Self::credential_server_id();
        let credentials = self
            .credential_repo()?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to query bilibili credentials: {error}"))
            })?;

        Ok(credentials
            .into_iter()
            .filter(|credential| credential.server_id == server_id)
            .filter(|credential| {
                requested_instance_name.is_none_or(|requested| {
                    normalize_provider_instance_name(credential.provider_instance_name.as_deref())
                        == Some(requested)
                })
            })
            .map(|credential| BilibiliBind {
                id: credential.id,
                server_id: credential.server_id,
                created_at: credential.created_at.timestamp(),
                provider_instance_name: credential.provider_instance_name,
            })
            .collect())
    }

    pub fn anonymous_access() -> BilibiliAccess {
        BilibiliAccess::anonymous("anonymous", None)
    }

    pub fn access_from_stored_credential(
        user_id: UserId,
        server_id: &str,
        credential: ProviderCredential,
        credential_revision: &str,
        provider_instance_name: Option<String>,
    ) -> Result<BilibiliAccess, ProviderError> {
        match credential {
            ProviderCredential::Bilibili { cookies } => Ok(BilibiliAccess::authenticated(
                cookies,
                format!("auth:{user_id}:{server_id}:{credential_revision}"),
                provider_instance_name,
            )),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    /// Get user info
    pub async fn user_info_with_context(
        &self,
        req: BilibiliUserInfoRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliUserInfoResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .user_info(bilibili_user_info_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliUserInfoResponse {
            is_login: resp.is_login,
            user_id: resp.user_id,
            username: resp.username,
            face: resp.face,
            is_vip: resp.is_vip,
        })
    }

    fn page_info_from_provider(page_info: bilibili_upstream::VideoPageInfo) -> BilibiliPageInfo {
        let actors = if page_info.actors.is_empty() {
            Vec::new()
        } else {
            page_info
                .actors
                .split(',')
                .map(|actor| actor.trim().to_string())
                .collect()
        };

        BilibiliPageInfo {
            title: page_info.title,
            actors,
            videos: page_info
                .video_infos
                .into_iter()
                .map(|video| BilibiliVideoInfo {
                    bvid: video.bvid,
                    aid: video.aid,
                    cid: video.cid,
                    epid: video.epid,
                    page: video.page,
                    name: video.name,
                    cover_image: video.cover_image,
                    r#live: video.live,
                    duration_seconds: video.duration_seconds,
                    width: video.width,
                    height: video.height,
                })
                .collect(),
            season_id: page_info.season_id,
            cover: page_info.cover,
            live_started_at: page_info.live_started_at,
            is_currently_live: page_info.is_currently_live,
            collection: page_info
                .collection
                .map(|collection| BilibiliCollectionInfo {
                    mid: collection.mid,
                    season_id: collection.season_id,
                    title: collection.title,
                    cover: collection.cover,
                }),
        }
    }

    const fn qr_login_status_from_provider(status: i32) -> BilibiliQrLoginStatus {
        match status {
            1 => BilibiliQrLoginStatus::Expired,
            2 => BilibiliQrLoginStatus::NotScanned,
            3 => BilibiliQrLoginStatus::Scanned,
            4 => BilibiliQrLoginStatus::Success,
            _ => BilibiliQrLoginStatus::Unknown,
        }
    }

    async fn resolve_source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliSourceConfig,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let credential_user_id = bilibili_optional_credential_user_id(
            ctx,
            ProviderCredentialPolicy::from_shared(config.shared()),
        )?;
        let (cookies, credential_cache_partition) =
            resolve_optional_bilibili_cookies(ctx, credential_user_id).await?;

        let instance_name = super::bound_provider_instance_name(ctx);
        let request_context = ctx.request_context();
        let provider_instance_key = instance_name.unwrap_or_default();
        let (cache_key, cache_ttl) = match config {
            BilibiliSourceConfig::Video(config) => {
                let identifier =
                    BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?;
                (
                    format!(
                        "source-cover:video:{}:cid:{}:{credential_cache_partition}:{provider_instance_key}",
                        identifier.cache_key_part(),
                        config.cid
                    ),
                    Duration::from_hours(2),
                )
            }
            BilibiliSourceConfig::Pgc(config) => (
                format!(
                    "source-cover:pgc:{}:cid:{}:{credential_cache_partition}:{provider_instance_key}",
                    config.epid, config.cid
                ),
                Duration::from_hours(2),
            ),
            BilibiliSourceConfig::Live(config) => (
                format!(
                    "source-cover:live:{}:{credential_cache_partition}:{provider_instance_key}",
                    config.room_id
                ),
                Duration::from_mins(2),
            ),
        };

        super::cached_source_cover_or_fill(Self::NAME, &cache_key, cache_ttl, ctx, || async {
            let page = match config {
                BilibiliSourceConfig::Video(config) => {
                    let identifier =
                        BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?;
                    self.parse_video_page_with_context(
                        BilibiliParseVideoPageRequest {
                            cookies,
                            aid: identifier.aid,
                            bvid: identifier.bvid.unwrap_or_default(),
                            sections: true,
                        },
                        instance_name,
                        request_context,
                    )
                    .await?
                }
                BilibiliSourceConfig::Pgc(config) => {
                    self.parse_pgc_page_with_context(
                        BilibiliParsePgcPageRequest {
                            cookies,
                            ssid: 0,
                            epid: config.epid,
                        },
                        instance_name,
                        request_context,
                    )
                    .await?
                }
                BilibiliSourceConfig::Live(config) => {
                    self.parse_live_page_with_context(
                        BilibiliParseLivePageRequest {
                            cookies,
                            room_id: config.room_id,
                        },
                        instance_name,
                        request_context,
                    )
                    .await?
                }
            };

            let selected = match config {
                BilibiliSourceConfig::Video(config) => page
                    .videos
                    .iter()
                    .find(|video| video.cid == config.cid)
                    .or_else(|| page.videos.first()),
                BilibiliSourceConfig::Pgc(config) => page
                    .videos
                    .iter()
                    .find(|video| video.cid == config.cid || video.epid == config.epid)
                    .or_else(|| page.videos.first()),
                BilibiliSourceConfig::Live(_) => page.videos.first(),
            };

            Ok(selected.and_then(|video| {
                let cover = video.cover_image.trim();
                (!cover.is_empty()).then(|| SourceCover::Url {
                    url: cover.to_string(),
                })
            }))
        })
        .await
    }
}

impl BilibiliSourceConfig {
    const fn shared(&self) -> bool {
        match self {
            Self::Video(config) => config.shared,
            Self::Pgc(config) => config.shared,
            Self::Live(config) => config.shared,
        }
    }

    fn from_media_config(config: &MediaSourceConfig) -> Result<&Self, ProviderError> {
        match config {
            MediaSourceConfig::Bilibili(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Bilibili requires Bilibili media source_config".to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BilibiliVideoIdentifier {
    bvid: Option<String>,
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
        Ok(Self { bvid, aid })
    }

    fn cache_key_part(&self) -> String {
        match (self.bvid.as_deref(), self.aid) {
            (Some(bvid), 0) => format!("bvid:{bvid}"),
            (None, aid) => format!("aid:{aid}"),
            (Some(bvid), aid) => format!("bvid:{bvid}:aid:{aid}"),
        }
    }
}

fn resolve_bilibili_video_identifier(
    bvid: Option<&str>,
    aid: Option<u64>,
) -> Result<(Option<String>, u64), ProviderError> {
    let identifier = BilibiliVideoIdentifier::parse(bvid, aid)?;
    Ok((identifier.bvid, identifier.aid))
}

fn bilibili_dash_resource_candidates(
    dash: &BilibiliDashManifest,
    scope_url: &str,
    resource_path: &str,
    resource_query: Option<&str>,
) -> Result<Option<Vec<String>>, ProviderError> {
    let streams = dash
        .video_streams
        .iter()
        .map(|stream| (&stream.base_url, &stream.backup_urls))
        .chain(
            dash.audio_streams
                .iter()
                .map(|stream| (&stream.base_url, &stream.backup_urls)),
        );

    for (base_url, backup_urls) in streams {
        let raw_candidates = std::iter::once(base_url.as_str())
            .chain(backup_urls.iter().map(String::as_str))
            .filter(|url| !url.trim().is_empty())
            .collect::<Vec<_>>();
        if !raw_candidates.contains(&scope_url) {
            continue;
        }

        let mut candidates = Vec::with_capacity(raw_candidates.len());
        for candidate in std::iter::once(scope_url).chain(
            raw_candidates
                .into_iter()
                .filter(|candidate| *candidate != scope_url),
        ) {
            let target = super::playback_transport::resolve_dash_scope_target(
                candidate,
                resource_path,
                resource_query,
            )?;
            if !candidates.contains(&target) {
                candidates.push(target);
            }
        }
        return Ok(Some(candidates));
    }

    Ok(None)
}

fn insert_dash_manifest_metadata(
    metadata: &mut PlaybackMetadata,
    mode: BilibiliDashManifestSlot,
    dash: BilibiliDashManifest,
) {
    let PlaybackMetadata::Bilibili(metadata) = metadata else {
        return;
    };
    metadata.dash_manifests.set(mode, dash);
}

fn dash_manifest_from_metadata(
    result: &PlaybackResult,
    mode_name: &str,
) -> Result<BilibiliDashManifest, ProviderError> {
    let mode = BilibiliDashManifestSlot::parse(mode_name).ok_or(ProviderError::NotFound)?;
    result
        .metadata
        .as_ref()
        .and_then(|metadata| match metadata {
            PlaybackMetadata::Bilibili(metadata) => Some(metadata),
            _ => None,
        })
        .and_then(|metadata| metadata.dash_manifests.get(mode))
        .cloned()
        .ok_or(ProviderError::NotFound)
}

fn has_dash_manifest_metadata(result: &PlaybackResult, mode_name: &str) -> bool {
    dash_manifest_from_metadata(result, mode_name).is_ok()
}

fn available_dash_manifest_slots(result: &PlaybackResult) -> Vec<BilibiliDashManifestSlot> {
    if has_dash_manifest_metadata(result, "dash") {
        return vec![BilibiliDashManifestSlot::Dash];
    }
    [
        BilibiliDashManifestSlot::H264,
        BilibiliDashManifestSlot::Av1,
        BilibiliDashManifestSlot::Hevc,
    ]
    .into_iter()
    .filter(|slot| dash_manifest_from_metadata(result, slot.as_str()).is_ok())
    .collect()
}

const fn bilibili_dash_label(slot: BilibiliDashManifestSlot) -> &'static str {
    match slot {
        BilibiliDashManifestSlot::Dash => "DASH",
        BilibiliDashManifestSlot::H264 => "H.264",
        BilibiliDashManifestSlot::Av1 => "AV1",
        BilibiliDashManifestSlot::Hevc => "HEVC",
    }
}

const fn bilibili_dash_mode_name(slot: BilibiliDashManifestSlot) -> &'static str {
    match slot {
        BilibiliDashManifestSlot::Dash => "dash",
        BilibiliDashManifestSlot::H264 => "h264",
        BilibiliDashManifestSlot::Av1 => "av1",
        BilibiliDashManifestSlot::Hevc => "hevc",
    }
}

fn dash_manifest_from_upstream(dash: &bilibili_upstream::DashInfo) -> BilibiliDashManifest {
    BilibiliDashManifest {
        duration: dash.duration,
        min_buffer_time: dash.min_buffer_time,
        video_streams: dash
            .video_streams
            .iter()
            .map(dash_video_from_upstream)
            .collect(),
        audio_streams: dash
            .audio_streams
            .iter()
            .map(dash_audio_from_upstream)
            .collect(),
    }
}

fn bilibili_dash_video_codec(codecs: &str) -> super::PlaybackVideoCodec {
    let codecs = codecs.trim().to_ascii_lowercase();
    if codecs.starts_with("hev1") || codecs.starts_with("hvc1") {
        super::PlaybackVideoCodec::Hevc
    } else if codecs.starts_with("av01") {
        super::PlaybackVideoCodec::Av1
    } else if codecs.starts_with("vp09") || codecs.starts_with("vp9") {
        super::PlaybackVideoCodec::Vp9
    } else {
        super::PlaybackVideoCodec::H264
    }
}

fn bilibili_dash_audio_codec(codecs: &str) -> Option<super::PlaybackAudioCodec> {
    let codecs = codecs.trim().to_ascii_lowercase();
    if codecs.starts_with("mp4a") || codecs.starts_with("aac") {
        Some(super::PlaybackAudioCodec::Aac)
    } else if codecs.starts_with("ec-3") || codecs.starts_with("eac3") {
        Some(super::PlaybackAudioCodec::Eac3)
    } else if codecs.starts_with("ac-3") || codecs.starts_with("ac3") {
        Some(super::PlaybackAudioCodec::Ac3)
    } else if codecs.starts_with("flac") {
        Some(super::PlaybackAudioCodec::Flac)
    } else if codecs.starts_with("opus") {
        Some(super::PlaybackAudioCodec::Opus)
    } else if codecs.starts_with("vorbis") {
        Some(super::PlaybackAudioCodec::Vorbis)
    } else if codecs.starts_with("mp3") {
        Some(super::PlaybackAudioCodec::Mp3)
    } else {
        None
    }
}

fn filter_bilibili_dash_manifest(
    dash: &BilibiliDashManifest,
    profile: Option<&super::PlaybackClientProfile>,
) -> BilibiliDashManifest {
    let Some(profile) = profile.filter(|profile| profile.uses_explicit_capabilities()) else {
        return dash.clone();
    };
    let transport = super::PlaybackMediaTransport::Dash;
    let container = Some(super::PlaybackContainer::Mp4);

    BilibiliDashManifest {
        duration: dash.duration,
        min_buffer_time: dash.min_buffer_time,
        video_streams: dash
            .video_streams
            .iter()
            .filter(|stream| {
                profile.supports_codec_string(
                    transport,
                    container,
                    Some(bilibili_dash_video_codec(&stream.codecs)),
                    None,
                    &stream.codecs,
                )
            })
            .cloned()
            .collect(),
        audio_streams: dash
            .audio_streams
            .iter()
            .filter(|stream| {
                profile.supports_codec_string(
                    transport,
                    container,
                    None,
                    bilibili_dash_audio_codec(&stream.codecs),
                    &stream.codecs,
                )
            })
            .cloned()
            .collect(),
    }
}

fn merge_bilibili_dash_manifests(
    primary: &BilibiliDashManifest,
    additional: Option<&BilibiliDashManifest>,
) -> BilibiliDashManifest {
    let mut merged = primary.clone();
    if let Some(additional) = additional {
        for stream in &additional.video_streams {
            if !merged.video_streams.iter().any(|existing| {
                existing.id == stream.id
                    && existing.codecid == stream.codecid
                    && existing.codecs.eq_ignore_ascii_case(&stream.codecs)
            }) {
                merged.video_streams.push(stream.clone());
            }
        }
        for stream in &additional.audio_streams {
            if !merged.audio_streams.iter().any(|existing| {
                existing.id == stream.id && existing.codecs.eq_ignore_ascii_case(&stream.codecs)
            }) {
                merged.audio_streams.push(stream.clone());
            }
        }
    }
    merged
}

#[derive(Clone, Copy)]
struct BilibiliDashPlaybackOptions<'a> {
    provider_instance_name: Option<&'a str>,
    subtitles: &'a [PlaybackSubtitle],
    danmakus: &'a [PlaybackDanmaku],
    client_profile: Option<&'a super::PlaybackClientProfile>,
}

fn bilibili_dash_playback_infos(
    metadata: &mut PlaybackMetadata,
    content_descriptor: &str,
    primary_dash: &BilibiliDashManifest,
    hevc_dash: Option<&BilibiliDashManifest>,
    options: BilibiliDashPlaybackOptions<'_>,
) -> Result<(HashMap<String, PlaybackInfo>, String), ProviderError> {
    let BilibiliDashPlaybackOptions {
        provider_instance_name,
        subtitles,
        danmakus,
        client_profile,
    } = options;
    let merged = merge_bilibili_dash_manifests(primary_dash, hevc_dash);
    let upstream_has_audio = !merged.audio_streams.is_empty();
    let dash = filter_bilibili_dash_manifest(&merged, client_profile);
    if dash.video_streams.is_empty() {
        if client_profile.is_some_and(super::PlaybackClientProfile::uses_explicit_capabilities) {
            return Err(ProviderError::ClientIncompatible {
                reason:
                    "Bilibili DASH has no video representation matching the client codec profile"
                        .to_string(),
                required_capability: Some("dash_video_codec_string".to_string()),
            });
        }
        return Err(ProviderError::ApiError(
            "Bilibili DASH response did not include playable video streams".to_string(),
        ));
    }
    if upstream_has_audio && dash.audio_streams.is_empty() {
        return Err(ProviderError::ClientIncompatible {
            reason: "Bilibili DASH has no audio representation matching the client codec profile"
                .to_string(),
            required_capability: Some("dash_audio_codec_string".to_string()),
        });
    }

    let slot = BilibiliDashManifestSlot::Dash;
    let mode_name = bilibili_dash_mode_name(slot).to_string();
    insert_dash_manifest_metadata(metadata, slot, dash.clone());
    let swarm_id = bilibili_dash_manifest_swarm_id(provider_instance_name, content_descriptor);
    let manifest_expires_at = dash_manifest_expiration(&dash);
    let info = PlaybackInfo {
        thumbnail: None,
        medias: vec![playback_media(
            bilibili_dash_label(slot).to_string(),
            "mpd".to_string(),
            manifest_expires_at,
            Some(swarm_id),
            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDashManifest {
                version: String::new(),
                expires_at: manifest_expires_at.unwrap_or_default(),
                mode_name: mode_name.clone(),
                headers: bilibili_headers(),
            }),
        )],
        default_media_index: Some(0),
        subtitles: subtitles.to_vec(),
        default_subtitle_index: None,
        danmakus: danmakus.to_vec(),
        default_danmaku_index: (!danmakus.is_empty()).then_some(0),
    };
    Ok((HashMap::from([(mode_name.clone(), info)]), mode_name))
}

fn dash_video_from_upstream(stream: &bilibili_upstream::VideoStream) -> BilibiliDashVideoStream {
    BilibiliDashVideoStream {
        id: stream.id,
        quality_name: stream.quality_name.clone(),
        base_url: stream.base_url.clone(),
        backup_urls: stream.backup_urls.clone(),
        mime_type: stream.mime_type.clone(),
        codecs: stream.codecs.clone(),
        width: stream.width,
        height: stream.height,
        frame_rate: stream.frame_rate.clone(),
        bandwidth: stream.bandwidth,
        codecid: stream.codecid,
        sar: stream.sar.clone(),
        start_with_sap: stream.start_with_sap,
        segment_base: stream
            .segment_base
            .as_ref()
            .map(dash_segment_base_from_upstream),
    }
}

fn dash_audio_from_upstream(stream: &bilibili_upstream::AudioStream) -> BilibiliDashAudioStream {
    BilibiliDashAudioStream {
        id: stream.id,
        quality_name: stream.quality_name.clone(),
        base_url: stream.base_url.clone(),
        backup_urls: stream.backup_urls.clone(),
        mime_type: stream.mime_type.clone(),
        codecs: stream.codecs.clone(),
        bandwidth: stream.bandwidth,
        start_with_sap: stream.start_with_sap,
        segment_base: stream
            .segment_base
            .as_ref()
            .map(dash_segment_base_from_upstream),
        audio_sampling_rate: stream.audio_sampling_rate,
    }
}

fn dash_segment_base_from_upstream(
    segment_base: &bilibili_upstream::SegmentBase,
) -> BilibiliDashSegmentBase {
    BilibiliDashSegmentBase {
        index_range: segment_base.index_range.clone(),
        initialization_range: segment_base.initialization_range.clone(),
    }
}

fn xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn dash_duration(value: f64) -> String {
    if value.is_finite() && value > 0.0 {
        format!("PT{value:.3}S")
    } else {
        "PT0S".to_string()
    }
}

fn frame_rate_attr(value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        String::new()
    } else {
        format!(" frameRate=\"{}\"", xml_escape(value))
    }
}

fn segment_base_xml(segment_base: Option<&BilibiliDashSegmentBase>) -> String {
    let Some(segment_base) = segment_base else {
        return String::new();
    };
    if segment_base.index_range.trim().is_empty()
        && segment_base.initialization_range.trim().is_empty()
    {
        return String::new();
    }
    let mut xml = String::new();
    let _ = write!(
        xml,
        "<SegmentBase indexRange=\"{}\">",
        xml_escape(&segment_base.index_range)
    );
    if !segment_base.initialization_range.trim().is_empty() {
        let _ = write!(
            xml,
            "<Initialization range=\"{}\"/>",
            xml_escape(&segment_base.initialization_range)
        );
    }
    xml.push_str("</SegmentBase>");
    xml
}

fn build_bilibili_mpd_manifest<F>(
    dash: &BilibiliDashManifest,
    mut url_for: F,
) -> Result<String, ProviderError>
where
    F: FnMut(usize, &str) -> String,
{
    let mut url_index = 0usize;
    let mut xml = String::new();
    let media_presentation_duration = dash_duration(dash.duration);
    let min_buffer_time = dash_duration(dash.min_buffer_time);
    let _ = write!(
        xml,
        r#"<?xml version="1.0" encoding="UTF-8"?><MPD xmlns="urn:mpeg:dash:schema:mpd:2011" type="static" profiles="urn:mpeg:dash:profile:isoff-on-demand:2011" mediaPresentationDuration="{media_presentation_duration}" minBufferTime="{min_buffer_time}"><Period id="0" duration="{media_presentation_duration}">"#
    );

    for codec in [
        super::PlaybackVideoCodec::H264,
        super::PlaybackVideoCodec::Hevc,
        super::PlaybackVideoCodec::Av1,
        super::PlaybackVideoCodec::Vp9,
    ] {
        let streams = dash
            .video_streams
            .iter()
            .filter(|stream| {
                !stream.base_url.trim().is_empty()
                    && bilibili_dash_video_codec(&stream.codecs) == codec
            })
            .collect::<Vec<_>>();
        if streams.is_empty() {
            continue;
        }
        let (adaptation_id, label, selection_priority) = bilibili_dash_video_adaptation(codec);
        let _ = write!(
            xml,
            r#"<AdaptationSet id="{adaptation_id}" contentType="video" segmentAlignment="true" startWithSAP="1" selectionPriority="{selection_priority}"><Label>{label}</Label><Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>"#,
        );
        for stream in streams {
            if stream.base_url.trim().is_empty() {
                continue;
            }
            let mut base_urls = String::new();
            for upstream_url in std::iter::once(&stream.base_url).chain(&stream.backup_urls) {
                if upstream_url.trim().is_empty() {
                    continue;
                }
                let resolved_url = url_for(url_index, upstream_url);
                url_index += 1;
                let _ = write!(
                    base_urls,
                    "<BaseURL>{}</BaseURL>",
                    xml_escape(&resolved_url)
                );
            }
            let segment_base = segment_base_xml(stream.segment_base.as_ref());
            let sar_attr = if stream.sar.trim().is_empty() {
                String::new()
            } else {
                format!(" sar=\"{}\"", xml_escape(&stream.sar))
            };
            let _ = write!(
                xml,
                r#"<Representation id="video-{adaptation_id}-{}-{}" mimeType="{}" codecs="{}" width="{}" height="{}" bandwidth="{}" startWithSAP="{}"{}{}><Label>{}</Label>{}{}</Representation>"#,
                stream.id,
                stream.codecid,
                xml_escape(&stream.mime_type),
                xml_escape(&stream.codecs),
                stream.width,
                stream.height,
                stream.bandwidth,
                stream.start_with_sap,
                frame_rate_attr(&stream.frame_rate),
                sar_attr,
                xml_escape(&stream.quality_name),
                base_urls,
                segment_base,
            );
        }
        xml.push_str("</AdaptationSet>");
    }

    for codec in [
        Some(super::PlaybackAudioCodec::Aac),
        Some(super::PlaybackAudioCodec::Eac3),
        Some(super::PlaybackAudioCodec::Ac3),
        Some(super::PlaybackAudioCodec::Flac),
        Some(super::PlaybackAudioCodec::Opus),
        Some(super::PlaybackAudioCodec::Vorbis),
        Some(super::PlaybackAudioCodec::Mp3),
        None,
    ] {
        let streams = dash
            .audio_streams
            .iter()
            .filter(|stream| {
                !stream.base_url.trim().is_empty()
                    && bilibili_dash_audio_codec(&stream.codecs) == codec
            })
            .collect::<Vec<_>>();
        if streams.is_empty() {
            continue;
        }
        let (adaptation_id, label, selection_priority) = bilibili_dash_audio_adaptation(codec);
        let _ = write!(
            xml,
            r#"<AdaptationSet id="{adaptation_id}" contentType="audio" segmentAlignment="true" startWithSAP="1" selectionPriority="{selection_priority}"><Label>{label}</Label><Role schemeIdUri="urn:mpeg:dash:role:2011" value="main"/>"#,
        );
        for stream in streams {
            let mut base_urls = String::new();
            for upstream_url in std::iter::once(&stream.base_url).chain(&stream.backup_urls) {
                if upstream_url.trim().is_empty() {
                    continue;
                }
                let resolved_url = url_for(url_index, upstream_url);
                url_index += 1;
                let _ = write!(
                    base_urls,
                    "<BaseURL>{}</BaseURL>",
                    xml_escape(&resolved_url)
                );
            }
            let segment_base = segment_base_xml(stream.segment_base.as_ref());
            let sampling_rate_attr = if stream.audio_sampling_rate == 0 {
                String::new()
            } else {
                format!(r#" audioSamplingRate="{}""#, stream.audio_sampling_rate)
            };
            let _ = write!(
                xml,
                r#"<Representation id="audio-{adaptation_id}-{}" mimeType="{}" codecs="{}" bandwidth="{}" startWithSAP="{}"{}><Label>{}</Label>{}{}</Representation>"#,
                stream.id,
                xml_escape(&stream.mime_type),
                xml_escape(&stream.codecs),
                stream.bandwidth,
                stream.start_with_sap,
                sampling_rate_attr,
                xml_escape(&stream.quality_name),
                base_urls,
                segment_base,
            );
        }
        xml.push_str("</AdaptationSet>");
    }

    xml.push_str("</Period></MPD>");

    if url_index == 0 {
        return Err(ProviderError::ApiError(
            "Bilibili DASH manifest did not include playable URLs".to_string(),
        ));
    }

    Ok(xml)
}

const fn bilibili_dash_video_adaptation(
    codec: super::PlaybackVideoCodec,
) -> (u64, &'static str, u64) {
    match codec {
        super::PlaybackVideoCodec::H264 => (100, "H.264", 400),
        super::PlaybackVideoCodec::Hevc => (110, "HEVC", 300),
        super::PlaybackVideoCodec::Av1 => (120, "AV1", 200),
        super::PlaybackVideoCodec::Vp9 => (130, "VP9", 100),
    }
}

const fn bilibili_dash_audio_adaptation(
    codec: Option<super::PlaybackAudioCodec>,
) -> (u64, &'static str, u64) {
    match codec {
        Some(super::PlaybackAudioCodec::Aac) => (200, "AAC", 700),
        Some(super::PlaybackAudioCodec::Eac3) => (210, "E-AC-3", 600),
        Some(super::PlaybackAudioCodec::Ac3) => (220, "AC-3", 500),
        Some(super::PlaybackAudioCodec::Flac) => (230, "FLAC", 400),
        Some(super::PlaybackAudioCodec::Opus) => (240, "Opus", 300),
        Some(super::PlaybackAudioCodec::Vorbis) => (250, "Vorbis", 200),
        Some(super::PlaybackAudioCodec::Mp3) => (260, "MP3", 100),
        None => (290, "Audio", 1),
    }
}

fn bilibili_route_selection(
    proxy_mode: crate::models::PlaybackProxyMode,
) -> super::PlaybackRouteSelection {
    use crate::models::PlaybackProxyMode;

    match proxy_mode {
        PlaybackProxyMode::Auto | PlaybackProxyMode::Only => {
            super::PlaybackRouteSelection::PROXY_ONLY
        }
        PlaybackProxyMode::Prefer => super::PlaybackRouteSelection::PROXY_PREFERRED,
        PlaybackProxyMode::DirectPrefer => super::PlaybackRouteSelection::DIRECT_PREFERRED,
        PlaybackProxyMode::DirectOnly => super::PlaybackRouteSelection::DIRECT_ONLY,
    }
}

fn bilibili_auto_variants(source_config: SourceConfig<'_>) -> Vec<&'static str> {
    use crate::models::{BilibiliHistoryType, BilibiliPlaylistSource};

    match source_config {
        SourceConfig::Media(MediaSourceConfig::Bilibili(BilibiliSourceConfig::Video(_))) => {
            vec!["video"]
        }
        SourceConfig::Media(MediaSourceConfig::Bilibili(BilibiliSourceConfig::Pgc(_))) => {
            vec!["pgc"]
        }
        SourceConfig::Media(MediaSourceConfig::Bilibili(BilibiliSourceConfig::Live(_))) => {
            vec!["live"]
        }
        SourceConfig::DynamicPlaylist(PlaylistSourceConfig::Bilibili(config)) => {
            match config.source {
                BilibiliPlaylistSource::LiveRecommended
                | BilibiliPlaylistSource::LiveFollowed
                | BilibiliPlaylistSource::LiveArea { .. }
                | BilibiliPlaylistSource::History {
                    history_type: BilibiliHistoryType::Live,
                } => vec!["live"],
                BilibiliPlaylistSource::PgcSeason { .. }
                | BilibiliPlaylistSource::PgcTimeline { .. } => vec!["pgc"],
                BilibiliPlaylistSource::History {
                    history_type: BilibiliHistoryType::All,
                } => vec!["video", "pgc", "live"],
                BilibiliPlaylistSource::History {
                    history_type: BilibiliHistoryType::Archive,
                }
                | BilibiliPlaylistSource::VideoParts { .. }
                | BilibiliPlaylistSource::Popular
                | BilibiliPlaylistSource::Recommended
                | BilibiliPlaylistSource::UpVideos { .. }
                | BilibiliPlaylistSource::FavoriteVideos { .. }
                | BilibiliPlaylistSource::CollectionVideos { .. }
                | BilibiliPlaylistSource::SeriesVideos { .. }
                | BilibiliPlaylistSource::WatchLater => vec!["video"],
            }
        }
        _ => Vec::new(),
    }
}

fn bilibili_playback_proxy_policy(
    source_config: SourceConfig<'_>,
) -> Result<PlaybackProxyPolicy, ProviderError> {
    let current_mode = match source_config {
        SourceConfig::Media(MediaSourceConfig::Bilibili(config)) => config.proxy_mode(),
        SourceConfig::DynamicPlaylist(PlaylistSourceConfig::Bilibili(config)) => config.proxy_mode,
        _ => {
            return Err(ProviderError::InvalidConfig(
                "Bilibili requires Bilibili source_config".to_string(),
            ));
        }
    };
    let auto_policies = bilibili_auto_variants(source_config)
        .into_iter()
        .map(|variant| {
            PlaybackProxyAutoPolicy::new(
                variant,
                crate::models::PlaybackProxyMode::Only,
                PlaybackProxyAutoReason::SignedResource,
            )
        })
        .collect();
    Ok(PlaybackProxyPolicy::all_modes(current_mode, auto_policies))
}

fn populate_bilibili_proxy_attachments(
    original_info: &PlaybackInfo,
    proxy_info: &mut PlaybackInfo,
    version: &str,
    expires_at: i64,
    mode_name: &str,
) {
    proxy_info.subtitles = original_info
        .subtitles
        .iter()
        .enumerate()
        .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
            name: subtitle.name().to_string(),
            language: subtitle.language().to_string(),
            format: subtitle.format().to_string(),
            p2p_swarm_id: subtitle.p2p_swarm_id.clone(),
            provider: PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Proxy {
                version: version.to_string(),
                expires_at: subtitle
                    .expiration_timestamp()
                    .into_iter()
                    .chain(std::iter::once(expires_at))
                    .min()
                    .unwrap_or(expires_at),
                mode_name: mode_name.to_string(),
                subtitle_index,
                url: subtitle.upstream_url().to_string(),
                headers: subtitle.upstream_headers(),
            }),
        })
        .collect();
    proxy_info.danmakus = original_info
        .danmakus
        .iter()
        .enumerate()
        .map(|(danmaku_index, danmaku)| {
            let provider = match &danmaku.provider {
                PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
                    room_id,
                    media_id,
                }) => PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
                    room_id: *room_id,
                    media_id: *media_id,
                }),
                PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::DynamicLive {
                    room_id,
                    playlist_id,
                    live_room_id,
                }) => PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::DynamicLive {
                    room_id: *room_id,
                    playlist_id: *playlist_id,
                    live_room_id: *live_room_id,
                }),
                _ => PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileProxy {
                    version: version.to_string(),
                    expires_at: danmaku
                        .expiration_timestamp()
                        .into_iter()
                        .chain(std::iter::once(expires_at))
                        .min()
                        .unwrap_or(expires_at),
                    danmaku_index,
                    url: danmaku.upstream_url().unwrap_or_default().to_string(),
                    headers: danmaku.upstream_headers(),
                }),
            };
            PlaybackDanmaku {
                name: danmaku.name().to_string(),
                format: danmaku.format().map(ToString::to_string),
                p2p_swarm_id: danmaku.p2p_swarm_id.clone(),
                provider,
            }
        })
        .collect();
}

fn attach_bilibili_live_danmaku(
    result: &mut PlaybackResult,
    context: Option<&ProviderContext<'_>>,
) {
    let Some(live_room_id) = result
        .metadata
        .as_ref()
        .and_then(PlaybackMetadata::as_bilibili)
        .filter(|metadata| metadata.kind == BilibiliPlaybackKind::Live && metadata.is_live)
        .and_then(|metadata| metadata.room_id)
    else {
        return;
    };
    let track = context.and_then(|context| bilibili_live_danmaku_track(context, live_room_id));

    for info in result.playback_infos.values_mut() {
        info.danmakus.retain(|danmaku| {
            !matches!(
                danmaku.provider,
                PlaybackDanmakuProvider::Bilibili(
                    PlaybackBilibiliDanmaku::Live { .. }
                        | PlaybackBilibiliDanmaku::DynamicLive { .. }
                )
            )
        });
        if let Some(track) = track.as_ref() {
            info.danmakus.push(track.clone());
            info.default_danmaku_index = Some(info.danmakus.len() - 1);
        } else {
            info.default_danmaku_index = (!info.danmakus.is_empty()).then_some(0);
        }
    }
}

fn mark_bilibili_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    proxy_mode: crate::models::PlaybackProxyMode,
    context: Option<&ProviderContext<'_>>,
) {
    // DASH/MPD modes keep both direct and proxy manifests: app clients can
    // apply the returned Bilibili headers to the manifest and segment requests,
    // while proxy siblings remain as a server-mediated fallback.
    attach_bilibili_live_danmaku(result, context);
    let selection = bilibili_route_selection(proxy_mode);
    let client_profile = context.and_then(ProviderContext::playback_client_profile);
    let original_default_mode = result.default_mode.clone();
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();
    let mut generated = std::collections::HashMap::new();

    for (mode_name, original_info) in original_modes {
        if original_info.medias.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }

        if original_info.medias.len() == 1
            && matches!(
                original_info.medias[0].provider,
                PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDurlManifest { .. })
            )
        {
            let mut direct_info = original_info.clone();
            if selection.direct
                && super::direct_playback_media_supported_by_client(
                    client_profile,
                    &mode_name,
                    &direct_info.medias[0],
                )
            {
                if let PlaybackMediaProvider::Bilibili(
                    PlaybackBilibiliMedia::DirectDurlManifest {
                        version: resource_version,
                        expires_at: resource_expires_at,
                        mode_name: resource_mode_name,
                        ..
                    },
                ) = &mut direct_info.medias[0].provider
                {
                    *resource_version = version.to_string();
                    *resource_expires_at = expires_at;
                    resource_mode_name.clone_from(&mode_name);
                }
                if let Some(direct_info) = super::build_direct_playback_info_for_client(
                    &mode_name,
                    &direct_info,
                    client_profile,
                ) {
                    generated.insert(mode_name.clone(), direct_info);
                }
            }

            // Direct DURL playback serves a generated manifest with Bilibili
            // segment URLs. The proxy sibling preserves server forwarding and
            // backup CDN candidate selection for proxy playback modes.
            if selection.proxy
                && super::proxy_playback_media_supported_by_client(
                    client_profile,
                    &mode_name,
                    &original_info.medias[0],
                )
            {
                let mut proxy_info = original_info.clone();
                if let Some(media) = proxy_info.medias.first_mut() {
                    if let PlaybackMediaProvider::Bilibili(
                        PlaybackBilibiliMedia::DirectDurlManifest {
                            segments, headers, ..
                        },
                    ) = &media.provider
                    {
                        media.provider =
                            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DurlManifest {
                                version: version.to_string(),
                                expires_at,
                                mode_name: mode_name.clone(),
                                segments: segments.clone(),
                                headers: headers.clone(),
                            });
                    }
                }
                if !proxy_info.medias.is_empty() {
                    populate_bilibili_proxy_attachments(
                        &original_info,
                        &mut proxy_info,
                        version,
                        expires_at,
                        &mode_name,
                    );
                    generated.insert(format!("proxy_{mode_name}"), proxy_info);
                }
            }
            continue;
        }

        let use_mpd_manifest = original_info
            .medias
            .first()
            .is_some_and(|media| media.format == "mpd")
            && has_dash_manifest_metadata(result, &mode_name);
        let dash_slots = if use_mpd_manifest {
            available_dash_manifest_slots(result)
                .into_iter()
                .filter(|slot| {
                    BilibiliDashManifestSlot::parse(&mode_name)
                        .is_none_or(|mode_slot| mode_slot == *slot)
                })
                .filter_map(|slot| {
                    dash_manifest_from_metadata(result, slot.as_str())
                        .ok()
                        .map(|manifest| {
                            let p2p_swarm_id = bilibili_content_descriptor(result).map(|content| {
                                bilibili_dash_manifest_swarm_id(
                                    result.provider_instance_name.as_deref(),
                                    &content,
                                )
                            });
                            (slot, dash_manifest_expiration(&manifest), p2p_swarm_id)
                        })
                })
                .collect::<Vec<_>>()
        } else {
            Default::default()
        };

        let mut direct_info = original_info.clone();
        if selection.direct && use_mpd_manifest {
            let source_media = original_info.medias.first();
            direct_info.medias = dash_slots
                .iter()
                .map(|(slot, slot_expires_at, p2p_swarm_id)| {
                    playback_media(
                        bilibili_dash_label(*slot).to_string(),
                        "mpd".to_string(),
                        *slot_expires_at,
                        p2p_swarm_id.clone(),
                        PlaybackMediaProvider::Bilibili(
                            PlaybackBilibiliMedia::DirectDashManifest {
                                version: version.to_string(),
                                expires_at,
                                mode_name: bilibili_dash_mode_name(*slot).to_string(),
                                headers: source_media
                                    .map_or_else(bilibili_headers, PlaybackMedia::upstream_headers),
                            },
                        ),
                    )
                })
                .filter(|media| {
                    super::direct_playback_media_supported_by_client(
                        client_profile,
                        &mode_name,
                        media,
                    )
                })
                .collect();
            if !direct_info.medias.is_empty() {
                direct_info.default_media_index = Some(0);
                if let Some(direct_info) = super::build_direct_playback_info_for_client(
                    &mode_name,
                    &direct_info,
                    client_profile,
                ) {
                    generated.insert(mode_name.clone(), direct_info);
                }
            }
        } else if selection.direct {
            if let Some(direct_info) = super::build_direct_playback_info_for_client(
                &mode_name,
                &direct_info,
                client_profile,
            ) {
                generated.insert(mode_name.clone(), direct_info);
            }
        }

        if selection.proxy {
            let proxy_mode_name = format!("proxy_{mode_name}");
            let mut proxy_info = original_info.clone();
            if use_mpd_manifest {
                proxy_info.medias = dash_slots
                    .iter()
                    .map(|(slot, slot_expires_at, p2p_swarm_id)| {
                        playback_media(
                            bilibili_dash_label(*slot).to_string(),
                            "mpd".to_string(),
                            *slot_expires_at,
                            p2p_swarm_id.clone(),
                            PlaybackMediaProvider::Bilibili(
                                PlaybackBilibiliMedia::ProxyDashManifest {
                                    version: version.to_string(),
                                    expires_at,
                                    mode_name: bilibili_dash_mode_name(*slot).to_string(),
                                },
                            ),
                        )
                    })
                    .filter(|media| {
                        super::proxy_playback_media_supported_by_client(
                            client_profile,
                            &mode_name,
                            media,
                        )
                    })
                    .collect();
                proxy_info.default_media_index = (!proxy_info.medias.is_empty()).then_some(0);
            } else {
                let (proxy_medias, proxy_default_media_index) = super::map_playback_resources(
                    &original_info.medias,
                    original_info.default_media_index,
                    |url_index, media| {
                        if !super::proxy_playback_media_supported_by_client(
                            client_profile,
                            &mode_name,
                            media,
                        ) {
                            return None;
                        }
                        let url = media.upstream_url()?.to_string();
                        Some(playback_media(
                            media.name.clone(),
                            media.format.clone(),
                            media.expire_at.map(|dt| dt.timestamp()),
                            media.p2p_swarm_id.clone(),
                            PlaybackMediaProvider::Bilibili(
                                if super::playback_media_is_hls(&mode_name, media) {
                                    PlaybackBilibiliMedia::ProxyHlsManifest {
                                        version: version.to_string(),
                                        expires_at,
                                        mode_name: mode_name.clone(),
                                        url_index,
                                        url,
                                        headers: media.upstream_headers(),
                                    }
                                } else {
                                    PlaybackBilibiliMedia::ProxyMediaStream {
                                        version: version.to_string(),
                                        expires_at,
                                        mode_name: mode_name.clone(),
                                        url_index,
                                        url,
                                        headers: media.upstream_headers(),
                                    }
                                },
                            ),
                        ))
                    },
                );
                proxy_info.medias = proxy_medias;
                proxy_info.default_media_index = proxy_default_media_index;
            }
            if !proxy_info.medias.is_empty() {
                populate_bilibili_proxy_attachments(
                    &original_info,
                    &mut proxy_info,
                    version,
                    expires_at,
                    &mode_name,
                );
                generated.insert(proxy_mode_name, proxy_info);
            }
        }
    }

    result.playback_infos = generated;
    super::select_generated_playback_default(
        result,
        &original_default_mode,
        selection.prefer_proxy,
    );
}

fn bilibili_credential_server_id() -> String {
    BilibiliProvider::credential_server_id()
}

fn is_bilibili_pgc_dash_unavailable(error: &synctv_media_providers::ProviderClientError) -> bool {
    is_bilibili_dash_unavailable_error(error, "get_dash_pgcurl")
}

fn is_bilibili_video_dash_unavailable(error: &synctv_media_providers::ProviderClientError) -> bool {
    is_bilibili_dash_unavailable_error(error, "get_dash_video_url")
}

fn is_bilibili_dash_unavailable_error(
    error: &synctv_media_providers::ProviderClientError,
    rpc_context: &str,
) -> bool {
    matches!(
        error,
        synctv_media_providers::ProviderClientError::Api { message, .. }
            if message.contains("DASH")
                || (message.contains(rpc_context) && message.contains("API error (code 0)"))
    )
}

fn playback_cache_entry(
    config: &BilibiliSourceConfig,
    credential_cache_partition: &str,
    playback_client_profile: Option<&super::PlaybackClientProfile>,
) -> Result<(String, Duration), ProviderError> {
    let profile_partition = playback_client_profile
        .filter(|profile| profile.uses_explicit_capabilities())
        .map_or_else(
            || "legacy".to_string(),
            |profile| {
                let digest = sha2::Sha256::digest(profile.cache_fingerprint().as_bytes());
                hex::encode(digest).chars().take(24).collect()
            },
        );
    match config {
        BilibiliSourceConfig::Video(config) => {
            let video_key = BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?
                .cache_key_part();
            Ok((
                format!(
                    "playback:{BILIBILI_PLAYBACK_CACHE_SCHEMA_VERSION}:video:{video_key}:{}:{credential_cache_partition}:profile:{profile_partition}",
                    config.cid,
                ),
                Duration::from_hours(2),
            ))
        }
        BilibiliSourceConfig::Pgc(config) => Ok((
            format!(
                "playback:{BILIBILI_PLAYBACK_CACHE_SCHEMA_VERSION}:pgc:{}:{}:{credential_cache_partition}:profile:{profile_partition}",
                config.epid, config.cid,
            ),
            Duration::from_hours(2),
        )),
        BilibiliSourceConfig::Live(config) => Ok((
            format!(
                "playback:{BILIBILI_PLAYBACK_CACHE_SCHEMA_VERSION}:live:{}:{credential_cache_partition}:{}",
                config.room_id,
                bilibili_live_transport_cache_token(playback_client_profile),
            ),
            Duration::from_mins(2),
        )),
    }
}

async fn resolve_optional_bilibili_cookies(
    ctx: &ProviderContext<'_>,
    credential_user_id: Option<UserId>,
) -> Result<(HashMap<String, String>, String), ProviderError> {
    if matches!(ctx.actor(), super::ProviderActor::Guest) {
        return Ok((HashMap::new(), "guest-anonymous".to_string()));
    }
    let Some(credential_user_id) = credential_user_id else {
        return Ok((HashMap::new(), "anonymous".to_string()));
    };
    let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
        ProviderError::Internal(
            "provider_access_service not available in ProviderContext".to_string(),
        )
    })?;
    let access = access_service
        .bilibili_access(credential_user_id, ctx.request_context())
        .await?;
    Ok(access.into_cookies_and_partition())
}

fn bilibili_optional_credential_user_id(
    ctx: &ProviderContext<'_>,
    credential_policy: ProviderCredentialPolicy,
) -> Result<Option<UserId>, ProviderError> {
    if matches!(ctx.actor(), super::ProviderActor::Guest) {
        return Ok(None);
    }
    let credential_user_id = ctx.selected_credential_user_id(credential_policy);
    if credential_policy.uses_resource_owner() && credential_user_id.is_none() {
        return Err(ProviderError::Internal(
            "Bilibili credential owner is unavailable".to_string(),
        ));
    }
    Ok(credential_user_id)
}

fn bilibili_credential_dependencies(
    ctx: &ProviderContext<'_>,
    source_config: SourceConfig<'_>,
) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
    let (credential_policy, required) = match source_config {
        SourceConfig::Media(config) => {
            let shared = BilibiliSourceConfig::from_media_config(config)?.shared();
            (ProviderCredentialPolicy::from_shared(shared), shared)
        }
        SourceConfig::DynamicPlaylist(config) => {
            let config = BilibiliProvider::playlist_config(config)?;
            (
                ProviderCredentialPolicy::from_shared(config.shared),
                BilibiliProvider::playlist_requires_credential(&config.source),
            )
        }
    };
    if matches!(ctx.actor(), super::ProviderActor::Guest) && !required {
        return Ok(Vec::new());
    }
    let credential_user_id = match ctx.actor() {
        super::ProviderActor::Guest if required => ctx.credential_owner_id().copied(),
        _ => ctx.selected_credential_user_id(credential_policy),
    };
    let Some(credential_user_id) = credential_user_id else {
        return if required {
            Err(ProviderError::CredentialRequired)
        } else {
            Ok(Vec::new())
        };
    };
    Ok(vec![if required {
        ProviderCredentialDependency::new(
            crate::models::SourceProvider::Bilibili,
            credential_user_id,
            bilibili_credential_server_id(),
        )
    } else {
        ProviderCredentialDependency::optional(
            crate::models::SourceProvider::Bilibili,
            credential_user_id,
            bilibili_credential_server_id(),
        )
    }])
}

impl BilibiliProvider {
    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&BilibiliPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Bilibili(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Bilibili requires Bilibili playlist source_config".to_string(),
            )),
        }
    }

    fn playlist_source_request(
        source: &BilibiliPlaylistSource,
    ) -> Result<bilibili_upstream::list_videos_req::Source, ProviderError> {
        use bilibili_upstream::list_videos_req::Source;
        Ok(match source {
            BilibiliPlaylistSource::VideoParts { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili video parts use the dedicated parts endpoint".to_string(),
                ));
            }
            BilibiliPlaylistSource::Popular => {
                Source::Popular(bilibili_upstream::PopularVideoSource {})
            }
            BilibiliPlaylistSource::Recommended => {
                Source::Recommended(bilibili_upstream::RecommendedVideoSource {})
            }
            BilibiliPlaylistSource::UpVideos { mid, keyword } => {
                Source::UpVideos(bilibili_upstream::UpVideosSource {
                    mid: *mid,
                    keyword: keyword.clone(),
                })
            }
            BilibiliPlaylistSource::FavoriteVideos { media_id } => {
                Source::FavoriteVideos(bilibili_upstream::FavoriteVideosSource {
                    media_id: *media_id,
                })
            }
            BilibiliPlaylistSource::CollectionVideos { mid, season_id } => {
                Source::CollectionVideos(bilibili_upstream::CollectionVideosSource {
                    mid: *mid,
                    season_id: *season_id,
                })
            }
            BilibiliPlaylistSource::SeriesVideos { mid, series_id } => {
                Source::SeriesVideos(bilibili_upstream::SeriesVideosSource {
                    mid: *mid,
                    series_id: *series_id,
                })
            }
            BilibiliPlaylistSource::WatchLater => {
                Source::WatchLater(bilibili_upstream::WatchLaterVideosSource {})
            }
            BilibiliPlaylistSource::PgcSeason { season_id } => {
                Source::PgcSeason(bilibili_upstream::PgcSeasonVideosSource {
                    season_id: *season_id,
                })
            }
            BilibiliPlaylistSource::LiveRecommended
            | BilibiliPlaylistSource::LiveFollowed
            | BilibiliPlaylistSource::LiveArea { .. }
            | BilibiliPlaylistSource::History { .. }
            | BilibiliPlaylistSource::PgcTimeline { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili live playlists use the live-room endpoint".to_string(),
                ));
            }
        })
    }

    const fn playlist_requires_credential(source: &BilibiliPlaylistSource) -> bool {
        matches!(
            source,
            BilibiliPlaylistSource::FavoriteVideos { .. }
                | BilibiliPlaylistSource::WatchLater
                | BilibiliPlaylistSource::LiveFollowed
                | BilibiliPlaylistSource::History { .. }
        )
    }

    async fn playlist_access(
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
    ) -> Result<BilibiliAccess, ProviderError> {
        let credential_required = Self::playlist_requires_credential(&config.source);
        let credential_policy = ProviderCredentialPolicy::from_shared(config.shared);
        let user_id = match ctx.actor() {
            super::ProviderActor::Guest | super::ProviderActor::System if credential_required => {
                ctx.credential_owner_id().copied()
            }
            super::ProviderActor::Guest => None,
            super::ProviderActor::System | super::ProviderActor::User(_) => {
                ctx.selected_credential_user_id(credential_policy)
            }
        };
        let Some(user_id) = user_id else {
            if credential_required {
                return Err(ProviderError::CredentialRequired);
            }
            return Ok(BilibiliAccess::anonymous(
                "anonymous",
                super::bound_provider_instance_name(ctx).map(str::to_owned),
            ));
        };
        let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
            ProviderError::Internal(
                "provider_access_service not available in ProviderContext".to_string(),
            )
        })?;
        let access = access_service
            .bilibili_access(user_id, ctx.request_context())
            .await?;
        if credential_required && !access.is_authenticated() {
            return Err(ProviderError::CredentialRequired);
        }
        Ok(access)
    }

    async fn list_playlist_page(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
        page: usize,
        page_size: usize,
    ) -> Result<bilibili_upstream::ListVideosResp, ProviderError> {
        let access = Self::playlist_access(ctx, config).await?;
        let client = self
            .get_client_with_context(
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        client
            .list_videos(bilibili_upstream::ListVideosReq {
                cookies: access.into_cookies(),
                source: Some(Self::playlist_source_request(&config.source)?),
                page: u64::try_from(page).map_err(|_| {
                    ProviderError::InvalidConfig("Bilibili page exceeds u64::MAX".to_string())
                })?,
                page_size: u32::try_from(page_size).map_err(|_| {
                    ProviderError::InvalidConfig("Bilibili page size exceeds u32::MAX".to_string())
                })?,
            })
            .await
            .map_err(ProviderError::from)
    }

    async fn video_parts(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
        bvid: &str,
        aid: u64,
    ) -> Result<bilibili_upstream::ListVideoPartsResp, ProviderError> {
        let access = Self::playlist_access(ctx, config).await?;
        let client = self
            .get_client_with_context(
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        client
            .list_video_parts(bilibili_upstream::ListVideoPartsReq {
                cookies: access.into_cookies(),
                bvid: bvid.to_string(),
                aid,
            })
            .await
            .map_err(ProviderError::from)
    }

    async fn list_live_playlist_page(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
        page: usize,
        page_size: usize,
    ) -> Result<bilibili_upstream::ListLiveRoomsResp, ProviderError> {
        use bilibili_upstream::list_live_rooms_req::Source;

        let access = Self::playlist_access(ctx, config).await?;
        let source = match &config.source {
            BilibiliPlaylistSource::LiveRecommended => {
                Source::Recommended(bilibili_upstream::RecommendedLiveRoomsSource {})
            }
            BilibiliPlaylistSource::LiveFollowed => {
                Source::Followed(bilibili_upstream::FollowedLiveRoomsSource {})
            }
            BilibiliPlaylistSource::LiveArea {
                parent_area_id,
                area_id,
            } => Source::Area(bilibili_upstream::AreaLiveRoomsSource {
                parent_area_id: *parent_area_id,
                area_id: *area_id,
            }),
            _ => {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili live playlist source is required".to_string(),
                ));
            }
        };
        let client = self
            .get_client_with_context(
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        client
            .list_live_rooms(bilibili_upstream::ListLiveRoomsReq {
                cookies: access.into_cookies(),
                source: Some(source),
                page: u64::try_from(page).map_err(|_| {
                    ProviderError::InvalidConfig("Bilibili page exceeds u64::MAX".to_string())
                })?,
                page_size: u32::try_from(page_size).map_err(|_| {
                    ProviderError::InvalidConfig("Bilibili page size exceeds u32::MAX".to_string())
                })?,
            })
            .await
            .map_err(ProviderError::from)
    }

    fn decode_history_cursor(
        cursor: Option<&str>,
    ) -> Result<Option<bilibili_upstream::HistoryCursor>, ProviderError> {
        let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) else {
            return Ok(None);
        };
        let bytes = URL_SAFE_NO_PAD.decode(cursor).map_err(|_| {
            ProviderError::InvalidConfig("Bilibili history cursor is invalid".to_string())
        })?;
        let cursor: BilibiliHistoryCursor = serde_json::from_slice(&bytes).map_err(|_| {
            ProviderError::InvalidConfig("Bilibili history cursor is invalid".to_string())
        })?;
        Ok(Some(bilibili_upstream::HistoryCursor {
            max: cursor.max,
            view_at: cursor.view_at,
            business: cursor.business,
        }))
    }

    fn encode_history_cursor(
        cursor: Option<bilibili_upstream::HistoryCursor>,
    ) -> Result<Option<String>, ProviderError> {
        cursor
            .map(|cursor| {
                serde_json::to_vec(&BilibiliHistoryCursor {
                    max: cursor.max,
                    view_at: cursor.view_at,
                    business: cursor.business,
                })
                .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
                .map_err(|error| ProviderError::Internal(error.to_string()))
            })
            .transpose()
    }

    async fn list_history_page(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
        cursor: Option<&str>,
        page_size: usize,
    ) -> Result<bilibili_upstream::ListHistoryResp, ProviderError> {
        let BilibiliPlaylistSource::History { history_type } = config.source else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili history source is required".to_string(),
            ));
        };
        let access = Self::playlist_access(ctx, config).await?;
        let client = self
            .get_client_with_context(
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        client
            .list_history(bilibili_upstream::ListHistoryReq {
                cookies: access.into_cookies(),
                r#type: match history_type {
                    BilibiliHistoryType::All => bilibili_upstream::HistoryType::All as i32,
                    BilibiliHistoryType::Archive => bilibili_upstream::HistoryType::Archive as i32,
                    BilibiliHistoryType::Live => bilibili_upstream::HistoryType::Live as i32,
                },
                cursor: Self::decode_history_cursor(cursor)?,
                page_size: u32::try_from(page_size).map_err(|_| {
                    ProviderError::InvalidConfig(
                        "Bilibili history page size exceeds u32::MAX".to_string(),
                    )
                })?,
            })
            .await
            .map_err(ProviderError::from)
    }

    async fn list_pgc_timeline(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
    ) -> Result<bilibili_upstream::ListPgcTimelineResp, ProviderError> {
        let BilibiliPlaylistSource::PgcTimeline {
            timeline_type,
            before_days,
            after_days,
        } = config.source
        else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili PGC timeline source is required".to_string(),
            ));
        };
        let access = Self::playlist_access(ctx, config).await?;
        let client = self
            .get_client_with_context(
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        client
            .list_pgc_timeline(bilibili_upstream::ListPgcTimelineReq {
                cookies: access.into_cookies(),
                r#type: match timeline_type {
                    BilibiliPgcTimelineType::Anime => {
                        bilibili_upstream::PgcTimelineType::Anime as i32
                    }
                    BilibiliPgcTimelineType::Cinema => {
                        bilibili_upstream::PgcTimelineType::Cinema as i32
                    }
                    BilibiliPgcTimelineType::Guochuang => {
                        bilibili_upstream::PgcTimelineType::Guochuang as i32
                    }
                },
                before_days,
                after_days,
            })
            .await
            .map_err(ProviderError::from)
    }

    fn history_target(
        item: &bilibili_upstream::BilibiliHistoryItem,
    ) -> Result<ProviderTarget, ProviderError> {
        use bilibili_upstream::bilibili_history_item::Target;
        match item.target.as_ref() {
            Some(Target::Video(video)) => Ok(ProviderTarget::bilibili_video_part(
                video.bvid.clone(),
                video.aid,
                video.cid,
                1,
            )),
            Some(Target::Pgc(pgc)) => Ok(ProviderTarget::bilibili_pgc_episode(pgc.epid, pgc.cid)),
            Some(Target::Live(live)) => Ok(ProviderTarget::bilibili_live(live.room_id)),
            None => Err(ProviderError::InvalidConfig(
                "Bilibili history item target is missing".to_string(),
            )),
        }
    }

    fn list_item_target(item: &bilibili_upstream::BilibiliVideoListItem) -> ProviderTarget {
        if item.epid > 0 {
            ProviderTarget::bilibili_pgc_episode(item.epid, item.cid)
        } else if item.part_count > 1 {
            ProviderTarget::bilibili_video(item.bvid.clone(), item.aid)
        } else {
            ProviderTarget::bilibili_video_part(item.bvid.clone(), item.aid, item.cid, 1)
        }
    }

    fn directory_metadata(target: &ProviderTarget) -> Option<PlaybackMetadata> {
        let ProviderTarget::Bilibili(target) = target else {
            return None;
        };
        let metadata = match target {
            BilibiliTarget::Video { bvid, aid } => BilibiliPlaybackMetadata {
                bvid: (!bvid.is_empty()).then(|| bvid.clone()),
                aid: (*aid > 0).then_some(*aid),
                ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video)
            },
            BilibiliTarget::VideoPart { bvid, aid, cid, .. } => BilibiliPlaybackMetadata {
                bvid: (!bvid.is_empty()).then(|| bvid.clone()),
                aid: (*aid > 0).then_some(*aid),
                cid: (*cid > 0).then_some(*cid),
                ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video)
            },
            BilibiliTarget::PgcEpisode { epid, cid } => BilibiliPlaybackMetadata {
                epid: (*epid > 0).then_some(*epid),
                cid: (*cid > 0).then_some(*cid),
                ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Pgc)
            },
            BilibiliTarget::Live { room_id } => BilibiliPlaybackMetadata {
                room_id: (*room_id > 0).then_some(*room_id),
                is_live: true,
                is_currently_live: Some(true),
                ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Live)
            },
        };
        Some(PlaybackMetadata::Bilibili(metadata))
    }

    fn media_config_for_target(
        config: &BilibiliPlaylistSourceConfig,
        target: &BilibiliTarget,
    ) -> Result<MediaSourceConfig, ProviderError> {
        match target {
            BilibiliTarget::Video { .. } => Err(ProviderError::InvalidConfig(
                "Bilibili video node requires selecting a part".to_string(),
            )),
            BilibiliTarget::VideoPart { bvid, aid, cid, .. } => Ok(MediaSourceConfig::Bilibili(
                BilibiliSourceConfig::Video(crate::models::BilibiliVideoSourceConfig {
                    bvid: (!bvid.is_empty()).then(|| bvid.clone()),
                    aid: (*aid > 0).then_some(*aid),
                    cid: *cid,
                    shared: config.shared,
                    proxy_mode: config.proxy_mode,
                }),
            )),
            BilibiliTarget::PgcEpisode { epid, cid } => Ok(MediaSourceConfig::Bilibili(
                BilibiliSourceConfig::Pgc(crate::models::BilibiliPgcSourceConfig {
                    epid: *epid,
                    cid: *cid,
                    shared: config.shared,
                    proxy_mode: config.proxy_mode,
                }),
            )),
            BilibiliTarget::Live { room_id } => Ok(MediaSourceConfig::Bilibili(
                BilibiliSourceConfig::Live(crate::models::BilibiliLiveSourceConfig {
                    room_id: *room_id,
                    shared: config.shared,
                    proxy_mode: config.proxy_mode,
                }),
            )),
        }
    }

    fn directory_source_config_for_target(
        config: &BilibiliPlaylistSourceConfig,
        target: &ProviderTarget,
    ) -> Result<DynamicPlaylistItemSourceConfig, ProviderError> {
        let ProviderTarget::Bilibili(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili target is required".to_string(),
            ));
        };
        match target {
            BilibiliTarget::Video { bvid, aid } => Ok(DynamicPlaylistItemSourceConfig::Playlist(
                PlaylistSourceConfig::Bilibili(BilibiliPlaylistSourceConfig {
                    source: BilibiliPlaylistSource::VideoParts {
                        bvid: bvid.clone(),
                        aid: (*aid > 0).then_some(*aid),
                    },
                    shared: config.shared,
                    proxy_mode: config.proxy_mode,
                }),
            )),
            BilibiliTarget::VideoPart { .. }
            | BilibiliTarget::PgcEpisode { .. }
            | BilibiliTarget::Live { .. } => Ok(DynamicPlaylistItemSourceConfig::Media(
                Self::media_config_for_target(config, target)?,
            )),
        }
    }

    fn next_play_item(
        config: &BilibiliPlaylistSourceConfig,
        name: String,
        target: ProviderTarget,
    ) -> Result<NextPlayItem, ProviderError> {
        let ProviderTarget::Bilibili(bilibili_target) = &target else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili target is required".to_string(),
            ));
        };
        Ok(NextPlayItem {
            name,
            item_type: ItemType::Media,
            source_config: Self::media_config_for_target(config, bilibili_target)?,
            target,
        })
    }

    async fn first_playable_for_target(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
        name: String,
        target: ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        match &target {
            ProviderTarget::Bilibili(BilibiliTarget::Video { bvid, aid }) => {
                let parts = self.video_parts(ctx, config, bvid, *aid).await?;
                let Some(part) = parts.parts.first() else {
                    return Ok(None);
                };
                let part_target = ProviderTarget::bilibili_video_part(
                    part.bvid.clone(),
                    part.aid,
                    part.cid,
                    part.page,
                );
                let part_name = if parts.parts.len() > 1 {
                    format!("{} - {}", name, part.title)
                } else {
                    name
                };
                Self::next_play_item(config, part_name, part_target).map(Some)
            }
            ProviderTarget::Bilibili(_) => Self::next_play_item(config, name, target).map(Some),
            _ => Err(ProviderError::InvalidConfig(
                "Bilibili target is required".to_string(),
            )),
        }
    }

    async fn next_live_room(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliPlaylistSourceConfig,
        current_room_id: u64,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        const PAGE_SIZE: usize = 50;
        const MAX_SHUFFLE_ITEMS: usize = 200;
        let mut page = 1;
        let mut first: Option<(String, u64)> = None;
        let mut found_current = false;
        let mut shuffle_items = Vec::new();

        loop {
            let response = self
                .list_live_playlist_page(ctx, config, page, PAGE_SIZE)
                .await?;
            for room in response.items {
                if first.is_none() {
                    first = Some((room.title.clone(), room.room_id));
                }
                if play_mode == PlayMode::Shuffle {
                    if room.room_id != current_room_id {
                        shuffle_items.push((room.title, room.room_id));
                    }
                    continue;
                }
                if found_current {
                    return Self::next_play_item(
                        config,
                        room.title,
                        ProviderTarget::bilibili_live(room.room_id),
                    )
                    .map(Some);
                }
                found_current = room.room_id == current_room_id;
            }
            if !response.has_more
                || (play_mode == PlayMode::Shuffle && shuffle_items.len() >= MAX_SHUFFLE_ITEMS)
            {
                break;
            }
            page = page.saturating_add(1);
        }

        if play_mode == PlayMode::Shuffle {
            shuffle_items.truncate(MAX_SHUFFLE_ITEMS);
            let selected = {
                let mut rng = rand::rng();
                shuffle_items.choose(&mut rng).cloned()
            };
            return match selected {
                Some((name, room_id)) => {
                    Self::next_play_item(config, name, ProviderTarget::bilibili_live(room_id))
                        .map(Some)
                }
                None => Ok(None),
            };
        }
        if found_current && play_mode == PlayMode::RepeatAll {
            if let Some((name, room_id)) = first {
                return Self::next_play_item(config, name, ProviderTarget::bilibili_live(room_id))
                    .map(Some);
            }
        }
        Ok(None)
    }

    fn root_target_matches(current: &BilibiliTarget, candidate: &ProviderTarget) -> bool {
        let ProviderTarget::Bilibili(candidate) = candidate else {
            return false;
        };
        match (current, candidate) {
            (
                BilibiliTarget::Video { bvid, aid },
                BilibiliTarget::Video {
                    bvid: other_bvid,
                    aid: other_aid,
                }
                | BilibiliTarget::VideoPart {
                    bvid: other_bvid,
                    aid: other_aid,
                    ..
                },
            )
            | (
                BilibiliTarget::VideoPart { bvid, aid, .. },
                BilibiliTarget::Video {
                    bvid: other_bvid,
                    aid: other_aid,
                }
                | BilibiliTarget::VideoPart {
                    bvid: other_bvid,
                    aid: other_aid,
                    ..
                },
            ) => (!bvid.is_empty() && bvid == other_bvid) || (*aid > 0 && *aid == *other_aid),
            (
                BilibiliTarget::PgcEpisode { epid, .. },
                BilibiliTarget::PgcEpisode {
                    epid: other_epid, ..
                },
            ) => epid == other_epid,
            (
                BilibiliTarget::Live { room_id },
                BilibiliTarget::Live {
                    room_id: other_room_id,
                },
            ) => room_id == other_room_id,
            _ => false,
        }
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

    fn playback_proxy_policy(
        &self,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<PlaybackProxyPolicy>, ProviderError> {
        bilibili_playback_proxy_policy(source_config).map(Some)
    }

    fn set_playback_proxy_mode(
        &self,
        source_config: &mut MediaSourceConfig,
        mode: crate::models::PlaybackProxyMode,
    ) -> Result<(), ProviderError> {
        let MediaSourceConfig::Bilibili(config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili requires Bilibili media source_config".to_string(),
            ));
        };
        match config {
            BilibiliSourceConfig::Video(config) => config.proxy_mode = mode,
            BilibiliSourceConfig::Pgc(config) => config.proxy_mode = mode,
            BilibiliSourceConfig::Live(config) => config.proxy_mode = mode,
        }
        Ok(())
    }

    async fn media_metadata(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<Option<PlaybackMetadata>, ProviderError> {
        let config = BilibiliSourceConfig::from_media_config(source_config)?;
        let (cache_key, cache_ttl) = match &config {
            BilibiliSourceConfig::Video(config) => {
                let identifier =
                    BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?;
                (
                    format!("video:{}:{}", identifier.cache_key_part(), config.cid),
                    Duration::from_hours(2),
                )
            }
            BilibiliSourceConfig::Pgc(config) => (
                format!("pgc:{}:{}", config.epid, config.cid),
                Duration::from_hours(2),
            ),
            BilibiliSourceConfig::Live(config) => (
                format!("live:{}:{}", config.room_id, config.shared),
                Duration::from_secs(15),
            ),
        };

        super::cached_provider_metadata_or_fill(
            Self::NAME,
            &cache_key,
            cache_ttl,
            ctx,
            || async move {
                let metadata = match config {
                    BilibiliSourceConfig::Video(config) => {
                        let (bvid, aid) =
                            resolve_bilibili_video_identifier(config.bvid.as_deref(), config.aid)?;
                        BilibiliPlaybackMetadata {
                            bvid,
                            aid: Some(aid),
                            cid: Some(config.cid),
                            ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video)
                        }
                    }
                    BilibiliSourceConfig::Pgc(config) => BilibiliPlaybackMetadata {
                        epid: Some(config.epid),
                        cid: Some(config.cid),
                        ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Pgc)
                    },
                    BilibiliSourceConfig::Live(config) => {
                        let credential_user_id = bilibili_optional_credential_user_id(
                            ctx,
                            ProviderCredentialPolicy::from_shared(config.shared),
                        )?;
                        let (cookies, _) =
                            resolve_optional_bilibili_cookies(ctx, credential_user_id).await?;
                        let client = self
                            .get_client_with_context(
                                super::bound_provider_instance_name(ctx),
                                ctx.request_context(),
                            )
                            .await?;
                        let page = client
                            .parse_live_page(bilibili_parse_live_page_request(
                                BilibiliParseLivePageRequest {
                                    cookies,
                                    room_id: config.room_id,
                                },
                            ))
                            .await?;
                        BilibiliPlaybackMetadata {
                            room_id: Some(config.room_id),
                            live_started_at: page.live_started_at,
                            is_live: true,
                            is_currently_live: Some(page.is_currently_live),
                            ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Live)
                        }
                    }
                };
                Ok(Some(PlaybackMetadata::Bilibili(metadata)))
            },
        )
        .await
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = BilibiliSourceConfig::from_media_config(source_config)?;

        let credential_user_id = bilibili_optional_credential_user_id(
            _ctx,
            ProviderCredentialPolicy::from_shared(config.shared()),
        )?;
        let (cookies, credential_cache_partition) =
            resolve_optional_bilibili_cookies(_ctx, credential_user_id).await?;

        let (cache_key, cache_ttl) = playback_cache_entry(
            config,
            &credential_cache_partition,
            _ctx.playback_client_profile(),
        )?;
        let proxy_mode = config.proxy_mode();

        let result = Box::pin(super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            |result, version, expires_at| {
                mark_bilibili_playback_resources(
                    result,
                    version,
                    expires_at,
                    proxy_mode,
                    Some(_ctx),
                );
            },
            || async {
                self.resolve_from_api_with_cookies(
                    _ctx,
                    config,
                    &cookies,
                    super::bound_provider_instance_name(_ctx),
                    _ctx.request_context(),
                )
                .await
            },
        ))
        .await?;
        super::require_compatible_playback_route(result, proxy_mode, _ctx.playback_client_profile())
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let SourceConfig::Media(media_config) = source_config else {
            let SourceConfig::DynamicPlaylist(playlist_config) = source_config else {
                unreachable!();
            };
            let config = Self::playlist_config(playlist_config)?;
            match &config.source {
                BilibiliPlaylistSource::VideoParts { bvid, aid }
                    if bvid.trim().is_empty() && aid.is_none_or(|aid| aid == 0) =>
                {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili video parts require a bvid or aid".to_string(),
                    ));
                }
                BilibiliPlaylistSource::UpVideos { mid, .. } if *mid == 0 => {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili UP playlist mid must be non-zero".to_string(),
                    ));
                }
                BilibiliPlaylistSource::FavoriteVideos { media_id } if *media_id == 0 => {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili favorite playlist media_id must be non-zero".to_string(),
                    ));
                }
                BilibiliPlaylistSource::CollectionVideos { mid, season_id }
                    if *mid == 0 || *season_id == 0 =>
                {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili collection playlist mid and season_id must be non-zero"
                            .to_string(),
                    ));
                }
                BilibiliPlaylistSource::SeriesVideos { mid, series_id }
                    if *mid == 0 || *series_id == 0 =>
                {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili series playlist mid and series_id must be non-zero".to_string(),
                    ));
                }
                BilibiliPlaylistSource::PgcSeason { season_id } if *season_id == 0 => {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC season_id must be non-zero".to_string(),
                    ));
                }
                BilibiliPlaylistSource::PgcTimeline {
                    before_days,
                    after_days,
                    ..
                } if *before_days > 7 || *after_days > 7 => {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC timeline day range must be at most seven".to_string(),
                    ));
                }
                _ => {}
            }
            if Self::playlist_requires_credential(&config.source) {
                Self::playlist_access(ctx, config).await?;
            }
            return Ok(());
        };
        let config = BilibiliSourceConfig::from_media_config(media_config)?;

        match &config {
            BilibiliSourceConfig::Video(config) => {
                BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?;
                if config.cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili video cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Pgc(config) => {
                if config.epid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC epid must be non-zero".to_string(),
                    ));
                }
                if config.cid == 0 {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili PGC cid must be non-zero".to_string(),
                    ));
                }
            }
            BilibiliSourceConfig::Live(config) => {
                if config.room_id == 0 {
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
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        bilibili_credential_dependencies(ctx, source_config)
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        match source_config {
            SourceConfig::Media(config) => {
                let config = BilibiliSourceConfig::from_media_config(config)?;
                self.resolve_source_cover(ctx, config).await
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                if let BilibiliPlaylistSource::VideoParts { bvid, aid } = &config.source {
                    let response = self
                        .video_parts(ctx, config, bvid, aid.unwrap_or_default())
                        .await?;
                    return Ok(response
                        .parts
                        .first()
                        .map(|part| part.cover.trim())
                        .filter(|cover| !cover.is_empty())
                        .map(|cover| SourceCover::Url {
                            url: cover.to_string(),
                        }));
                }
                if matches!(
                    config.source,
                    BilibiliPlaylistSource::LiveRecommended
                        | BilibiliPlaylistSource::LiveFollowed
                        | BilibiliPlaylistSource::LiveArea { .. }
                ) {
                    let response = self.list_live_playlist_page(ctx, config, 1, 1).await?;
                    return Ok(response
                        .items
                        .first()
                        .map(|item| item.cover.trim())
                        .filter(|cover| !cover.is_empty())
                        .map(|cover| SourceCover::Url {
                            url: cover.to_string(),
                        }));
                }
                if matches!(config.source, BilibiliPlaylistSource::History { .. }) {
                    let response = self.list_history_page(ctx, config, None, 1).await?;
                    return Ok(response
                        .items
                        .first()
                        .map(|item| item.cover.trim())
                        .filter(|cover| !cover.is_empty())
                        .map(|cover| SourceCover::Url {
                            url: cover.to_string(),
                        }));
                }
                if matches!(config.source, BilibiliPlaylistSource::PgcTimeline { .. }) {
                    let response = self.list_pgc_timeline(ctx, config).await?;
                    return Ok(response
                        .items
                        .iter()
                        .find(|item| item.published && item.cid > 0)
                        .map(|item| item.episode_cover.trim())
                        .filter(|cover| !cover.is_empty())
                        .map(|cover| SourceCover::Url {
                            url: cover.to_string(),
                        }));
                }
                let response = self.list_playlist_page(ctx, config, 1, 1).await?;
                Ok(response
                    .items
                    .first()
                    .map(|item| item.cover.trim())
                    .filter(|cover| !cover.is_empty())
                    .map(|cover| SourceCover::Url {
                        url: cover.to_string(),
                    }))
            }
        }
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<PreparedSourceConfig, ProviderError> {
        match source_config {
            SourceConfig::Media(config) => {
                BilibiliSourceConfig::from_media_config(config)?;
            }
            SourceConfig::DynamicPlaylist(config) => {
                Self::playlist_config(config)?;
            }
        }
        Ok(source_config.into())
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    fn as_bilibili_live_danmaku_provider(&self) -> Option<&dyn super::BilibiliLiveDanmakuProvider> {
        Some(self)
    }
}

impl BilibiliProvider {
    pub async fn discover_playlist_with_context(
        &self,
        ctx: &ProviderContext<'_>,
        config: BilibiliPlaylistSourceConfig,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let now = chrono::Utc::now();
        let playlist = Playlist {
            id: PlaylistId::new(),
            room_id: RoomId::new(),
            creator_id: ctx.credential_owner_id().copied(),
            browse_access_mode: crate::models::PlaylistBrowseAccessMode::Default,
            name: "Bilibili discovery".to_string(),
            description: String::new(),
            cover_file_reference_id: None,
            parent_id: None,
            position: 0.0,
            source_provider: Some(SourceProvider::Bilibili),
            source_config: Some(PlaylistSourceConfig::Bilibili(config)),
            provider_instance_name: ctx.provider_instance_name().map(str::to_string),
            created_at: now,
            updated_at: now,
            version: 1,
        };
        <Self as DynamicPlaylistProvider>::list_playlist(self, ctx, &playlist, None, query).await
    }
}

#[async_trait]
impl DynamicPlaylistProvider for BilibiliProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let source_config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let config = Self::playlist_config(source_config)?;
        if matches!(config.source, BilibiliPlaylistSource::History { .. }) {
            if target.is_some() {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili history has a flat root".to_string(),
                ));
            }
            if query
                .search
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili history does not expose server-side search".to_string(),
                ));
            }
            let cursor = match &query.pagination {
                DynamicPagination::Cursor { cursor } => cursor.as_deref(),
                DynamicPagination::Page { page: 1 } => None,
                DynamicPagination::Page { .. } => {
                    return Err(ProviderError::InvalidConfig(
                        "Bilibili history requires cursor pagination after the first page"
                            .to_string(),
                    ));
                }
            };
            let response = self
                .list_history_page(ctx, config, cursor, query.page_size.clamp(1, 30))
                .await?;
            let items = response
                .items
                .into_iter()
                .map(|item| {
                    let target = Self::history_target(&item)?;
                    let description = [
                        (!item.author.trim().is_empty()).then_some(item.author),
                        (!item.subtitle.trim().is_empty()).then_some(item.subtitle),
                        (item.progress_seconds >= 0).then(|| {
                            format!("{}s / {}s", item.progress_seconds, item.duration_seconds)
                        }),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    Ok(DynamicPlaylistItem {
                        name: item.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target: target.clone(),
                        size: None,
                        thumbnail: (!item.cover.trim().is_empty())
                            .then_some(DynamicPlaylistItemThumbnail::Url(item.cover)),
                        description: (!description.is_empty()).then_some(description),
                        modified_at: (item.viewed_at > 0).then_some(item.viewed_at),
                        metadata: Self::directory_metadata(&target),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                items,
                pagination: DynamicPagination::Cursor {
                    cursor: Self::encode_history_cursor(response.cursor)?,
                },
                has_more: response.has_more,
                supports_search: false,
            });
        }
        let DynamicPagination::Page { page } = query.pagination else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili uses page pagination".to_string(),
            ));
        };
        let page = page.max(1);
        let page_size = query.page_size.clamp(1, 50);

        if matches!(config.source, BilibiliPlaylistSource::PgcTimeline { .. }) {
            if target.is_some() {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili PGC timeline has a flat root".to_string(),
                ));
            }
            let response = self.list_pgc_timeline(ctx, config).await?;
            let playable = response
                .items
                .into_iter()
                .filter(|item| item.published && item.cid > 0)
                .collect::<Vec<_>>();
            let start = page.saturating_sub(1).saturating_mul(page_size);
            let total = playable.len();
            let items = playable
                .into_iter()
                .skip(start)
                .take(page_size)
                .map(|item| {
                    let target = ProviderTarget::bilibili_pgc_episode(item.episode_id, item.cid);
                    let name = if item.episode_title.trim().is_empty() {
                        item.title
                    } else {
                        format!("{} - {}", item.title, item.episode_title)
                    };
                    let description = [
                        (!item.date.trim().is_empty()).then_some(item.date),
                        item.delayed.then_some(item.delay_reason),
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|value| !value.trim().is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ");
                    let cover = if item.episode_cover.trim().is_empty() {
                        item.cover
                    } else {
                        item.episode_cover
                    };
                    Ok(DynamicPlaylistItem {
                        name,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target: target.clone(),
                        size: None,
                        thumbnail: (!cover.trim().is_empty())
                            .then_some(DynamicPlaylistItemThumbnail::Url(cover)),
                        description: (!description.is_empty()).then_some(description),
                        modified_at: (item.publish_at > 0).then_some(item.publish_at),
                        metadata: Self::directory_metadata(&target),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                has_more: start.saturating_add(items.len()) < total,
                items,
                pagination: DynamicPagination::Page { page },
                supports_search: false,
            });
        }

        if matches!(
            &config.source,
            BilibiliPlaylistSource::LiveRecommended
                | BilibiliPlaylistSource::LiveFollowed
                | BilibiliPlaylistSource::LiveArea { .. }
        ) {
            if target.is_some() {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili live playlists have a flat root".to_string(),
                ));
            }
            let response = self
                .list_live_playlist_page(ctx, config, page, page_size)
                .await?;
            let items = response
                .items
                .into_iter()
                .map(|room| {
                    let target = ProviderTarget::bilibili_live(room.room_id);
                    let description = [
                        (!room.author.trim().is_empty()).then_some(room.author),
                        (!room.parent_area_name.trim().is_empty()).then_some(room.parent_area_name),
                        (!room.area_name.trim().is_empty()).then_some(room.area_name),
                        (room.online > 0).then(|| format!("{} online", room.online)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    Ok(DynamicPlaylistItem {
                        name: room.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target: target.clone(),
                        size: None,
                        thumbnail: (!room.cover.trim().is_empty())
                            .then_some(DynamicPlaylistItemThumbnail::Url(room.cover)),
                        description: (!description.is_empty()).then_some(description),
                        modified_at: None,
                        metadata: Self::directory_metadata(&target),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                items,
                pagination: DynamicPagination::Page { page },
                has_more: response.has_more,
                supports_search: false,
            });
        }

        if let BilibiliPlaylistSource::VideoParts { bvid, aid } = &config.source {
            if target.is_some() {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili video-parts playlists have a flat root".to_string(),
                ));
            }
            let response = self
                .video_parts(ctx, config, bvid, aid.unwrap_or_default())
                .await?;
            let start = page.saturating_sub(1).saturating_mul(page_size);
            let total = response.parts.len();
            let items = response
                .parts
                .into_iter()
                .skip(start)
                .take(page_size)
                .map(|part| {
                    let target = ProviderTarget::bilibili_video_part(
                        part.bvid, part.aid, part.cid, part.page,
                    );
                    let metadata = Self::directory_metadata(&target);
                    Ok(DynamicPlaylistItem {
                        name: part.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target: target.clone(),
                        size: None,
                        thumbnail: (!part.cover.trim().is_empty())
                            .then_some(DynamicPlaylistItemThumbnail::Url(part.cover)),
                        description: Some(format!(
                            "{}x{} · {}s",
                            part.width, part.height, part.duration_seconds
                        )),
                        modified_at: None,
                        metadata,
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                has_more: start.saturating_add(items.len()) < total,
                items,
                pagination: DynamicPagination::Page { page },
                supports_search: false,
            });
        }

        if let Some(target) = target {
            let ProviderTarget::Bilibili(BilibiliTarget::Video { bvid, aid }) = target else {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili browse target must be a video node".to_string(),
                ));
            };
            let response = self.video_parts(ctx, config, bvid, *aid).await?;
            let start = page.saturating_sub(1).saturating_mul(page_size);
            let total = response.parts.len();
            let items = response
                .parts
                .into_iter()
                .skip(start)
                .take(page_size)
                .map(|part| {
                    let target = ProviderTarget::bilibili_video_part(
                        part.bvid, part.aid, part.cid, part.page,
                    );
                    Ok(DynamicPlaylistItem {
                        name: part.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target: target.clone(),
                        size: None,
                        thumbnail: (!part.cover.trim().is_empty())
                            .then_some(DynamicPlaylistItemThumbnail::Url(part.cover)),
                        description: Some(format!(
                            "{}x{} · {}s",
                            part.width, part.height, part.duration_seconds
                        )),
                        modified_at: None,
                        metadata: Self::directory_metadata(&target),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                has_more: start.saturating_add(items.len()) < total,
                items,
                pagination: DynamicPagination::Page { page },
                supports_search: false,
            });
        }

        let mut effective_config = config.clone();
        if let (BilibiliPlaylistSource::UpVideos { keyword, .. }, Some(search)) =
            (&mut effective_config.source, query.search)
        {
            *keyword = search;
        }
        let response = self
            .list_playlist_page(ctx, &effective_config, page, page_size)
            .await?;
        let items = response
            .items
            .into_iter()
            .map(|item| {
                let target = Self::list_item_target(&item);
                let item_type = if item.epid == 0 && item.part_count > 1 {
                    ItemType::Playlist
                } else {
                    ItemType::Media
                };
                Ok(DynamicPlaylistItem {
                    name: item.title,
                    item_type,
                    source_config: Some(Self::directory_source_config_for_target(config, &target)?),
                    target: target.clone(),
                    size: None,
                    thumbnail: (!item.cover.trim().is_empty())
                        .then_some(DynamicPlaylistItemThumbnail::Url(item.cover)),
                    description: (!item.description.trim().is_empty()).then_some(item.description),
                    modified_at: (item.published_at > 0).then_some(item.published_at),
                    metadata: Self::directory_metadata(&target),
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        Ok(DynamicListResult {
            items,
            pagination: DynamicPagination::Page { page },
            has_more: response.has_more,
            supports_search: matches!(&config.source, BilibiliPlaylistSource::UpVideos { .. }),
        })
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let source_config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let config = Self::playlist_config(source_config)?;
        let ProviderTarget::Bilibili(target_value) = target else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili target is required".to_string(),
            ));
        };
        if matches!(target_value, BilibiliTarget::Video { .. }) {
            return Ok(None);
        }
        Self::next_play_item(config, "Bilibili".to_string(), target.clone()).map(Some)
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        const PAGE_SIZE: usize = 50;

        if play_mode == PlayMode::RepeatOne {
            return Ok(None);
        }
        let source_config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let config = Self::playlist_config(source_config)?;
        let ProviderTarget::Bilibili(current) = target else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili target is required".to_string(),
            ));
        };

        if matches!(
            &config.source,
            BilibiliPlaylistSource::LiveRecommended
                | BilibiliPlaylistSource::LiveFollowed
                | BilibiliPlaylistSource::LiveArea { .. }
        ) {
            let BilibiliTarget::Live { room_id } = current else {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili live playlist requires a live-room target".to_string(),
                ));
            };
            return self.next_live_room(ctx, config, *room_id, play_mode).await;
        }

        if matches!(config.source, BilibiliPlaylistSource::History { .. }) {
            const PAGE_SIZE: usize = 30;
            const MAX_SHUFFLE_ITEMS: usize = 200;
            let mut cursor = None;
            let mut first: Option<DynamicPlaylistItem> = None;
            let mut found_current = false;
            let mut candidates = Vec::new();
            loop {
                let result = self
                    .list_playlist(
                        ctx,
                        playlist,
                        None,
                        DynamicListQuery {
                            pagination: DynamicPagination::Cursor { cursor },
                            page_size: PAGE_SIZE,
                            ..DynamicListQuery::default()
                        },
                    )
                    .await?;
                let next_cursor = match &result.pagination {
                    DynamicPagination::Cursor { cursor } if result.has_more => cursor.clone(),
                    _ => None,
                };
                for item in result.items {
                    first.get_or_insert_with(|| item.clone());
                    if play_mode == PlayMode::Shuffle {
                        if !Self::root_target_matches(current, &item.target) {
                            candidates.push(item);
                        }
                        continue;
                    }
                    if found_current {
                        return self
                            .first_playable_for_target(ctx, config, item.name, item.target)
                            .await;
                    }
                    found_current = Self::root_target_matches(current, &item.target);
                }
                if play_mode == PlayMode::Shuffle && candidates.len() >= MAX_SHUFFLE_ITEMS {
                    break;
                }
                let Some(next_cursor) = next_cursor else {
                    break;
                };
                cursor = Some(next_cursor);
            }
            if play_mode == PlayMode::Shuffle {
                candidates.truncate(MAX_SHUFFLE_ITEMS);
                let selected = {
                    let mut rng = rand::rng();
                    candidates.choose(&mut rng).cloned()
                };
                return match selected {
                    Some(item) => {
                        self.first_playable_for_target(ctx, config, item.name, item.target)
                            .await
                    }
                    None => Ok(None),
                };
            }
            if found_current && play_mode == PlayMode::RepeatAll {
                if let Some(first) = first {
                    return self
                        .first_playable_for_target(ctx, config, first.name, first.target)
                        .await;
                }
            }
            return Ok(None);
        }

        if matches!(config.source, BilibiliPlaylistSource::PgcTimeline { .. }) {
            const PAGE_SIZE: usize = 50;
            let mut page = 1;
            let mut first: Option<DynamicPlaylistItem> = None;
            let mut found_current = false;
            let mut candidates = Vec::new();
            loop {
                let result = self
                    .list_playlist(
                        ctx,
                        playlist,
                        None,
                        DynamicListQuery {
                            pagination: DynamicPagination::Page { page },
                            page_size: PAGE_SIZE,
                            ..DynamicListQuery::default()
                        },
                    )
                    .await?;
                for item in result.items {
                    first.get_or_insert_with(|| item.clone());
                    if play_mode == PlayMode::Shuffle {
                        if !Self::root_target_matches(current, &item.target) {
                            candidates.push(item);
                        }
                    } else if found_current {
                        return self
                            .first_playable_for_target(ctx, config, item.name, item.target)
                            .await;
                    } else {
                        found_current = Self::root_target_matches(current, &item.target);
                    }
                }
                if !result.has_more {
                    break;
                }
                page = page.saturating_add(1);
            }
            if play_mode == PlayMode::Shuffle {
                let selected = {
                    let mut rng = rand::rng();
                    candidates.choose(&mut rng).cloned()
                };
                return match selected {
                    Some(item) => {
                        self.first_playable_for_target(ctx, config, item.name, item.target)
                            .await
                    }
                    None => Ok(None),
                };
            }
            if found_current && play_mode == PlayMode::RepeatAll {
                if let Some(first) = first {
                    return self
                        .first_playable_for_target(ctx, config, first.name, first.target)
                        .await;
                }
            }
            return Ok(None);
        }

        if let BilibiliPlaylistSource::VideoParts { bvid, aid } = &config.source {
            let BilibiliTarget::VideoPart { cid, page, .. } = current else {
                return Err(ProviderError::InvalidConfig(
                    "Bilibili video-parts playback requires a part target".to_string(),
                ));
            };
            let response = self
                .video_parts(ctx, config, bvid, aid.unwrap_or_default())
                .await?;
            let current_index = response
                .parts
                .iter()
                .position(|part| part.cid == *cid || part.page == *page)
                .ok_or(ProviderError::NotFound)?;
            let selected = match play_mode {
                PlayMode::Sequential => response.parts.get(current_index + 1),
                PlayMode::RepeatAll => response
                    .parts
                    .get(current_index + 1)
                    .or_else(|| response.parts.first()),
                PlayMode::Shuffle => {
                    let candidates = response
                        .parts
                        .iter()
                        .enumerate()
                        .filter_map(|(index, part)| (index != current_index).then_some(part))
                        .collect::<Vec<_>>();
                    let mut rng = rand::rng();
                    candidates.choose(&mut rng).copied()
                }
                PlayMode::RepeatOne => unreachable!(),
            };
            let Some(part) = selected else {
                return Ok(None);
            };
            let next_target = ProviderTarget::bilibili_video_part(
                part.bvid.clone(),
                part.aid,
                part.cid,
                part.page,
            );
            return Self::next_play_item(config, part.title.clone(), next_target).map(Some);
        }

        if let BilibiliTarget::VideoPart {
            bvid,
            aid,
            cid,
            page,
        } = current
        {
            let parts = self.video_parts(ctx, config, bvid, *aid).await?;
            if let Some(next_part) = parts.parts.iter().find(|part| {
                part.page > *page || (part.page == page.saturating_add(1) && part.cid != *cid)
            }) {
                let next_target = ProviderTarget::bilibili_video_part(
                    next_part.bvid.clone(),
                    next_part.aid,
                    next_part.cid,
                    next_part.page,
                );
                return Self::next_play_item(config, next_part.title.clone(), next_target)
                    .map(Some);
            }
        }

        if play_mode == PlayMode::Shuffle {
            const MAX_ITEMS: usize = 200;
            let mut candidates = Vec::new();
            let mut page = 1;
            loop {
                let response = self
                    .list_playlist_page(ctx, config, page, PAGE_SIZE)
                    .await?;
                candidates.extend(response.items.into_iter().map(|item| {
                    let target = Self::list_item_target(&item);
                    (item.title, target)
                }));
                if !response.has_more || candidates.len() >= MAX_ITEMS {
                    break;
                }
                page = page.saturating_add(1);
            }
            candidates.truncate(MAX_ITEMS);
            let selected = {
                let mut rng = rand::rng();
                candidates.choose(&mut rng).cloned()
            };
            let Some((name, selected_target)) = selected else {
                return Ok(None);
            };
            return self
                .first_playable_for_target(ctx, config, name, selected_target)
                .await;
        }

        let mut page = 1;
        let mut found_current = false;
        let mut first: Option<(String, ProviderTarget)> = None;
        loop {
            let response = self
                .list_playlist_page(ctx, config, page, PAGE_SIZE)
                .await?;
            for item in response.items {
                let candidate_target = Self::list_item_target(&item);
                if first.is_none() {
                    first = Some((item.title.clone(), candidate_target.clone()));
                }
                if found_current {
                    return self
                        .first_playable_for_target(ctx, config, item.title, candidate_target)
                        .await;
                }
                if Self::root_target_matches(current, &candidate_target) {
                    found_current = true;
                }
            }
            if !response.has_more {
                break;
            }
            page = page.saturating_add(1);
        }

        if found_current && play_mode == PlayMode::RepeatAll {
            if let Some((name, first_target)) = first {
                return self
                    .first_playable_for_target(ctx, config, name, first_target)
                    .await;
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bilibili_dash_label, bilibili_dash_manifest_swarm_id, bilibili_dash_playback_infos,
        bilibili_dash_resource_candidates, bilibili_dash_video_codec, bilibili_durl_media,
        bilibili_durl_resource_candidates, bilibili_live_danmaku_track,
        bilibili_live_playback_infos, bilibili_subtitle_track, bilibili_upstream,
        bilibili_vod_danmaku_track, build_bilibili_durl_manifest, build_bilibili_mpd_manifest,
        default_bilibili_live_mode, filter_bilibili_dash_manifest,
        mark_bilibili_playback_resources, BilibiliDashPlaybackOptions, BilibiliProvider,
        BilibiliSmsLoginSession, BilibiliSmsLoginTokenCodec,
    };
    use crate::models::media::{
        BilibiliDashAudioStream, BilibiliDashManifest, BilibiliDashManifestSlot,
        BilibiliDashManifests, BilibiliDashVideoStream, BilibiliDurlSegment, BilibiliPlaybackKind,
        BilibiliPlaybackMetadata, PlaybackBilibiliDanmaku, PlaybackBilibiliMedia,
        PlaybackBilibiliSubtitle, PlaybackDanmakuProvider, PlaybackMediaProvider, PlaybackMetadata,
        PlaybackSubtitleProvider,
    };
    use crate::models::{BilibiliTarget, PlaylistId, ProviderTarget, RoomId};
    use crate::provider::{
        InMemoryProviderStore, PlaybackAudioCodec, PlaybackClientEnvironment,
        PlaybackClientProfile, PlaybackInfo, PlaybackMediaCapability, PlaybackMediaPipeline,
        PlaybackMediaTransport, PlaybackResult, PlaybackTransportAction, PlaybackVideoCodec,
        ProviderActor, ProviderContext, ProviderStore, ProviderStoreExt, SourceConfig,
        VersionedPlayback, CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn provider_ok<T>(result: Result<T, super::ProviderError>) -> TestResult<T> {
        result.map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    fn web_hls_profile() -> PlaybackClientProfile {
        PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            supported_video_codecs: vec![PlaybackVideoCodec::H264],
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Hls,
                container: None,
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline: PlaybackMediaPipeline::MediaSource,
                codec_string: Some("avc1.42E01E,mp4a.40.2".to_string()),
            }],
            supports_custom_http_headers: false,
            supports_provider_proxy: true,
            ..PlaybackClientProfile::default()
        }
    }

    fn durl_test_result() -> TestResult<PlaybackResult> {
        let media = provider_ok(bilibili_durl_media(
            "MP4",
            [(
                "https://cdn.example/video.mp4?deadline=200".to_string(),
                Vec::new(),
                1_000,
            )],
            "sm3_test_durl_web".to_string(),
        ))?;
        Ok(PlaybackResult {
            playback_infos: HashMap::from([(
                "durl".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![media],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "durl".to_string(),
            provider: crate::models::SourceProvider::Bilibili,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        })
    }

    #[test]
    fn playback_cache_key_uses_the_codec_route_schema_version() -> TestResult {
        let config = super::BilibiliSourceConfig::Video(crate::models::BilibiliVideoSourceConfig {
            bvid: Some("BV1test12345".to_string()),
            aid: None,
            cid: 42,
            shared: false,
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
        });

        let (key, _) = provider_ok(super::playback_cache_entry(&config, "anonymous", None))?;

        assert!(key.starts_with("playback:v9:video:"));
        Ok(())
    }

    #[test]
    fn vod_playback_cache_is_partitioned_by_client_codec_capabilities() -> TestResult {
        let config = super::BilibiliSourceConfig::Video(crate::models::BilibiliVideoSourceConfig {
            bvid: Some("BV1test12345".to_string()),
            aid: None,
            cid: 42,
            shared: false,
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
        });
        let mut high_profile = web_hls_profile();
        high_profile.media_capabilities[0].transport = PlaybackMediaTransport::Dash;
        high_profile.media_capabilities[0].codec_string = Some("avc1.640033,mp4a.40.2".to_string());
        let mut main_profile = high_profile.clone();
        main_profile.media_capabilities[0].codec_string = Some("avc1.64001F,mp4a.40.2".to_string());

        let (high_key, _) = provider_ok(super::playback_cache_entry(
            &config,
            "anonymous",
            Some(&high_profile),
        ))?;
        let (main_key, _) = provider_ok(super::playback_cache_entry(
            &config,
            "anonymous",
            Some(&main_profile),
        ))?;

        assert_ne!(high_key, main_key);
        assert!(!high_key.contains("avc1"));
        Ok(())
    }

    #[test]
    fn anonymous_access_uses_the_canonical_playback_cache_partition() {
        let (cookies, partition) =
            BilibiliProvider::anonymous_access().into_cookies_and_partition();

        assert!(cookies.is_empty());
        assert_eq!(partition, "anonymous");
    }

    #[test]
    fn playback_policy_covers_video_pgc_live_and_mixed_dynamic_playlists() -> TestResult {
        let media_configs = [
            (
                crate::models::MediaSourceConfig::Bilibili(super::BilibiliSourceConfig::Video(
                    crate::models::BilibiliVideoSourceConfig {
                        bvid: Some("BV1test12345".to_string()),
                        aid: None,
                        cid: 1,
                        shared: false,
                        proxy_mode: crate::models::PlaybackProxyMode::Auto,
                    },
                )),
                "video",
            ),
            (
                crate::models::MediaSourceConfig::Bilibili(super::BilibiliSourceConfig::Pgc(
                    crate::models::BilibiliPgcSourceConfig {
                        epid: 2,
                        cid: 3,
                        shared: false,
                        proxy_mode: crate::models::PlaybackProxyMode::Auto,
                    },
                )),
                "pgc",
            ),
            (
                crate::models::MediaSourceConfig::Bilibili(super::BilibiliSourceConfig::Live(
                    crate::models::BilibiliLiveSourceConfig {
                        room_id: 4,
                        shared: false,
                        proxy_mode: crate::models::PlaybackProxyMode::Auto,
                    },
                )),
                "live",
            ),
        ];

        for (config, variant) in &media_configs {
            let policy = provider_ok(super::bilibili_playback_proxy_policy(SourceConfig::media(
                config,
            )))?;
            assert_eq!(policy.auto_policies.len(), 1);
            assert_eq!(policy.auto_policies[0].variant, *variant);
            assert_eq!(
                policy.auto_policies[0].mode,
                crate::models::PlaybackProxyMode::Only
            );
            assert_eq!(
                policy.auto_policies[0].reason,
                crate::provider::PlaybackProxyAutoReason::SignedResource
            );
        }

        let playlist = crate::models::PlaylistSourceConfig::Bilibili(
            crate::models::BilibiliPlaylistSourceConfig {
                source: crate::models::BilibiliPlaylistSource::History {
                    history_type: crate::models::BilibiliHistoryType::All,
                },
                shared: false,
                proxy_mode: crate::models::PlaybackProxyMode::Auto,
            },
        );
        let policy = provider_ok(super::bilibili_playback_proxy_policy(
            SourceConfig::dynamic_playlist(&playlist),
        ))?;
        assert_eq!(
            policy
                .auto_policies
                .iter()
                .map(|policy| policy.variant.as_str())
                .collect::<Vec<_>>(),
            vec!["video", "pgc", "live"]
        );
        assert!(policy.auto_policies.iter().all(|policy| {
            policy.mode == crate::models::PlaybackProxyMode::Only
                && policy.reason == crate::provider::PlaybackProxyAutoReason::SignedResource
        }));
        Ok(())
    }

    fn test_sms_login_secret() -> &'static [u8] {
        b"test-bilibili-sms-login-secret"
    }

    #[tokio::test]
    async fn guest_playback_uses_anonymous_bilibili_access() -> TestResult {
        let viewer_id = crate::models::UserId::expect_positive(7);
        let context = ProviderContext::new("test", ProviderActor::Guest);

        let (cookies, partition) =
            provider_ok(super::resolve_optional_bilibili_cookies(&context, Some(viewer_id)).await)?;

        assert!(cookies.is_empty());
        assert_eq!(partition, "guest-anonymous");
        Ok(())
    }

    #[test]
    fn guest_public_playlist_has_no_credential_dependency() -> TestResult {
        let creator_id = crate::models::UserId::expect_positive(8);
        let context =
            ProviderContext::new("test", ProviderActor::Guest).with_credential_owner_id(creator_id);
        let config = crate::models::PlaylistSourceConfig::Bilibili(
            crate::models::BilibiliPlaylistSourceConfig {
                source: crate::models::BilibiliPlaylistSource::Popular,
                shared: true,
                proxy_mode: crate::models::PlaybackProxyMode::Auto,
            },
        );

        let dependencies = provider_ok(super::bilibili_credential_dependencies(
            &context,
            super::SourceConfig::dynamic_playlist(&config),
        ))?;
        assert!(dependencies.is_empty());
        Ok(())
    }

    #[test]
    fn guest_private_playlist_uses_creator_credential() -> TestResult {
        let creator_id = crate::models::UserId::expect_positive(9);
        let context =
            ProviderContext::new("test", ProviderActor::Guest).with_credential_owner_id(creator_id);
        let config = crate::models::PlaylistSourceConfig::Bilibili(
            crate::models::BilibiliPlaylistSourceConfig {
                source: crate::models::BilibiliPlaylistSource::WatchLater,
                shared: false,
                proxy_mode: crate::models::PlaybackProxyMode::Auto,
            },
        );

        let dependencies = provider_ok(super::bilibili_credential_dependencies(
            &context,
            super::SourceConfig::dynamic_playlist(&config),
        ))?;
        assert_eq!(dependencies.len(), 1);
        assert_eq!(dependencies[0].user_id, creator_id);
        assert!(dependencies[0].requirement.is_required());
        Ok(())
    }

    #[test]
    fn sms_login_session_token_decodes_across_codecs() -> TestResult {
        let codec_one = provider_ok(BilibiliSmsLoginTokenCodec::derive_from(
            test_sms_login_secret(),
        ))?;
        let codec_two = provider_ok(BilibiliSmsLoginTokenCodec::derive_from(
            test_sms_login_secret(),
        ))?;
        let session = BilibiliSmsLoginSession {
            token: "captcha-token".to_string(),
            challenge: "captcha-challenge".to_string(),
            phone: Some("13800000000".to_string()),
            captcha_key: Some("captcha-key".to_string()),
            instance_name: Some("bilibili_remote".to_string()),
            expires_at: crate::SystemClock.now().timestamp() + 60,
        };

        let encoded = provider_ok(codec_one.encode(&session))?;
        assert!(
            !encoded.contains("captcha-token")
                && !encoded.contains("captcha-challenge")
                && !encoded.contains("captcha-key")
                && !encoded.contains("13800000000"),
            "session token must keep Bilibili SMS login secrets and phone number encrypted"
        );
        let decoded = provider_ok(codec_two.decode(&encoded))?;

        assert_eq!(decoded.token, session.token);
        assert_eq!(decoded.challenge, session.challenge);
        assert_eq!(decoded.phone, session.phone);
        assert_eq!(decoded.captcha_key, session.captcha_key);
        assert_eq!(decoded.instance_name, session.instance_name);
        Ok(())
    }

    #[test]
    fn sms_login_session_token_rejects_tampering_and_expiry() -> TestResult {
        let codec = provider_ok(BilibiliSmsLoginTokenCodec::derive_from(
            test_sms_login_secret(),
        ))?;
        let valid = BilibiliSmsLoginSession {
            token: "captcha-token".to_string(),
            challenge: "captcha-challenge".to_string(),
            phone: None,
            captcha_key: None,
            instance_name: None,
            expires_at: crate::SystemClock.now().timestamp() + 60,
        };
        let expired = BilibiliSmsLoginSession {
            expires_at: crate::SystemClock.now().timestamp() - 1,
            ..valid.clone()
        };

        let encoded = provider_ok(codec.encode(&valid))?;
        let mut tampered = encoded.clone();
        tampered.push('x');

        assert!(codec.decode(&tampered).is_err());
        let expired_token = provider_ok(codec.encode(&expired))?;
        assert!(codec.decode(&expired_token).is_err());
        Ok(())
    }

    #[test]
    fn history_cursor_round_trips_as_an_opaque_cursor() -> TestResult {
        let encoded = provider_ok(BilibiliProvider::encode_history_cursor(Some(
            super::bilibili_upstream::HistoryCursor {
                max: 77,
                view_at: 123_456,
                business: "archive".to_string(),
            },
        )))?
        .ok_or_else(|| anyhow::anyhow!("encoded cursor should exist"))?;
        assert!(!encoded.contains("archive"));

        let decoded = provider_ok(BilibiliProvider::decode_history_cursor(Some(&encoded)))?
            .ok_or_else(|| anyhow::anyhow!("decoded cursor should exist"))?;
        assert_eq!(decoded.max, 77);
        assert_eq!(decoded.view_at, 123_456);
        assert_eq!(decoded.business, "archive");
        Ok(())
    }

    #[test]
    fn dynamic_items_keep_provider_owned_metadata() {
        let target = ProviderTarget::Bilibili(BilibiliTarget::Live {
            room_id: 21_292_831,
        });
        let Some(PlaybackMetadata::Bilibili(metadata)) =
            BilibiliProvider::directory_metadata(&target)
        else {
            panic!("expected Bilibili metadata");
        };
        assert_eq!(metadata.kind, BilibiliPlaybackKind::Live);
        assert_eq!(metadata.room_id, Some(21_292_831));
    }

    #[test]
    fn live_playback_prefers_the_main_route() {
        let playback_info = || super::PlaybackInfo {
            thumbnail: None,
            medias: Vec::new(),
            default_media_index: None,
            subtitles: Vec::new(),
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        };
        let playback_infos = [
            ("backup_1".to_string(), playback_info()),
            ("main".to_string(), playback_info()),
        ]
        .into_iter()
        .collect();

        assert_eq!(default_bilibili_live_mode(&playback_infos), "main");
    }

    #[test]
    fn dynamic_live_playback_has_a_live_danmaku_track() {
        let room_id = RoomId::new();
        let playlist_id = PlaylistId::new();
        let context = ProviderContext::new("test", ProviderActor::Guest)
            .with_room_id(room_id)
            .with_playlist_id(playlist_id);
        let track = bilibili_live_danmaku_track(&context, 21_292_831)
            .expect("dynamic live playback should expose a danmaku track");

        assert!(matches!(
            track.provider,
            crate::models::media::PlaybackDanmakuProvider::Bilibili(
                crate::models::media::PlaybackBilibiliDanmaku::DynamicLive {
                    room_id: track_room_id,
                    playlist_id: track_playlist_id,
                    live_room_id: 21_292_831,
                }
            ) if track_room_id == room_id && track_playlist_id == playlist_id
        ));
    }

    #[test]
    fn cached_dynamic_live_playback_attaches_danmaku_to_proxy_route() {
        let room_id = RoomId::new();
        let playlist_id = PlaylistId::new();
        let context = ProviderContext::new("test", ProviderActor::Guest)
            .with_room_id(room_id)
            .with_playlist_id(playlist_id);
        let mut result = PlaybackResult {
            playback_infos: HashMap::from([(
                "main".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![super::playback_media(
                        "Live HLS".to_string(),
                        "m3u8".to_string(),
                        Some(456),
                        None,
                        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct {
                            url: "https://live.example.com/main.m3u8".to_string(),
                            headers: HashMap::new(),
                        }),
                    )],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "main".to_string(),
            provider: crate::models::SourceProvider::Bilibili,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Live),
            metadata: Some(PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                room_id: Some(21_292_831),
                is_live: true,
                is_currently_live: Some(true),
                ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Live)
            })),
        };

        mark_bilibili_playback_resources(
            &mut result,
            "version",
            123,
            crate::models::PlaybackProxyMode::Auto,
            Some(&context),
        );

        let info = &result.playback_infos["proxy_main"];
        assert_eq!(info.default_danmaku_index, Some(0));
        assert!(matches!(
            info.danmakus.as_slice(),
            [crate::models::media::PlaybackDanmaku {
                provider: PlaybackDanmakuProvider::Bilibili(
                    PlaybackBilibiliDanmaku::DynamicLive {
                        room_id: track_room_id,
                        playlist_id: track_playlist_id,
                        live_room_id: 21_292_831,
                    }
                ),
                ..
            }] if *track_room_id == room_id && *track_playlist_id == playlist_id
        ));
    }

    #[test]
    fn live_playback_uses_the_transport_advertised_by_the_client() -> TestResult {
        let hls_profile = super::super::PlaybackClientProfile::default();
        assert!(super::bilibili_live_uses_hls(Some(&hls_profile)));

        let native_profile = super::super::PlaybackClientProfile {
            supported_live_transports: vec![super::super::PlaybackLiveTransport::Flv],
            ..Default::default()
        };
        assert!(!super::bilibili_live_uses_hls(Some(&native_profile)));

        let web_flv_profile = super::super::PlaybackClientProfile {
            profile_version: super::super::CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: super::super::PlaybackClientEnvironment::Web,
            media_capabilities: vec![super::super::PlaybackMediaCapability {
                transport: super::super::PlaybackMediaTransport::Flv,
                container: None,
                video_codec: Some(super::super::PlaybackVideoCodec::H264),
                audio_codec: Some(super::super::PlaybackAudioCodec::Aac),
                pipeline: super::super::PlaybackMediaPipeline::MediaSource,
                codec_string: Some("avc1.42E01E,mp4a.40.2".to_string()),
            }],
            supported_live_transports: Vec::new(),
            ..Default::default()
        };
        assert!(!super::bilibili_live_uses_hls(Some(&web_flv_profile)));

        let config = super::BilibiliSourceConfig::Live(crate::models::BilibiliLiveSourceConfig {
            room_id: 42,
            shared: false,
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
        });
        let (hls_key, _) = provider_ok(super::playback_cache_entry(
            &config,
            "anonymous",
            Some(&hls_profile),
        ))?;
        let (flv_key, _) = provider_ok(super::playback_cache_entry(
            &config,
            "anonymous",
            Some(&native_profile),
        ))?;
        let (web_flv_key, _) = provider_ok(super::playback_cache_entry(
            &config,
            "anonymous",
            Some(&web_flv_profile),
        ))?;
        assert!(hls_key.ends_with(":hls"));
        assert!(flv_key.ends_with(":flv"));
        assert!(web_flv_key.ends_with(":flv"));
        assert_ne!(hls_key, flv_key);
        Ok(())
    }

    #[test]
    fn durl_segments_are_exposed_as_one_ordered_vod_manifest() -> TestResult {
        let media = provider_ok(bilibili_durl_media(
            "MP4",
            [
                (
                    "https://cdn.example/part-1.mp4?deadline=200".to_string(),
                    vec!["https://backup.example/part-1.mp4?deadline=75".to_string()],
                    1_250,
                ),
                (
                    "https://cdn.example/part-2.mp4?deadline=100".to_string(),
                    Vec::new(),
                    2_500,
                ),
            ],
            "sm3_test_durl".to_string(),
        ))?;
        let PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDurlManifest {
            segments,
            ..
        }) = &media.provider
        else {
            anyhow::bail!("DURL media should use a direct in-memory manifest");
        };

        let manifest = provider_ok(build_bilibili_durl_manifest(segments))?;
        assert_eq!(media.expire_at.map(|value| value.timestamp()), Some(75));
        assert_eq!(
            bilibili_durl_resource_candidates(
                &media,
                "https://cdn.example/part-1.mp4?deadline=200"
            ),
            None
        );
        let restored: crate::models::PlaybackMedia =
            serde_json::from_value(serde_json::to_value(&media)?)?;
        assert_eq!(media.p2p_swarm_id.as_deref(), Some("sm3_test_durl"));
        assert_eq!(restored.p2p_swarm_id, media.p2p_swarm_id);
        assert!(matches!(
            restored.provider,
            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDurlManifest { .. })
        ));
        assert_eq!(manifest.matches("#EXTINF:").count(), 2);
        assert_eq!(manifest.matches("#EXT-X-DISCONTINUITY").count(), 1);
        assert!(manifest.contains("#EXT-X-TARGETDURATION:3"));
        assert!(manifest.contains("#EXTINF:1.250,\nhttps://cdn.example/part-1.mp4"));
        assert!(manifest.contains("#EXTINF:2.500,\nhttps://cdn.example/part-2.mp4"));
        assert!(manifest.ends_with("#EXT-X-ENDLIST\n"));
        Ok(())
    }

    #[test]
    fn durl_media_expiry_is_derived_only_from_upstream_urls() -> TestResult {
        let undated = provider_ok(bilibili_durl_media(
            "MP4",
            [(
                "https://cdn.example/video.mp4".to_string(),
                Vec::new(),
                1_000,
            )],
            "sm3_test_durl_undated".to_string(),
        ))?;
        assert_eq!(undated.expire_at, None);

        let upstream_expires_at = 4_102_444_800;
        let dated = provider_ok(bilibili_durl_media(
            "MP4",
            [(
                format!("https://cdn.example/video.mp4?deadline={upstream_expires_at}"),
                Vec::new(),
                1_000,
            )],
            "sm3_test_durl_dated".to_string(),
        ))?;
        assert_eq!(
            dated.expire_at.map(|value| value.timestamp()),
            Some(upstream_expires_at)
        );
        Ok(())
    }

    #[test]
    fn dash_resources_keep_server_side_backup_candidates() -> TestResult {
        let primary = "https://primary.example/video.m4s?deadline=200";
        let backup = "https://backup.example/video.m4s?deadline=200";
        let dash = BilibiliDashManifest {
            video_streams: vec![BilibiliDashVideoStream {
                base_url: primary.to_string(),
                backup_urls: vec![backup.to_string(), backup.to_string()],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(
            provider_ok(bilibili_dash_resource_candidates(&dash, primary, "", None))?,
            Some(vec![primary.to_string(), backup.to_string()])
        );
        assert_eq!(
            provider_ok(bilibili_dash_resource_candidates(&dash, backup, "", None))?,
            Some(vec![backup.to_string(), primary.to_string()])
        );
        assert_eq!(
            provider_ok(bilibili_dash_resource_candidates(
                &dash,
                "https://unknown.example/video.m4s",
                "",
                None
            ))?,
            None
        );
        Ok(())
    }

    #[test]
    fn durl_manifest_keeps_a_proxy_sibling_for_proxy_only_mode() -> TestResult {
        let media = provider_ok(bilibili_durl_media(
            "MP4",
            [(
                "https://cdn.example/video.mp4?deadline=200".to_string(),
                Vec::new(),
                1_000,
            )],
            "sm3_test_durl_proxy".to_string(),
        ))?;
        let mut result = PlaybackResult {
            playback_infos: HashMap::from([(
                "durl".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![media],
                    default_media_index: Some(0),
                    subtitles: vec![bilibili_subtitle_track(
                        None,
                        "video:BV1test:cid:1",
                        "Chinese",
                        "https://subtitle.bilibili.com/subtitle.json?deadline=200".to_string(),
                    )],
                    default_subtitle_index: Some(0),
                    danmakus: vec![bilibili_vod_danmaku_track(None, "video:BV1test:cid:1", 1)],
                    default_danmaku_index: Some(0),
                },
            )]),
            default_mode: "durl".to_string(),
            provider: crate::models::SourceProvider::Bilibili,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        };

        let direct_only_base = result.clone();
        mark_bilibili_playback_resources(
            &mut result,
            "version",
            123,
            crate::models::PlaybackProxyMode::Auto,
            None,
        );
        assert_eq!(result.default_mode, "proxy_durl");
        assert!(result.playback_infos.contains_key("proxy_durl"));
        assert!(!result.playback_infos.contains_key("durl"));
        assert!(matches!(
            result.playback_infos["proxy_durl"].medias[0].provider,
            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DurlManifest { .. })
        ));
        assert!(matches!(
            result.playback_infos["proxy_durl"].subtitles[0].provider,
            PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Proxy { .. })
        ));
        assert!(matches!(
            result.playback_infos["proxy_durl"].danmakus[0].provider,
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileProxy { .. })
        ));

        let mut direct_only = direct_only_base.clone();
        mark_bilibili_playback_resources(
            &mut direct_only,
            "version",
            123,
            crate::models::PlaybackProxyMode::DirectOnly,
            None,
        );
        assert_eq!(direct_only.default_mode, "durl");
        assert!(matches!(
            direct_only.playback_infos["durl"].medias[0].provider,
            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDurlManifest { .. })
        ));
        assert!(!direct_only.playback_infos.contains_key("proxy_durl"));
        assert!(matches!(
            direct_only.playback_infos["durl"].medias[0].provider,
            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDurlManifest { .. })
        ));

        let mut proxy_only = direct_only_base;
        mark_bilibili_playback_resources(
            &mut proxy_only,
            "version",
            123,
            crate::models::PlaybackProxyMode::Only,
            None,
        );
        assert_eq!(proxy_only.default_mode, "proxy_durl");
        assert!(!proxy_only.playback_infos.contains_key("durl"));
        assert_eq!(proxy_only.playback_infos["proxy_durl"].medias.len(), 1);
        assert!(matches!(
            proxy_only.playback_infos["proxy_durl"].subtitles[0].provider,
            PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Proxy { .. })
        ));
        assert!(matches!(
            proxy_only.playback_infos["proxy_durl"].danmakus[0].provider,
            PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileProxy { .. })
        ));
        Ok(())
    }

    #[test]
    fn web_durl_with_required_headers_only_exposes_the_proxy_route() -> TestResult {
        let profile = web_hls_profile();
        let context = ProviderContext::new("test", ProviderActor::System)
            .with_playback_client_profile(Some(profile));
        let mut result = durl_test_result()?;

        mark_bilibili_playback_resources(
            &mut result,
            "version",
            123,
            crate::models::PlaybackProxyMode::DirectPrefer,
            Some(&context),
        );

        assert_eq!(result.default_mode, "proxy_durl");
        assert!(!result.playback_infos.contains_key("durl"));
        assert!(result.playback_infos.contains_key("proxy_durl"));
        Ok(())
    }

    #[test]
    fn web_direct_only_durl_returns_structured_client_incompatibility() -> TestResult {
        let profile = web_hls_profile();
        let context = ProviderContext::new("test", ProviderActor::System)
            .with_playback_client_profile(Some(profile.clone()));
        let mut result = durl_test_result()?;
        mark_bilibili_playback_resources(
            &mut result,
            "version",
            123,
            crate::models::PlaybackProxyMode::DirectOnly,
            Some(&context),
        );

        let error = super::super::require_compatible_playback_route(
            result,
            crate::models::PlaybackProxyMode::DirectOnly,
            Some(&profile),
        )
        .expect_err("DirectOnly cannot satisfy browser header restrictions");
        assert!(matches!(
            error,
            super::ProviderError::ClientIncompatible {
                required_capability: Some(ref capability),
                ..
            } if capability == "browser_direct_media_access_or_provider_proxy"
        ));
        Ok(())
    }

    #[tokio::test]
    async fn bilibili_subtitle_uses_full_response_cache_transport() -> TestResult {
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(8));
        let subtitle_url = "https://subtitle.bilibili.com/track.json";
        let playback = PlaybackResult {
            playback_infos: HashMap::from([(
                "proxy_h264".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: Vec::new(),
                    default_media_index: None,
                    subtitles: vec![bilibili_subtitle_track(
                        None,
                        "video:BV1test:cid:1",
                        "Chinese",
                        subtitle_url.to_string(),
                    )],
                    default_subtitle_index: Some(0),
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "proxy_h264".to_string(),
            provider: crate::models::SourceProvider::Bilibili,
            provider_instance_name: None,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: None,
        };
        store
            .set(
                "v:subtitle-test",
                &VersionedPlayback {
                    version: "subtitle-test".to_string(),
                    result: playback,
                    expires_at: crate::SystemClock.now().timestamp() + 60,
                    playback_context: None,
                },
                std::time::Duration::from_secs(60),
            )
            .await?;

        let provider = BilibiliProvider::new_local_only()?;
        let action = provider
            .get_subtitle(Some(&store), "subtitle-test", "proxy_h264", 0, None)
            .await?;

        let PlaybackTransportAction::FetchAndForward {
            url,
            headers,
            range_header,
            proxy_strategy,
        } = action
        else {
            anyhow::bail!("Bilibili subtitles must use the full response cache transport");
        };
        assert_eq!(url, subtitle_url);
        assert_eq!(range_header, None);
        assert_eq!(
            proxy_strategy,
            crate::provider::PlaybackResourceProxyStrategy::FullResponseCache
        );
        assert_eq!(
            headers.get("Referer").map(String::as_str),
            Some("https://www.bilibili.com")
        );
        assert!(headers.contains_key("User-Agent"));
        assert!(
            !headers
                .keys()
                .any(|name| name.eq_ignore_ascii_case("range")),
            "Bilibili subtitle provider headers must leave Range ownership to the transport layer"
        );
        Ok(())
    }

    #[test]
    fn dash_video_codec_family_uses_the_exact_codec_string() {
        let regular = bilibili_upstream::DashInfo {
            video_streams: vec![bilibili_upstream::VideoStream::default()],
            ..Default::default()
        };
        let hevc = bilibili_upstream::DashInfo {
            video_streams: vec![bilibili_upstream::VideoStream {
                codecs: "hev1.1.6.L120.90".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(
            bilibili_dash_video_codec(&regular.video_streams[0].codecs),
            PlaybackVideoCodec::H264
        );
        assert_eq!(
            bilibili_dash_video_codec(&hevc.video_streams[0].codecs),
            PlaybackVideoCodec::Hevc
        );
        assert_eq!(
            bilibili_dash_video_codec("vp09.00.21.08"),
            PlaybackVideoCodec::Vp9
        );
        assert_eq!(bilibili_dash_label(BilibiliDashManifestSlot::Hevc), "HEVC");
    }

    #[test]
    fn dash_codecs_are_exposed_in_one_playback_mode() -> TestResult {
        let video = |id: u64, codecs: &str, url: &str| BilibiliDashVideoStream {
            id,
            codecs: codecs.to_string(),
            base_url: url.to_string(),
            ..Default::default()
        };
        let dash = BilibiliDashManifest {
            video_streams: vec![
                video(
                    1,
                    "avc1.640033",
                    "https://cdn.example/h264.m4s?deadline=100",
                ),
                video(
                    2,
                    "av01.0.08M.08",
                    "https://cdn.example/av1.m4s?deadline=100",
                ),
                video(
                    3,
                    "hev1.1.6.L120.90",
                    "https://cdn.example/hevc.m4s?deadline=100",
                ),
            ],
            audio_streams: vec![
                BilibiliDashAudioStream {
                    id: 30_280,
                    codecs: "mp4a.40.2".to_string(),
                    base_url: "https://cdn.example/aac.m4s?deadline=100".to_string(),
                    ..Default::default()
                },
                BilibiliDashAudioStream {
                    id: 30_250,
                    codecs: "ec-3".to_string(),
                    base_url: "https://cdn.example/eac3.m4s?deadline=100".to_string(),
                    ..Default::default()
                },
                BilibiliDashAudioStream {
                    id: 30_251,
                    codecs: "fLaC".to_string(),
                    base_url: "https://cdn.example/flac.m4s?deadline=100".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut metadata =
            PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video));
        let (infos, default_mode) = bilibili_dash_playback_infos(
            &mut metadata,
            "video:bvid:BV1test:cid:42",
            &dash,
            None,
            BilibiliDashPlaybackOptions {
                provider_instance_name: None,
                subtitles: &[],
                danmakus: &[],
                client_profile: None,
            },
        )?;

        assert_eq!(default_mode, "dash");
        assert_eq!(infos.len(), 1);
        let medias = &infos["dash"].medias;
        assert_eq!(medias.len(), 1);
        assert_eq!(medias[0].name, "DASH");
        assert_eq!(medias[0].format, "mpd");
        assert!(matches!(
            &medias[0].provider,
            PlaybackMediaProvider::Bilibili(
                PlaybackBilibiliMedia::DirectDashManifest { mode_name, .. }
            ) if mode_name == "dash"
        ));
        let PlaybackMetadata::Bilibili(metadata) = metadata else {
            anyhow::bail!("expected Bilibili metadata");
        };
        let Some(manifest) = metadata.dash_manifests.dash.as_ref() else {
            anyhow::bail!("expected unified DASH manifest");
        };
        assert_eq!(manifest.video_streams.len(), 3);
        assert_eq!(manifest.audio_streams.len(), 3);
        Ok(())
    }

    #[test]
    fn unified_dash_merges_unique_video_and_audio_from_additional_response() -> TestResult {
        let primary = BilibiliDashManifest {
            video_streams: vec![BilibiliDashVideoStream {
                id: 80,
                codecid: 7,
                codecs: "avc1.640032".to_string(),
                base_url: "https://cdn.example/h264.m4s".to_string(),
                ..Default::default()
            }],
            audio_streams: vec![BilibiliDashAudioStream {
                id: 30_280,
                codecs: "mp4a.40.2".to_string(),
                base_url: "https://cdn.example/aac.m4s".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let additional = BilibiliDashManifest {
            video_streams: vec![BilibiliDashVideoStream {
                id: 80,
                codecid: 12,
                codecs: "hev1.1.6.L120.90".to_string(),
                base_url: "https://cdn.example/hevc.m4s".to_string(),
                ..Default::default()
            }],
            audio_streams: vec![
                primary.audio_streams[0].clone(),
                BilibiliDashAudioStream {
                    id: 30_251,
                    codecs: "fLaC".to_string(),
                    base_url: "https://cdn.example/flac.m4s".to_string(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let mut metadata =
            PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video));

        bilibili_dash_playback_infos(
            &mut metadata,
            "video:bvid:BV1test:cid:42",
            &primary,
            Some(&additional),
            BilibiliDashPlaybackOptions {
                provider_instance_name: None,
                subtitles: &[],
                danmakus: &[],
                client_profile: None,
            },
        )?;

        let PlaybackMetadata::Bilibili(metadata) = metadata else {
            anyhow::bail!("expected Bilibili metadata");
        };
        let Some(manifest) = metadata.dash_manifests.dash else {
            anyhow::bail!("expected unified DASH manifest");
        };
        assert_eq!(manifest.video_streams.len(), 2);
        assert_eq!(manifest.audio_streams.len(), 2);
        assert_eq!(manifest.audio_streams[1].codecs, "fLaC");
        Ok(())
    }

    #[test]
    fn dash_manifest_groups_each_codec_in_a_stable_adaptation_set() -> TestResult {
        let video = |id: u64, codecs: &str| BilibiliDashVideoStream {
            id,
            codecid: u32::try_from(id).expect("test stream id fits u32"),
            codecs: codecs.to_string(),
            base_url: format!("https://cdn.example/video-{id}.m4s"),
            backup_urls: vec![format!("https://backup.example/video-{id}.m4s")],
            ..Default::default()
        };
        let audio = |id: u64, codecs: &str| BilibiliDashAudioStream {
            id,
            codecs: codecs.to_string(),
            base_url: format!("https://cdn.example/audio-{id}.m4s"),
            ..Default::default()
        };
        let manifest = BilibiliDashManifest {
            video_streams: vec![
                video(1, "avc1.640033"),
                video(2, "hev1.1.6.L120.90"),
                video(3, "av01.0.08M.08"),
                video(4, "vp09.00.21.08"),
            ],
            audio_streams: vec![
                audio(30_280, "mp4a.40.2"),
                audio(30_250, "ec-3"),
                audio(30_251, "fLaC"),
            ],
            duration: 10.0,
            min_buffer_time: 1.5,
        };

        let mpd = build_bilibili_mpd_manifest(&manifest, |_index, url| url.to_string())?;

        for (id, label) in [
            (100, "H.264"),
            (110, "HEVC"),
            (120, "AV1"),
            (130, "VP9"),
            (200, "AAC"),
            (210, "E-AC-3"),
            (230, "FLAC"),
        ] {
            assert!(mpd.contains(&format!("<AdaptationSet id=\"{id}\"")));
            assert!(mpd.contains(&format!("<Label>{label}</Label>")));
        }
        assert_eq!(mpd.matches("<AdaptationSet ").count(), 7);
        assert_eq!(mpd.matches("<Role ").count(), 7);
        assert!(mpd.contains("selectionPriority=\"700\"><Label>AAC</Label>"));
        assert!(mpd.contains("id=\"video-110-2-2\""));
        assert!(mpd.contains("id=\"audio-230-30251\""));
        Ok(())
    }

    #[test]
    fn dash_manifest_keeps_only_codecs_advertised_by_the_client() {
        let video = |id: u64, codecs: &str| BilibiliDashVideoStream {
            id,
            codecs: codecs.to_string(),
            base_url: format!("https://cdn.example/video-{id}.m4s"),
            ..Default::default()
        };
        let audio = |id: u64, codecs: &str| BilibiliDashAudioStream {
            id,
            codecs: codecs.to_string(),
            base_url: format!("https://cdn.example/audio-{id}.m4s"),
            ..Default::default()
        };
        let dash = BilibiliDashManifest {
            video_streams: vec![
                video(32, "avc1.64001F"),
                video(80, "avc1.640032"),
                video(120, "avc1.640033"),
            ],
            audio_streams: vec![
                audio(30_280, "mp4a.40.2"),
                audio(30_250, "ec-3"),
                audio(30_251, "fLaC"),
            ],
            ..Default::default()
        };
        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Dash,
                container: Some(crate::provider::PlaybackContainer::Mp4),
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline: PlaybackMediaPipeline::MediaSource,
                codec_string: Some("avc1.64001F,mp4a.40.2".to_string()),
            }],
            ..PlaybackClientProfile::default()
        };

        let filtered = filter_bilibili_dash_manifest(&dash, Some(&profile));

        assert_eq!(filtered.video_streams.len(), 1);
        assert_eq!(filtered.video_streams[0].codecs, "avc1.64001F");
        assert_eq!(filtered.audio_streams.len(), 1);
        assert_eq!(filtered.audio_streams[0].codecs, "mp4a.40.2");
    }

    #[test]
    fn dash_playback_metadata_contains_only_client_compatible_codecs() -> TestResult {
        let video = |id: u64, codecs: &str| BilibiliDashVideoStream {
            id,
            codecs: codecs.to_string(),
            base_url: format!("https://cdn.example/video-{id}.m4s?deadline=100"),
            ..Default::default()
        };
        let audio = |id: u64, codecs: &str| BilibiliDashAudioStream {
            id,
            codecs: codecs.to_string(),
            base_url: format!("https://cdn.example/audio-{id}.m4s?deadline=100"),
            ..Default::default()
        };
        let dash = BilibiliDashManifest {
            video_streams: vec![
                video(32, "avc1.64001F"),
                video(80, "avc1.640032"),
                video(120, "av01.0.08M.08"),
            ],
            audio_streams: vec![audio(30_280, "mp4a.40.2"), audio(30_250, "ec-3")],
            ..Default::default()
        };
        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Dash,
                container: Some(crate::provider::PlaybackContainer::Mp4),
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Aac),
                pipeline: PlaybackMediaPipeline::MediaSource,
                codec_string: Some("avc1.64001F,mp4a.40.2".to_string()),
            }],
            ..PlaybackClientProfile::default()
        };
        let mut metadata =
            PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video));

        let (infos, default_mode) = bilibili_dash_playback_infos(
            &mut metadata,
            "video:bvid:BV1test:cid:42",
            &dash,
            None,
            BilibiliDashPlaybackOptions {
                provider_instance_name: None,
                subtitles: &[],
                danmakus: &[],
                client_profile: Some(&profile),
            },
        )?;

        assert_eq!(default_mode, "dash");
        assert_eq!(infos.len(), 1);
        let PlaybackMetadata::Bilibili(metadata) = metadata else {
            anyhow::bail!("expected Bilibili metadata");
        };
        let Some(manifest) = metadata.dash_manifests.dash else {
            anyhow::bail!("expected unified DASH manifest");
        };
        assert_eq!(
            manifest
                .video_streams
                .iter()
                .map(|stream| stream.codecs.as_str())
                .collect::<Vec<_>>(),
            vec!["avc1.64001F"]
        );
        assert_eq!(
            manifest
                .audio_streams
                .iter()
                .map(|stream| stream.codecs.as_str())
                .collect::<Vec<_>>(),
            vec!["mp4a.40.2"]
        );
        assert!(metadata.dash_manifests.av1.is_none());
        assert!(metadata.dash_manifests.hevc.is_none());
        Ok(())
    }

    #[test]
    fn dash_playback_reports_incompatible_video_and_audio_separately() {
        let dash = BilibiliDashManifest {
            video_streams: vec![BilibiliDashVideoStream {
                id: 32,
                codecs: "avc1.64001F".to_string(),
                base_url: "https://cdn.example/video.m4s?deadline=100".to_string(),
                ..Default::default()
            }],
            audio_streams: vec![BilibiliDashAudioStream {
                id: 30_280,
                codecs: "mp4a.40.2".to_string(),
                base_url: "https://cdn.example/audio.m4s?deadline=100".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let profile = |video_codec, audio_codec, codec_string: &str| PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            environment: PlaybackClientEnvironment::Web,
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Dash,
                container: Some(crate::provider::PlaybackContainer::Mp4),
                video_codec: Some(video_codec),
                audio_codec: Some(audio_codec),
                pipeline: PlaybackMediaPipeline::MediaSource,
                codec_string: Some(codec_string.to_string()),
            }],
            ..PlaybackClientProfile::default()
        };

        let unsupported_video = profile(
            PlaybackVideoCodec::Av1,
            PlaybackAudioCodec::Aac,
            "av01.0.08M.08,mp4a.40.2",
        );
        let mut metadata =
            PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video));
        let video_error = bilibili_dash_playback_infos(
            &mut metadata,
            "video:bvid:BV1test:cid:42",
            &dash,
            None,
            BilibiliDashPlaybackOptions {
                provider_instance_name: None,
                subtitles: &[],
                danmakus: &[],
                client_profile: Some(&unsupported_video),
            },
        )
        .expect_err("the client does not advertise the upstream video codec");
        assert!(matches!(
            video_error,
            super::ProviderError::ClientIncompatible {
                required_capability: Some(ref capability),
                ..
            } if capability == "dash_video_codec_string"
        ));

        let unsupported_audio = profile(
            PlaybackVideoCodec::H264,
            PlaybackAudioCodec::Eac3,
            "avc1.64001F,ec-3",
        );
        let mut metadata =
            PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video));
        let audio_error = bilibili_dash_playback_infos(
            &mut metadata,
            "video:bvid:BV1test:cid:42",
            &dash,
            None,
            BilibiliDashPlaybackOptions {
                provider_instance_name: None,
                subtitles: &[],
                danmakus: &[],
                client_profile: Some(&unsupported_audio),
            },
        )
        .expect_err("the client does not advertise the upstream audio codec");
        assert!(matches!(
            audio_error,
            super::ProviderError::ClientIncompatible {
                required_capability: Some(ref capability),
                ..
            } if capability == "dash_audio_codec_string"
        ));
    }

    #[test]
    fn unified_dash_keeps_one_proxy_route_and_a_content_level_swarm() {
        let source = super::playback_media(
            "DASH".to_string(),
            "mpd".to_string(),
            None,
            Some("sm3_test_dash".to_string()),
            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct {
                url: "https://cdn.example/source.mpd".to_string(),
                headers: HashMap::new(),
            }),
        );
        let mut result = PlaybackResult {
            playback_infos: HashMap::from([(
                "dash".to_string(),
                PlaybackInfo {
                    thumbnail: None,
                    medias: vec![source],
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            )]),
            default_mode: "dash".to_string(),
            provider: crate::models::SourceProvider::Bilibili,
            provider_instance_name: None,
            duration_seconds: Some(10.0),
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: Some(PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                bvid: Some("BV1test".to_string()),
                cid: Some(42),
                dash_manifests: BilibiliDashManifests {
                    dash: Some(BilibiliDashManifest {
                        video_streams: vec![
                            BilibiliDashVideoStream {
                                id: 32,
                                codecs: "avc1.64001F".to_string(),
                                base_url: "https://cdn.example/dash.m4s?deadline=100".to_string(),
                                ..Default::default()
                            },
                            BilibiliDashVideoStream {
                                id: 80,
                                codecs: "hev1.1.6.L120.90".to_string(),
                                base_url: "https://cdn.example/hevc.m4s?deadline=200".to_string(),
                                ..Default::default()
                            },
                        ],
                        audio_streams: vec![BilibiliDashAudioStream {
                            id: 30_250,
                            codecs: "ec-3".to_string(),
                            base_url: "https://cdn.example/eac3.m4s?deadline=150".to_string(),
                            ..Default::default()
                        }],
                        ..Default::default()
                    }),
                    h264: None,
                    av1: None,
                    hevc: None,
                },
                ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video)
            })),
        };
        let mut eac3_result = result.clone();

        mark_bilibili_playback_resources(
            &mut result,
            "version",
            123,
            crate::models::PlaybackProxyMode::Auto,
            None,
        );

        assert_eq!(result.default_mode, "proxy_dash");
        assert!(!result.playback_infos.contains_key("dash"));
        let proxied = &result.playback_infos["proxy_dash"].medias;
        assert_eq!(proxied.len(), 1);
        assert_eq!(proxied[0].name, "DASH");
        assert_eq!(
            proxied[0].expire_at.map(|value| value.timestamp()),
            Some(100)
        );
        assert!(matches!(
            &proxied[0].provider,
            PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::ProxyDashManifest {
                mode_name,
                ..
            }) if mode_name == "dash"
        ));
        assert_eq!(
            proxied[0].p2p_swarm_id.as_deref(),
            Some(bilibili_dash_manifest_swarm_id(None, "video:bvid:BV1test:cid:42").as_str())
        );

        let profile = PlaybackClientProfile {
            profile_version: CURRENT_PLAYBACK_CLIENT_PROFILE_VERSION,
            media_capabilities: vec![PlaybackMediaCapability {
                transport: PlaybackMediaTransport::Dash,
                container: Some(crate::provider::PlaybackContainer::Mp4),
                video_codec: Some(PlaybackVideoCodec::H264),
                audio_codec: Some(PlaybackAudioCodec::Eac3),
                pipeline: PlaybackMediaPipeline::Native,
                codec_string: Some("avc1.64001F,ec-3".to_string()),
            }],
            ..PlaybackClientProfile::default()
        };
        let context = ProviderContext::new("test", ProviderActor::System)
            .with_playback_client_profile(Some(profile));
        mark_bilibili_playback_resources(
            &mut eac3_result,
            "version",
            123,
            crate::models::PlaybackProxyMode::Prefer,
            Some(&context),
        );

        assert!(eac3_result.playback_infos.contains_key("dash"));
        assert!(eac3_result.playback_infos.contains_key("proxy_dash"));
    }

    #[test]
    fn live_streams_group_quality_variants_by_cdn_host() -> TestResult {
        let stream = |quality, quality_name: &str, host: &str, suffix: &str, expires_at| {
            super::bilibili_upstream::LiveStream {
                quality,
                quality_name: quality_name.to_string(),
                protocol: "http_hls".to_string(),
                format: "fmp4".to_string(),
                codec: "avc".to_string(),
                urls: vec![super::bilibili_upstream::LiveStreamUrl {
                    host: host.to_string(),
                    url: format!("{host}/{suffix}.m3u8"),
                    expires_at,
                }],
            }
        };
        let infos = provider_ok(bilibili_live_playback_infos(
            vec![
                stream(400, "1080P", "https://main.example", "high", Some(400)),
                stream(80, "流畅", "https://main.example", "low", Some(80)),
                stream(400, "1080P", "https://backup.example", "high", Some(401)),
                stream(80, "流畅", "https://backup.example", "low", None),
            ],
            &[],
        ))?;

        assert_eq!(infos.len(), 2);
        assert_eq!(infos["main"].medias.len(), 2);
        assert_eq!(infos["main"].medias[0].name, "1080P");
        assert_eq!(infos["main"].medias[1].name, "流畅");
        assert_eq!(
            infos["main"].medias[0]
                .expire_at
                .map(|value| value.timestamp()),
            Some(400)
        );
        assert_eq!(
            infos["main"].medias[1]
                .expire_at
                .map(|value| value.timestamp()),
            Some(80)
        );
        assert_eq!(infos["backup_1"].medias.len(), 2);
        assert_eq!(
            infos["backup_1"].medias[0]
                .expire_at
                .map(|value| value.timestamp()),
            Some(401)
        );
        assert_eq!(infos["backup_1"].medias[1].expire_at, None);
        Ok(())
    }

    #[test]
    fn live_streams_preserve_hls_and_flv_formats() -> TestResult {
        let stream =
            |protocol: &str, format: &str, url: &str| super::bilibili_upstream::LiveStream {
                quality: 400,
                quality_name: "1080P".to_string(),
                protocol: protocol.to_string(),
                format: format.to_string(),
                codec: "avc".to_string(),
                urls: vec![super::bilibili_upstream::LiveStreamUrl {
                    host: format!("https://{format}.example"),
                    url: url.to_string(),
                    expires_at: None,
                }],
            };
        let infos = provider_ok(bilibili_live_playback_infos(
            vec![
                stream("http_hls", "fmp4", "https://fmp4.example/live.m3u8"),
                stream("http_stream", "flv", "https://flv.example/live.flv"),
            ],
            &[],
        ))?;

        let formats = infos
            .values()
            .flat_map(|info| info.medias.iter().map(|media| media.format.as_str()))
            .collect::<Vec<_>>();
        assert!(formats.contains(&"m3u8"));
        assert!(formats.contains(&"flv"));
        Ok(())
    }

    #[test]
    fn live_playback_rejects_an_empty_upstream_stream_set() {
        assert!(matches!(
            bilibili_live_playback_infos(Vec::new(), &[]),
            Err(super::ProviderError::NotFound)
        ));
    }

    #[test]
    fn durl_manifest_rejects_line_breaks_in_segment_urls() {
        let error = build_bilibili_durl_manifest(&[BilibiliDurlSegment {
            url: "https://cdn.example/video.mp4".to_string(),
            backup_urls: vec!["https://backup.example/video.mp4\n#EXT-X-ENDLIST".to_string()],
            duration_millis: 1_000,
        }])
        .expect_err("line breaks must not enter generated manifests");
        assert!(error.to_string().contains("line break"));
    }
}

fn map_bilibili_live_danmaku_event(
    event: bilibili_upstream::BilibiliLiveDanmakuEvent,
) -> super::BilibiliLiveDanmakuEvent {
    let event_type = bilibili_upstream::BilibiliLiveDanmakuEventType::try_from(event.r#type)
        .unwrap_or(bilibili_upstream::BilibiliLiveDanmakuEventType::Unspecified);
    let kind = match event_type {
        bilibili_upstream::BilibiliLiveDanmakuEventType::Unspecified => {
            super::BilibiliLiveDanmakuEventKind::Unspecified
        }
        bilibili_upstream::BilibiliLiveDanmakuEventType::Chat => {
            super::BilibiliLiveDanmakuEventKind::Chat
        }
        bilibili_upstream::BilibiliLiveDanmakuEventType::UserEnter => {
            super::BilibiliLiveDanmakuEventKind::UserEnter
        }
        bilibili_upstream::BilibiliLiveDanmakuEventType::Gift => {
            super::BilibiliLiveDanmakuEventKind::Gift
        }
        bilibili_upstream::BilibiliLiveDanmakuEventType::Heartbeat => {
            super::BilibiliLiveDanmakuEventKind::Heartbeat
        }
        bilibili_upstream::BilibiliLiveDanmakuEventType::Unknown => {
            super::BilibiliLiveDanmakuEventKind::Unknown
        }
    };
    super::BilibiliLiveDanmakuEvent {
        format: event.format,
        event_type: event.event_type,
        kind,
        user: event.user,
        message: event.message,
        timestamp: event.timestamp,
        gift_name: event.gift_name,
        gift_count: event.gift_count,
        online_count: event.online_count,
    }
}

#[async_trait]
impl super::BilibiliLiveDanmakuProvider for BilibiliProvider {
    async fn watch_bilibili_live_danmaku(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<super::BilibiliLiveDanmakuStream, ProviderError> {
        let config = BilibiliSourceConfig::from_media_config(source_config)?;
        let BilibiliSourceConfig::Live(config) = config else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili live danmaku requires a live source config".to_string(),
            ));
        };

        let credential_user_id = bilibili_optional_credential_user_id(
            ctx,
            ProviderCredentialPolicy::from_shared(config.shared),
        )?;
        let (cookies, _) = resolve_optional_bilibili_cookies(ctx, credential_user_id).await?;
        let client = self
            .get_client_with_context(ctx.provider_instance_name(), ctx.request_context())
            .await?;
        let stream = client
            .watch_bilibili_live_danmaku(bilibili_upstream::WatchBilibiliLiveDanmakuReq {
                cookies,
                room_id: config.room_id,
            })
            .await?;
        let stream = stream.map(|event| {
            event
                .map(map_bilibili_live_danmaku_event)
                .map_err(ProviderError::from)
        });
        Ok(Box::pin(stream))
    }
}

impl BilibiliProvider {
    pub async fn get_media_stream(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        let headers = media.upstream_headers();
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
                range_header: range_header.map(ToString::to_string),
                proxy_strategy: super::PlaybackResourceProxyStrategy::SliceCache,
            },
        )
    }

    pub async fn get_hls_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let media = playback_info
            .medias
            .get(url_index)
            .ok_or(ProviderError::NotFound)?;
        if let PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDurlManifest {
            segments,
            ..
        }) = &media.provider
        {
            return Ok(
                super::playback_transport::PlaybackTransportAction::M3u8DirectBody {
                    body: build_bilibili_durl_manifest(segments)?.into_bytes(),
                },
            );
        }
        if let PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DurlManifest {
            segments,
            ..
        }) = &media.provider
        {
            return Ok(
                super::playback_transport::PlaybackTransportAction::M3u8BodyRewrite {
                    body: build_bilibili_durl_manifest(segments)?.into_bytes(),
                },
            );
        }
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        let headers = media.upstream_headers();
        Ok(
            super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                url: url.to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
            },
        )
    }

    pub async fn get_hls_resource(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        request: BilibiliHlsResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, request.version, request_context)
                .await?;
        let media = versioned
            .result
            .playback_infos
            .get(request.mode_name)
            .and_then(|info| info.medias.get(request.media_index))
            .ok_or(ProviderError::NotFound)?;
        let headers = media.upstream_headers();
        let headers = if headers.is_empty() {
            bilibili_headers()
        } else {
            headers
        };
        if request.is_manifest {
            Ok(
                super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                    url: request.target_url.to_string(),
                    headers,
                },
            )
        } else {
            if matches!(
                &media.provider,
                PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DurlManifest { .. })
            ) {
                let urls = bilibili_durl_resource_candidates(media, request.target_url)
                    .ok_or(ProviderError::NotFound)?;
                return Ok(
                    super::playback_transport::PlaybackTransportAction::FetchAndForwardCandidates {
                        urls,
                        headers,
                        range_header: request.range_header.map(ToString::to_string),
                    },
                );
            }
            Ok(
                super::playback_transport::PlaybackTransportAction::FetchAndForward {
                    url: request.target_url.to_string(),
                    headers,
                    range_header: request.range_header.map(ToString::to_string),
                    proxy_strategy: super::PlaybackResourceProxyStrategy::SliceCache,
                },
            )
        }
    }

    pub async fn get_dash_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        manifest_mode: BilibiliDashManifestMode,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let dash = dash_manifest_from_metadata(&versioned.result, mode_name)?;
        let body = build_bilibili_mpd_manifest(&dash, |_index, url| url.to_string())?;
        Ok(match manifest_mode {
            BilibiliDashManifestMode::Direct => {
                super::playback_transport::PlaybackTransportAction::DirectBody {
                    body: body.into_bytes(),
                    content_type: "application/dash+xml".to_string(),
                    status: 200,
                }
            }
            BilibiliDashManifestMode::Proxy => {
                super::playback_transport::PlaybackTransportAction::MpdBodyRewrite {
                    body: body.into_bytes(),
                    source_url: "https://synctv.invalid/bilibili-generated.mpd".to_string(),
                }
            }
        })
    }

    pub async fn get_dash_resource(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        request: BilibiliDashResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let target_url = super::playback_transport::resolve_dash_scope_target(
            request.scope_url,
            request.resource_path,
            request.resource_query,
        )?;
        if request.is_manifest {
            Ok(
                super::playback_transport::PlaybackTransportAction::MpdRewrite {
                    url: target_url,
                    headers: bilibili_headers(),
                },
            )
        } else {
            let versioned = super::playback_transport::lookup_versioned(
                store,
                request.version,
                request_context,
            )
            .await?;
            let dash = dash_manifest_from_metadata(&versioned.result, request.mode_name)?;
            let urls = bilibili_dash_resource_candidates(
                &dash,
                request.scope_url,
                request.resource_path,
                request.resource_query,
            )?
            .unwrap_or_else(|| vec![target_url]);
            Ok(
                super::playback_transport::PlaybackTransportAction::FetchAndForwardCandidates {
                    urls,
                    headers: bilibili_headers(),
                    range_header: request.range_header.map(ToString::to_string),
                },
            )
        }
    }

    pub async fn get_subtitle(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let subtitle = playback_info
            .subtitles
            .get(subtitle_index)
            .ok_or(ProviderError::NotFound)?;
        let media_headers = playback_info
            .medias
            .first()
            .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
        let headers = super::subtitle_headers_for_proxy(&media_headers, subtitle);
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: subtitle.upstream_url().to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
                range_header: None,
                proxy_strategy: super::PlaybackResourceProxyStrategy::FullResponseCache,
            },
        )
    }

    pub async fn get_danmaku_file(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        danmaku_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let playback_info = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .ok_or(ProviderError::NotFound)?;
        let danmaku = playback_info
            .danmakus
            .get(danmaku_index)
            .ok_or(ProviderError::NotFound)?;
        let url = danmaku.upstream_url().ok_or(ProviderError::NotFound)?;
        let headers = danmaku.upstream_headers();
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.to_string(),
                headers: if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                },
                range_header: None,
                proxy_strategy: super::PlaybackResourceProxyStrategy::FullResponseCache,
            },
        )
    }
}

use super::bilibili_headers;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BilibiliDashManifestMode {
    Direct,
    Proxy,
}

fn playback_media(
    name: String,
    format: String,
    expires_at: Option<i64>,
    p2p_swarm_id: Option<String>,
    provider: PlaybackMediaProvider,
) -> PlaybackMedia {
    PlaybackMedia {
        name,
        format,
        expire_at: expires_at.and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        metadata: None,
        p2p_swarm_id,
        provider,
    }
}

fn bilibili_url_expiration_timestamp(value: &str) -> Option<i64> {
    let bilibili_expiration = url::Url::parse(value).ok().and_then(|url| {
        url.query_pairs()
            .filter(|(key, _)| {
                key.eq_ignore_ascii_case("deadline") || key.eq_ignore_ascii_case("expires")
            })
            .filter_map(|(_, value)| value.parse::<i64>().ok())
            .filter(|expires_at| *expires_at > 0)
            .min()
    });
    super::url_expiration_timestamp(value)
        .into_iter()
        .chain(bilibili_expiration)
        .min()
}

fn bilibili_subtitle_track(
    provider_instance_name: Option<&str>,
    content_descriptor: &str,
    name: &str,
    url: String,
) -> PlaybackSubtitle {
    let expire_at = bilibili_url_expiration_timestamp(&url)
        .and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0));
    let url_identity = url::Url::parse(&url).map_or_else(
        |_| url.split('?').next().unwrap_or_default().to_string(),
        |url| {
            format!(
                "{}://{}{}",
                url.scheme(),
                url.host_str().unwrap_or_default(),
                url.path()
            )
        },
    );
    PlaybackSubtitle {
        language: name.to_string(),
        name: name.to_string(),
        format: "json".to_string(),
        p2p_swarm_id: Some(super::provider_p2p_swarm_id(
            BilibiliProvider::NAME,
            provider_instance_name,
            "subtitle",
            &format!("{content_descriptor}:track:{name}:url:{url_identity}"),
        )),
        provider: PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle::Direct {
            url,
            headers: bilibili_headers(),
            expire_at,
        }),
    }
}

fn dash_manifest_expiration(dash: &BilibiliDashManifest) -> Option<i64> {
    dash.video_streams
        .iter()
        .flat_map(|stream| std::iter::once(&stream.base_url).chain(&stream.backup_urls))
        .chain(
            dash.audio_streams
                .iter()
                .flat_map(|stream| std::iter::once(&stream.base_url).chain(&stream.backup_urls)),
        )
        .filter_map(|url| bilibili_url_expiration_timestamp(url))
        .min()
}

fn bilibili_video_content_descriptor(bvid: Option<&str>, aid: u64, cid: u64) -> String {
    bvid.map_or_else(
        || format!("video:aid:{aid}:cid:{cid}"),
        |bvid| format!("video:bvid:{bvid}:cid:{cid}"),
    )
}

fn bilibili_pgc_content_descriptor(epid: u64, cid: u64) -> String {
    format!("pgc:epid:{epid}:cid:{cid}")
}

fn bilibili_content_descriptor(result: &PlaybackResult) -> Option<String> {
    let Some(PlaybackMetadata::Bilibili(metadata)) = result.metadata.as_ref() else {
        return None;
    };
    match metadata.kind {
        BilibiliPlaybackKind::Video => Some(bilibili_video_content_descriptor(
            metadata.bvid.as_deref(),
            metadata.aid.unwrap_or_default(),
            metadata.cid?,
        )),
        BilibiliPlaybackKind::Pgc => Some(bilibili_pgc_content_descriptor(
            metadata.epid?,
            metadata.cid?,
        )),
        BilibiliPlaybackKind::Live => None,
    }
}

fn bilibili_dash_manifest_swarm_id(
    provider_instance_name: Option<&str>,
    content_descriptor: &str,
) -> String {
    super::provider_p2p_swarm_id(
        BilibiliProvider::NAME,
        provider_instance_name,
        "dash-media-set",
        &format!("{content_descriptor}:schema:{BILIBILI_DASH_SWARM_SCHEMA_VERSION}"),
    )
}

fn bilibili_durl_media(
    name: &str,
    segments: impl IntoIterator<Item = (String, Vec<String>, u64)>,
    p2p_swarm_id: String,
) -> Result<PlaybackMedia, ProviderError> {
    let segments = segments
        .into_iter()
        .map(|(url, backup_urls, duration_millis)| BilibiliDurlSegment {
            url,
            backup_urls,
            duration_millis,
        })
        .collect::<Vec<_>>();
    if segments.is_empty() {
        return Err(ProviderError::ApiError(
            "Bilibili DURL playback is empty".to_string(),
        ));
    }
    let expires_at = segments
        .iter()
        .flat_map(|segment| std::iter::once(&segment.url).chain(&segment.backup_urls))
        .filter_map(|url| bilibili_url_expiration_timestamp(url))
        .min();
    Ok(playback_media(
        name.to_string(),
        "m3u8".to_string(),
        expires_at,
        Some(p2p_swarm_id),
        PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDurlManifest {
            version: String::new(),
            expires_at: 0,
            mode_name: "mp4".to_string(),
            segments,
            headers: bilibili_headers(),
        }),
    ))
}

fn bilibili_durl_resource_candidates(
    media: &PlaybackMedia,
    target_url: &str,
) -> Option<Vec<String>> {
    let PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DurlManifest { segments, .. }) =
        &media.provider
    else {
        return None;
    };
    let segment = segments.iter().find(|segment| segment.url == target_url)?;
    let mut candidates = Vec::with_capacity(1 + segment.backup_urls.len());
    for candidate in std::iter::once(&segment.url).chain(&segment.backup_urls) {
        if !candidate.trim().is_empty() && !candidates.contains(candidate) {
            candidates.push(candidate.clone());
        }
    }
    (!candidates.is_empty()).then_some(candidates)
}

fn build_bilibili_durl_manifest(segments: &[BilibiliDurlSegment]) -> Result<String, ProviderError> {
    if segments.is_empty() {
        return Err(ProviderError::NotFound);
    }
    if segments.iter().any(|segment| {
        std::iter::once(&segment.url)
            .chain(&segment.backup_urls)
            .any(|url| url.contains('\r') || url.contains('\n'))
    }) {
        return Err(ProviderError::ApiError(
            "Bilibili DURL segment URL contains a line break".to_string(),
        ));
    }
    let target_duration = segments
        .iter()
        .map(|segment| segment.duration_millis.div_ceil(1_000))
        .max()
        .unwrap_or(1)
        .max(1);
    let mut manifest = format!(
        "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:0\n#EXT-X-PLAYLIST-TYPE:VOD\n"
    );
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            manifest.push_str("#EXT-X-DISCONTINUITY\n");
        }
        let duration_seconds = segment.duration_millis / 1_000;
        let duration_millis = segment.duration_millis % 1_000;
        let _ = writeln!(manifest, "#EXTINF:{duration_seconds}.{duration_millis:03},");
        let _ = writeln!(manifest, "{}", segment.url);
    }
    manifest.push_str("#EXT-X-ENDLIST\n");
    Ok(manifest)
}

fn bilibili_live_headers() -> HashMap<String, String> {
    let mut headers = bilibili_headers();
    headers.insert(
        "Referer".to_string(),
        "https://live.bilibili.com".to_string(),
    );
    headers
}

fn bilibili_live_danmaku_track(
    ctx: &ProviderContext<'_>,
    live_room_id: u64,
) -> Option<PlaybackDanmaku> {
    let room_id = ctx.room_id()?;
    let provider = match (ctx.media_id(), ctx.playlist_id()) {
        (Some(media_id), _) => PlaybackBilibiliDanmaku::Live {
            room_id: *room_id,
            media_id: *media_id,
        },
        (None, Some(playlist_id)) => PlaybackBilibiliDanmaku::DynamicLive {
            room_id: *room_id,
            playlist_id: *playlist_id,
            live_room_id,
        },
        (None, None) => return None,
    };
    Some(PlaybackDanmaku {
        name: LIVE_DANMAKU_TRACK_NAME.to_string(),
        format: Some(LIVE_DANMAKU_FORMAT.to_string()),
        p2p_swarm_id: None,
        provider: PlaybackDanmakuProvider::Bilibili(provider),
    })
}

fn bilibili_vod_danmaku_track(
    provider_instance_name: Option<&str>,
    content_descriptor: &str,
    cid: u64,
) -> PlaybackDanmaku {
    PlaybackDanmaku {
        name: "Bilibili danmaku".to_string(),
        format: Some("xml".to_string()),
        p2p_swarm_id: Some(super::provider_p2p_swarm_id(
            BilibiliProvider::NAME,
            provider_instance_name,
            "danmaku",
            &format!("{content_descriptor}:cid:{cid}"),
        )),
        provider: PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::FileDirect {
            url: format!("https://api.bilibili.com/x/v1/dm/list.so?oid={cid}"),
            headers: bilibili_headers(),
            expire_at: None,
        }),
    }
}

fn bilibili_dash_video_url_request(
    cookies: &HashMap<String, String>,
    aid: u64,
    bvid: String,
    cid: u64,
) -> bilibili_upstream::GetDashVideoUrlReq {
    bilibili_upstream::GetDashVideoUrlReq {
        aid,
        bvid,
        cid,
        cookies: cookies.clone(),
    }
}

fn bilibili_video_url_request(
    cookies: &HashMap<String, String>,
    aid: u64,
    bvid: String,
    cid: u64,
    quality: u64,
) -> bilibili_upstream::GetVideoUrlReq {
    bilibili_upstream::GetVideoUrlReq {
        aid,
        bvid,
        cid,
        quality,
        cookies: cookies.clone(),
    }
}

fn bilibili_subtitles_request(
    cookies: &HashMap<String, String>,
    aid: u64,
    bvid: String,
    cid: u64,
) -> bilibili_upstream::GetSubtitlesReq {
    bilibili_upstream::GetSubtitlesReq {
        aid,
        bvid,
        cid,
        cookies: cookies.clone(),
    }
}

fn bilibili_dash_pgc_url_request(
    cookies: &HashMap<String, String>,
    epid: u64,
    cid: u64,
) -> bilibili_upstream::GetDashPgcurlReq {
    bilibili_upstream::GetDashPgcurlReq {
        epid,
        cid,
        cookies: cookies.clone(),
    }
}

fn bilibili_pgc_url_request(
    cookies: &HashMap<String, String>,
    epid: u64,
    cid: u64,
    quality: u64,
) -> bilibili_upstream::GetPgcurlReq {
    bilibili_upstream::GetPgcurlReq {
        epid,
        cid,
        quality,
        cookies: cookies.clone(),
    }
}

fn bilibili_live_streams_request(
    cookies: HashMap<String, String>,
    room_id: u64,
    hls: bool,
) -> bilibili_upstream::GetLiveStreamsReq {
    bilibili_upstream::GetLiveStreamsReq {
        cid: room_id,
        hls,
        cookies,
    }
}

fn bilibili_live_uses_hls(profile: Option<&super::PlaybackClientProfile>) -> bool {
    !profile.is_some_and(|profile| {
        profile.supports_transport(super::PlaybackMediaTransport::Flv)
            && !profile.supports_transport(super::PlaybackMediaTransport::Hls)
    })
}

fn bilibili_live_transport_cache_token(
    profile: Option<&super::PlaybackClientProfile>,
) -> &'static str {
    if bilibili_live_uses_hls(profile) {
        "hls"
    } else {
        "flv"
    }
}

impl BilibiliProvider {
    /// Resolve playback result from Bilibili API (no caching).
    /// Cookies are resolved from the credential store, not from source_config.
    async fn resolve_from_api_with_cookies(
        &self,
        ctx: &ProviderContext<'_>,
        config: &BilibiliSourceConfig,
        cookies: &HashMap<String, String>,
        provider_instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<PlaybackResult, ProviderError> {
        let sanitized_cookies = cookies.clone();
        let client = self
            .get_client_with_context(provider_instance_name, request_context)
            .await?;

        let mode_info = |medias: Vec<PlaybackMedia>,
                         subtitles: Vec<PlaybackSubtitle>,
                         danmakus: Vec<PlaybackDanmaku>| {
            let default_danmaku_index = (!danmakus.is_empty()).then_some(0);
            PlaybackInfo {
                thumbnail: None,
                medias,
                default_media_index: None,
                subtitles,
                default_subtitle_index: None,
                danmakus,
                default_danmaku_index,
            }
        };

        match config {
            BilibiliSourceConfig::Video(config) => {
                let (bvid, aid) =
                    resolve_bilibili_video_identifier(config.bvid.as_deref(), config.aid)?;
                let request_bvid = bvid.clone().unwrap_or_default();
                let cid = config.cid;
                let content_descriptor =
                    bilibili_video_content_descriptor(bvid.as_deref(), aid, cid);

                let request = bilibili_dash_video_url_request(
                    &sanitized_cookies,
                    aid,
                    request_bvid.clone(),
                    cid,
                );
                let dash_resp = client.get_dash_video_url(request).await;
                let dash_resp = match dash_resp {
                    Ok(dash_resp) if dash_resp.dash.is_some() => Some(dash_resp),
                    Ok(_) => None,
                    Err(error) if is_bilibili_video_dash_unavailable(&error) => None,
                    Err(error) => return Err(error.into()),
                };

                let mut metadata = PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                    bvid,
                    aid: Some(aid),
                    cid: Some(cid),
                    ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Video)
                });
                let mut subtitles = Vec::new();

                let subtitle_request =
                    bilibili_subtitles_request(&sanitized_cookies, aid, request_bvid.clone(), cid);
                match client.get_subtitles(subtitle_request).await {
                    Ok(subtitle_resp) => {
                        subtitles = subtitle_resp
                            .subtitles
                            .into_iter()
                            .map(|(name, url)| {
                                bilibili_subtitle_track(
                                    provider_instance_name,
                                    &content_descriptor,
                                    &name,
                                    url,
                                )
                            })
                            .collect();
                    }
                    Err(e) => {
                        tracing::warn!(
                            bvid = %request_bvid, aid = %aid, cid = %cid, error = %e,
                            "Failed to fetch Bilibili subtitles for video, continuing without subtitles"
                        );
                    }
                }

                let duration_seconds = dash_resp
                    .as_ref()
                    .and_then(|resp| resp.dash.as_ref())
                    .map(|dash| dash.duration);
                if let Some(dash) = dash_resp.as_ref().and_then(|resp| resp.dash.as_ref()) {
                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.min_buffer_time = Some(dash.min_buffer_time);
                    }
                }

                let mut playback_infos = HashMap::new();
                let danmakus = vec![bilibili_vod_danmaku_track(
                    provider_instance_name,
                    &content_descriptor,
                    cid,
                )];

                let default_mode = if let Some(dash_resp) = dash_resp {
                    let dash = dash_resp.dash.as_ref().ok_or_else(|| {
                        ProviderError::ApiError(
                            "Bilibili video playback response did not include DASH streams"
                                .to_string(),
                        )
                    })?;
                    let primary_dash = dash_manifest_from_upstream(dash);
                    let hevc_dash = dash_resp
                        .hevc_dash
                        .as_ref()
                        .filter(|dash| !dash.video_streams.is_empty())
                        .map(dash_manifest_from_upstream);
                    let (dash_infos, default_mode) = bilibili_dash_playback_infos(
                        &mut metadata,
                        &content_descriptor,
                        &primary_dash,
                        hevc_dash.as_ref(),
                        BilibiliDashPlaybackOptions {
                            provider_instance_name,
                            subtitles: &subtitles,
                            danmakus: &danmakus,
                            client_profile: ctx.playback_client_profile(),
                        },
                    )?;
                    playback_infos.extend(dash_infos);
                    default_mode
                } else {
                    let request =
                        bilibili_video_url_request(&sanitized_cookies, aid, request_bvid, cid, 80);
                    let video_resp = client.get_video_url(request).await?;
                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.fallback_format = Some("durl".to_string());
                        metadata.quality = Some(video_resp.current_quality);
                    }
                    let durl_swarm_id = super::provider_p2p_swarm_id(
                        Self::NAME,
                        provider_instance_name,
                        "media",
                        &format!(
                            "{content_descriptor}:durl:quality:{}",
                            video_resp.current_quality
                        ),
                    );
                    playback_infos.insert(
                        "mp4".to_string(),
                        mode_info(
                            vec![bilibili_durl_media(
                                "MP4",
                                video_resp.segments.into_iter().map(|segment| {
                                    (segment.url, segment.backup_urls, segment.duration_millis)
                                }),
                                durl_swarm_id,
                            )?],
                            subtitles,
                            danmakus,
                        ),
                    );
                    "mp4".to_string()
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: crate::models::SourceProvider::Bilibili,
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds,
                    playback_kind: Some(crate::models::PlaybackKind::Regular),
                    metadata: Some(metadata),
                })
            }

            BilibiliSourceConfig::Pgc(config) => {
                let epid = config.epid;
                let cid = config.cid;
                let content_descriptor = bilibili_pgc_content_descriptor(epid, cid);

                let request = bilibili_dash_pgc_url_request(&sanitized_cookies, epid, cid);
                let first_dash_resp = client.get_dash_pgcurl(request.clone()).await;
                let dash_resp = if matches!(
                    &first_dash_resp,
                    Ok(response) if response.dash.is_none()
                ) || matches!(
                    &first_dash_resp,
                    Err(error) if is_bilibili_pgc_dash_unavailable(error)
                ) {
                    tracing::warn!(
                        epid,
                        cid,
                        "Bilibili PGC DASH response was empty; retrying once before DURL fallback"
                    );
                    tokio::time::sleep(BILIBILI_DASH_RETRY_DELAY).await;
                    client.get_dash_pgcurl(request).await
                } else {
                    first_dash_resp
                };
                let dash_resp = match dash_resp {
                    Ok(dash_resp) if dash_resp.dash.is_some() => Some(dash_resp),
                    Ok(_) => None,
                    Err(error) if is_bilibili_pgc_dash_unavailable(&error) => None,
                    Err(error) => return Err(error.into()),
                };

                let mut metadata = PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                    epid: Some(epid),
                    cid: Some(cid),
                    ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Pgc)
                });
                let mut subtitles = Vec::new();

                let subtitle_request =
                    bilibili_subtitles_request(&sanitized_cookies, 0, String::new(), cid);
                match client.get_subtitles(subtitle_request).await {
                    Ok(subtitle_resp) => {
                        subtitles = subtitle_resp
                            .subtitles
                            .into_iter()
                            .map(|(name, url)| {
                                bilibili_subtitle_track(
                                    provider_instance_name,
                                    &content_descriptor,
                                    &name,
                                    url,
                                )
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

                let duration_seconds = dash_resp
                    .as_ref()
                    .and_then(|resp| resp.dash.as_ref())
                    .map(|dash| dash.duration);
                if let Some(dash) = dash_resp.as_ref().and_then(|resp| resp.dash.as_ref()) {
                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.min_buffer_time = Some(dash.min_buffer_time);
                    }
                }

                let mut playback_infos = HashMap::new();
                let danmakus = vec![bilibili_vod_danmaku_track(
                    provider_instance_name,
                    &content_descriptor,
                    cid,
                )];

                let default_mode = if let Some(dash_resp) = dash_resp {
                    let dash = dash_resp.dash.as_ref().ok_or_else(|| {
                        ProviderError::ApiError(
                            "Bilibili PGC playback response did not include DASH streams"
                                .to_string(),
                        )
                    })?;
                    let primary_dash = dash_manifest_from_upstream(dash);
                    let hevc_dash = dash_resp
                        .hevc_dash
                        .as_ref()
                        .filter(|dash| !dash.video_streams.is_empty())
                        .map(dash_manifest_from_upstream);
                    let (dash_infos, default_mode) = bilibili_dash_playback_infos(
                        &mut metadata,
                        &content_descriptor,
                        &primary_dash,
                        hevc_dash.as_ref(),
                        BilibiliDashPlaybackOptions {
                            provider_instance_name,
                            subtitles: &subtitles,
                            danmakus: &danmakus,
                            client_profile: ctx.playback_client_profile(),
                        },
                    )?;
                    playback_infos.extend(dash_infos);
                    default_mode
                } else {
                    let request = bilibili_pgc_url_request(&sanitized_cookies, epid, cid, 80);
                    let pgc_resp = client.get_pgcurl(request).await?;
                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.fallback_format = Some("durl".to_string());
                        metadata.quality = Some(pgc_resp.current_quality);
                    }
                    let durl_swarm_id = super::provider_p2p_swarm_id(
                        Self::NAME,
                        provider_instance_name,
                        "media",
                        &format!(
                            "{content_descriptor}:durl:quality:{}",
                            pgc_resp.current_quality
                        ),
                    );
                    playback_infos.insert(
                        "mp4".to_string(),
                        mode_info(
                            vec![bilibili_durl_media(
                                "MP4",
                                pgc_resp.segments.into_iter().map(|segment| {
                                    (segment.url, segment.backup_urls, segment.duration_millis)
                                }),
                                durl_swarm_id,
                            )?],
                            subtitles,
                            danmakus,
                        ),
                    );
                    "mp4".to_string()
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: crate::models::SourceProvider::Bilibili,
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds,
                    playback_kind: Some(crate::models::PlaybackKind::Regular),
                    metadata: Some(metadata),
                })
            }

            BilibiliSourceConfig::Live(config) => {
                let room_id = config.room_id;

                let request = bilibili_live_streams_request(
                    sanitized_cookies.clone(),
                    room_id,
                    bilibili_live_uses_hls(ctx.playback_client_profile()),
                );
                let page_request = bilibili_parse_live_page_request(BilibiliParseLivePageRequest {
                    cookies: sanitized_cookies,
                    room_id,
                });
                let (live_resp, live_page) = tokio::join!(
                    client.get_live_streams(request),
                    client.parse_live_page(page_request),
                );
                let live_resp = live_resp?;
                let live_started_at = match live_page {
                    Ok(page) => page.live_started_at,
                    Err(error) => {
                        tracing::warn!(
                            room_id,
                            error = %error,
                            "Failed to fetch Bilibili live start time"
                        );
                        None
                    }
                };

                let metadata = PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                    room_id: Some(room_id),
                    live_started_at,
                    is_live: true,
                    is_currently_live: Some(true),
                    ..BilibiliPlaybackMetadata::new(BilibiliPlaybackKind::Live)
                });
                // The versioned result is keyed by upstream content. Room and
                // playlist identifiers belong to the live danmaku route, so
                // they are attached after cache lookup for each request.
                let playback_infos = bilibili_live_playback_infos(live_resp.live_streams, &[])?;

                let default_mode = default_bilibili_live_mode(&playback_infos);

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: crate::models::SourceProvider::Bilibili,
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds: None,
                    playback_kind: Some(crate::models::PlaybackKind::Live),
                    metadata: Some(metadata),
                })
            }
        }
    }
}

fn bilibili_live_playback_infos(
    mut live_streams: Vec<bilibili_upstream::LiveStream>,
    danmakus: &[PlaybackDanmaku],
) -> Result<HashMap<String, PlaybackInfo>, ProviderError> {
    live_streams.sort_by(|left, right| {
        right
            .quality
            .cmp(&left.quality)
            .then_with(|| {
                bilibili_live_format_rank(&left.format)
                    .cmp(&bilibili_live_format_rank(&right.format))
            })
            .then_with(|| {
                bilibili_live_codec_rank(&left.codec).cmp(&bilibili_live_codec_rank(&right.codec))
            })
    });
    let mut route_hosts = Vec::new();
    for stream in &live_streams {
        for url in &stream.urls {
            if !route_hosts.contains(&url.host) {
                route_hosts.push(url.host.clone());
            }
        }
    }

    let playback_infos = route_hosts
        .iter()
        .enumerate()
        .filter_map(|(route_index, route_host)| {
            let medias = live_streams
                .iter()
                .flat_map(|stream| {
                    stream
                        .urls
                        .iter()
                        .filter(|&candidate| candidate.host == *route_host)
                        .map(|candidate| {
                            let mut media = playback_media(
                                stream.quality_name.clone(),
                                bilibili_live_media_format(stream).to_string(),
                                candidate.expires_at,
                                None,
                                PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct {
                                    url: candidate.url.clone(),
                                    headers: bilibili_live_headers(),
                                }),
                            );
                            media.metadata = Some(crate::models::PlaybackMediaMetadata {
                                resolution: None,
                                bitrate: None,
                                codec: (!stream.codec.is_empty()).then(|| stream.codec.clone()),
                                fps: None,
                            });
                            media
                        })
                })
                .collect::<Vec<_>>();
            if medias.is_empty() {
                return None;
            }
            let mode_name = if route_index == 0 {
                "main".to_string()
            } else {
                format!("backup_{route_index}")
            };
            Some((
                mode_name,
                PlaybackInfo {
                    thumbnail: None,
                    medias,
                    default_media_index: Some(0),
                    subtitles: Vec::new(),
                    default_subtitle_index: None,
                    danmakus: danmakus.to_vec(),
                    default_danmaku_index: (!danmakus.is_empty()).then_some(0),
                },
            ))
        })
        .collect::<HashMap<_, _>>();

    if playback_infos.is_empty() {
        return Err(ProviderError::NotFound);
    }

    Ok(playback_infos)
}

fn bilibili_live_media_format(stream: &bilibili_upstream::LiveStream) -> &'static str {
    if stream.protocol == "http_stream" {
        "flv"
    } else {
        "m3u8"
    }
}

const fn bilibili_live_format_rank(format: &str) -> u8 {
    match format.as_bytes() {
        b"ts" => 0,
        b"fmp4" => 1,
        _ => 2,
    }
}

const fn bilibili_live_codec_rank(codec: &str) -> u8 {
    match codec.as_bytes() {
        b"avc" => 0,
        b"hevc" => 1,
        _ => 2,
    }
}

fn default_bilibili_live_mode(playback_infos: &HashMap<String, PlaybackInfo>) -> String {
    playback_infos
        .contains_key("main")
        .then(|| "main".to_string())
        .or_else(|| playback_infos.keys().min().cloned())
        .unwrap_or_else(|| "main".to_string())
}
