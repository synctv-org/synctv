//! Bilibili `MediaProvider` Adapter
//!
//! Adapter that calls `BilibiliClient` to implement `MediaProvider` trait

use super::{
    access::BilibiliAccess,
    provider_client::{create_remote_bilibili_client, BilibiliClientArc, ProviderClientManager},
    DirectoryItem, DirectoryItemSourceConfig, DirectoryItemThumbnail, DynamicListQuery,
    DynamicListResult, DynamicPagination, DynamicPlaylistProvider, ItemType, MediaProvider,
    NextPlayItem, PlaybackInfo, PlaybackResult, PreparedSourceConfig, ProviderContext,
    ProviderCredentialDependency, ProviderError, SourceConfig, SourceCover,
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
    BilibiliDashSegmentBase, BilibiliDashVideoStream, BilibiliPlaybackMetadata,
    PlaybackBilibiliDanmaku, PlaybackBilibiliMedia, PlaybackBilibiliSubtitle, PlaybackDanmaku,
    PlaybackDanmakuProvider, PlaybackExternalDanmaku, PlaybackExternalSubtitle, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::models::{
    normalize_provider_instance_name, validate_provider_instance_name, BilibiliHistoryType,
    BilibiliMediaSourceConfig as BilibiliSourceConfig, BilibiliPgcTimelineType,
    BilibiliPlaylistSource, BilibiliPlaylistSourceConfig, BilibiliTarget, MediaSourceConfig,
    PlayMode, PlaylistSourceConfig, ProviderCredential, ProviderTarget, UserId,
    UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use crate::service::RemoteProviderManager;

use super::upstream_transport::bilibili as bilibili_upstream;

pub const DASH_MANIFEST_METADATA_KEY: &str = "bilibili_dash_manifests";
pub const LIVE_DANMAKU_FORMAT: &str = "synctv-bilibili-live";
pub const LIVE_DANMAKU_TRACK_NAME: &str = "Bilibili Live Danmaku";
const SMS_LOGIN_SESSION_TTL_SECONDS: i64 = 10 * 60;
const SMS_LOGIN_SESSION_VERSION: &str = "v2";
const SMS_LOGIN_DOMAIN_SEPARATOR: &[u8] = b"synctv-bilibili-sms-login";
const SMS_LOGIN_TOKEN_NONCE_SIZE: usize = 12;
type HmacSha256 = Hmac<sha2::Sha256>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct BilibiliHistoryCursor {
    max: u64,
    view_at: i64,
    business: String,
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

#[derive(Debug, Clone)]
pub struct BilibiliLiveDanmuInfoRequest {
    pub room_id: u64,
    pub cookies: HashMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct BilibiliLiveDanmuHost {
    pub host: String,
    pub port: u32,
    pub wss_port: u32,
    pub ws_port: u32,
}

#[derive(Debug, Clone)]
pub struct BilibiliLiveDanmuInfoResponse {
    pub token: String,
    pub host_list: Vec<BilibiliLiveDanmuHost>,
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

fn bilibili_live_danmu_info_request(
    req: BilibiliLiveDanmuInfoRequest,
) -> bilibili_upstream::GetLiveDanmuInfoReq {
    bilibili_upstream::GetLiveDanmuInfoReq {
        cookies: req.cookies,
        room_id: req.room_id,
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
        BilibiliAccess {
            cookies: HashMap::new(),
            credential_cache_partition: "anon".to_string(),
            authenticated: false,
            provider_instance_name: None,
        }
    }

    pub fn access_from_stored_credential(
        user_id: UserId,
        server_id: &str,
        credential: ProviderCredential,
        credential_revision: &str,
        provider_instance_name: Option<String>,
    ) -> Result<BilibiliAccess, ProviderError> {
        match credential {
            ProviderCredential::Bilibili { cookies } => Ok(BilibiliAccess {
                cookies,
                credential_cache_partition: format!(
                    "auth:{user_id}:{server_id}:{credential_revision}"
                ),
                authenticated: true,
                provider_instance_name,
            }),
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

    /// Get live danmaku server info for the WebSocket connection
    pub async fn get_live_danmu_info_with_context(
        &self,
        req: BilibiliLiveDanmuInfoRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<BilibiliLiveDanmuInfoResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .get_live_danmu_info(bilibili_live_danmu_info_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(BilibiliLiveDanmuInfoResponse {
            token: resp.token,
            host_list: resp
                .host_list
                .into_iter()
                .map(|host| BilibiliLiveDanmuHost {
                    host: host.host,
                    port: host.port,
                    wss_port: host.wss_port,
                    ws_port: host.ws_port,
                })
                .collect(),
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
        let (cookies, credential_cache_partition) = if config.shared() {
            let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?;
            resolve_optional_bilibili_cookies(ctx, *credential_owner_id).await?
        } else if let Some(user_id) = ctx.user_id() {
            resolve_optional_bilibili_cookies(ctx, *user_id).await?
        } else {
            (HashMap::new(), "anonymous".to_string())
        };

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

fn dash_playback_urls(
    dash: &BilibiliDashManifest,
    context: &str,
) -> Result<Vec<String>, ProviderError> {
    non_empty_playback_urls(
        dash.video_streams
            .iter()
            .flat_map(|stream| {
                std::iter::once(stream.base_url.clone()).chain(stream.backup_urls.clone())
            })
            .chain(dash.audio_streams.iter().flat_map(|stream| {
                std::iter::once(stream.base_url.clone()).chain(stream.backup_urls.clone())
            })),
        context,
    )
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

    if !dash.video_streams.is_empty() {
        xml.push_str(r#"<AdaptationSet id="1" contentType="video" segmentAlignment="true" startWithSAP="1">"#);
        for stream in &dash.video_streams {
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
                r#"<Representation id="video-{}-{}" mimeType="{}" codecs="{}" width="{}" height="{}" bandwidth="{}" startWithSAP="{}"{}{}><Label>{}</Label>{}{}</Representation>"#,
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

    if !dash.audio_streams.is_empty() {
        xml.push_str(r#"<AdaptationSet id="2" contentType="audio" segmentAlignment="true" startWithSAP="1">"#);
        for stream in &dash.audio_streams {
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
            let sampling_rate_attr = if stream.audio_sampling_rate == 0 {
                String::new()
            } else {
                format!(r#" audioSamplingRate="{}""#, stream.audio_sampling_rate)
            };
            let _ = write!(
                xml,
                r#"<Representation id="audio-{}-{}" mimeType="{}" codecs="{}" bandwidth="{}" startWithSAP="{}"{}><Label>{}</Label>{}{}</Representation>"#,
                stream.id,
                xml_escape(&stream.codecs),
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

fn mark_bilibili_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // DASH/MPD modes keep both direct and proxy manifests: app clients can
    // apply the returned Bilibili headers to the manifest and segment requests,
    // while proxy siblings remain as a server-mediated fallback.
    let original_default_mode = result.default_mode.clone();
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(mode_name, info)| (mode_name.clone(), info.clone()))
        .collect::<Vec<_>>();

    for (mode_name, original_info) in original_modes {
        if original_info.medias.is_empty() || mode_name.starts_with("proxy_") {
            continue;
        }

        let use_mpd_manifest = original_info
            .medias
            .first()
            .is_some_and(|media| media.format == "mpd")
            && has_dash_manifest_metadata(result, &mode_name);

        if let Some(info) = result.playback_infos.get_mut(&mode_name) {
            if use_mpd_manifest {
                let source_media = original_info.medias.first();
                info.medias = vec![playback_media(
                    source_media.map_or_else(|| mode_name.clone(), |media| media.name.clone()),
                    source_media.map_or_else(|| "mpd".to_string(), |media| media.format.clone()),
                    source_media.and_then(|media| media.expire_at.map(|dt| dt.timestamp())),
                    PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::DirectDashManifest {
                        version: version.to_string(),
                        expires_at,
                        mode_name: mode_name.clone(),
                        headers: source_media
                            .map_or_else(bilibili_headers, PlaybackMedia::upstream_headers),
                    }),
                )];
            }
        }

        let proxy_mode_name = format!("proxy_{mode_name}");
        if result.playback_infos.contains_key(&proxy_mode_name) {
            continue;
        }

        let mut proxy_info = original_info.clone();
        if use_mpd_manifest {
            let source_media = original_info.medias.first();
            proxy_info.medias = vec![playback_media(
                source_media.map_or_else(|| mode_name.clone(), |media| media.name.clone()),
                source_media.map_or_else(|| "mpd".to_string(), |media| media.format.clone()),
                source_media.and_then(|media| media.expire_at.map(|dt| dt.timestamp())),
                PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::ProxyDashManifest {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                }),
            )];
        } else {
            let proxy_is_hls = super::playback_info_is_hls(&mode_name, &original_info);
            proxy_info.medias = original_info
                .medias
                .iter()
                .enumerate()
                .filter_map(|(url_index, media)| {
                    let url = media.upstream_url()?.to_string();
                    Some(playback_media(
                        media.name.clone(),
                        media.format.clone(),
                        media.expire_at.map(|dt| dt.timestamp()),
                        PlaybackMediaProvider::Bilibili(if proxy_is_hls {
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
                        }),
                    ))
                })
                .collect();
        }
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name().to_string(),
                language: subtitle.language().to_string(),
                format: subtitle.format().to_string(),
                provider: PlaybackSubtitleProvider::Bilibili(PlaybackBilibiliSubtitle {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
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
                    _ => PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::File {
                        version: version.to_string(),
                        expires_at,
                        danmaku_index,
                        url: danmaku.upstream_url().unwrap_or_default().to_string(),
                        headers: danmaku.upstream_headers(),
                    }),
                };
                PlaybackDanmaku {
                    name: danmaku.name().to_string(),
                    format: danmaku.format().map(ToString::to_string),
                    provider,
                }
            })
            .collect();

        result.playback_infos.insert(proxy_mode_name, proxy_info);
    }

    let proxy_default_mode = format!("proxy_{original_default_mode}");
    result.default_mode = if result.playback_infos.contains_key(&original_default_mode) {
        original_default_mode
    } else if result.playback_infos.contains_key(&proxy_default_mode) {
        proxy_default_mode
    } else {
        original_default_mode
    };
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
) -> Result<(String, Duration), ProviderError> {
    match config {
        BilibiliSourceConfig::Video(config) => {
            let video_key = BilibiliVideoIdentifier::parse(config.bvid.as_deref(), config.aid)?
                .cache_key_part();
            Ok((
                format!(
                    "playback:video:{video_key}:{}:{credential_cache_partition}",
                    config.cid
                ),
                Duration::from_hours(2),
            ))
        }
        BilibiliSourceConfig::Pgc(config) => Ok((
            format!(
                "playback:pgc:{}:{}:{credential_cache_partition}",
                config.epid, config.cid
            ),
            Duration::from_hours(2),
        )),
        BilibiliSourceConfig::Live(config) => Ok((
            format!(
                "playback:live:{}:{credential_cache_partition}",
                config.room_id
            ),
            Duration::from_mins(2),
        )),
    }
}

async fn resolve_optional_bilibili_cookies(
    ctx: &ProviderContext<'_>,
    credential_owner_id: UserId,
) -> Result<(HashMap<String, String>, String), ProviderError> {
    let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
        ProviderError::Internal(
            "provider_access_service not available in ProviderContext".to_string(),
        )
    })?;
    let access = access_service
        .bilibili_access(credential_owner_id, ctx.request_context())
        .await?;
    Ok((access.cookies, access.credential_cache_partition))
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
        let user_id = if config.shared {
            ctx.credential_owner_id()
        } else {
            ctx.user_id()
        }
        .ok_or_else(|| {
            ProviderError::Internal("Bilibili playlist user context is unavailable".to_string())
        })?;
        let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
            ProviderError::Internal(
                "provider_access_service not available in ProviderContext".to_string(),
            )
        })?;
        let access = access_service
            .bilibili_access(*user_id, ctx.request_context())
            .await?;
        if Self::playlist_requires_credential(&config.source) && !access.authenticated {
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
                cookies: access.cookies,
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
                cookies: access.cookies,
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
                cookies: access.cookies,
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
                cookies: access.cookies,
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
                cookies: access.cookies,
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
                }),
            )),
            BilibiliTarget::PgcEpisode { epid, cid } => Ok(MediaSourceConfig::Bilibili(
                BilibiliSourceConfig::Pgc(crate::models::BilibiliPgcSourceConfig {
                    epid: *epid,
                    cid: *cid,
                    shared: config.shared,
                }),
            )),
            BilibiliTarget::Live { room_id } => Ok(MediaSourceConfig::Bilibili(
                BilibiliSourceConfig::Live(crate::models::BilibiliLiveSourceConfig {
                    room_id: *room_id,
                    shared: config.shared,
                }),
            )),
        }
    }

    fn directory_source_config_for_target(
        config: &BilibiliPlaylistSourceConfig,
        target: &ProviderTarget,
    ) -> Result<DirectoryItemSourceConfig, ProviderError> {
        let ProviderTarget::Bilibili(target) = target else {
            return Err(ProviderError::InvalidConfig(
                "Bilibili target is required".to_string(),
            ));
        };
        match target {
            BilibiliTarget::Video { bvid, aid } => Ok(DirectoryItemSourceConfig::Playlist(
                PlaylistSourceConfig::Bilibili(BilibiliPlaylistSourceConfig {
                    source: BilibiliPlaylistSource::VideoParts {
                        bvid: bvid.clone(),
                        aid: (*aid > 0).then_some(*aid),
                    },
                    shared: config.shared,
                }),
            )),
            BilibiliTarget::VideoPart { .. }
            | BilibiliTarget::PgcEpisode { .. }
            | BilibiliTarget::Live { .. } => Ok(DirectoryItemSourceConfig::Media(
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

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = BilibiliSourceConfig::from_media_config(source_config)?;

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

        let (cache_key, cache_ttl) = playback_cache_entry(config, &credential_cache_partition)?;

        Box::pin(super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            mark_bilibili_playback_resources,
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
        .await
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
        let (shared, required) = match source_config {
            SourceConfig::Media(config) => {
                let shared = BilibiliSourceConfig::from_media_config(config)?.shared();
                (shared, shared)
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                (
                    config.shared,
                    config.shared || Self::playlist_requires_credential(&config.source),
                )
            }
        };
        if shared {
            let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?;
            let dependency = if required {
                ProviderCredentialDependency::new(
                    Self::NAME,
                    credential_owner_id.to_string(),
                    bilibili_credential_server_id(),
                )
            } else {
                ProviderCredentialDependency::optional(
                    Self::NAME,
                    credential_owner_id.to_string(),
                    bilibili_credential_server_id(),
                )
            };
            return Ok(vec![dependency]);
        }

        let viewer_id = ctx.user_id().ok_or_else(|| {
            ProviderError::Internal("user_id not available in ProviderContext".to_string())
        })?;
        let dependency = if required {
            ProviderCredentialDependency::new(
                Self::NAME,
                viewer_id.to_string(),
                bilibili_credential_server_id(),
            )
        } else {
            ProviderCredentialDependency::optional(
                Self::NAME,
                viewer_id.to_string(),
                bilibili_credential_server_id(),
            )
        };
        Ok(vec![dependency])
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
                    Ok(DirectoryItem {
                        name: item.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target,
                        size: None,
                        thumbnail: (!item.cover.trim().is_empty())
                            .then_some(DirectoryItemThumbnail::Url(item.cover)),
                        description: (!description.is_empty()).then_some(description),
                        modified_at: (item.viewed_at > 0).then_some(item.viewed_at),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                items,
                pagination: DynamicPagination::Cursor {
                    cursor: Self::encode_history_cursor(response.cursor)?,
                },
                has_more: response.has_more,
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
                    Ok(DirectoryItem {
                        name,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target,
                        size: None,
                        thumbnail: (!cover.trim().is_empty())
                            .then_some(DirectoryItemThumbnail::Url(cover)),
                        description: (!description.is_empty()).then_some(description),
                        modified_at: (item.publish_at > 0).then_some(item.publish_at),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                has_more: start.saturating_add(items.len()) < total,
                items,
                pagination: DynamicPagination::Page { page },
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
                    Ok(DirectoryItem {
                        name: room.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target,
                        size: None,
                        thumbnail: (!room.cover.trim().is_empty())
                            .then_some(DirectoryItemThumbnail::Url(room.cover)),
                        description: (!description.is_empty()).then_some(description),
                        modified_at: None,
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                items,
                pagination: DynamicPagination::Page { page },
                has_more: response.has_more,
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
                    Ok(DirectoryItem {
                        name: part.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target,
                        size: None,
                        thumbnail: (!part.cover.trim().is_empty())
                            .then_some(DirectoryItemThumbnail::Url(part.cover)),
                        description: Some(format!(
                            "{}x{} · {}s",
                            part.width, part.height, part.duration_seconds
                        )),
                        modified_at: None,
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                has_more: start.saturating_add(items.len()) < total,
                items,
                pagination: DynamicPagination::Page { page },
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
                    Ok(DirectoryItem {
                        name: part.title,
                        item_type: ItemType::Media,
                        source_config: Some(Self::directory_source_config_for_target(
                            config, &target,
                        )?),
                        target,
                        size: None,
                        thumbnail: (!part.cover.trim().is_empty())
                            .then_some(DirectoryItemThumbnail::Url(part.cover)),
                        description: Some(format!(
                            "{}x{} · {}s",
                            part.width, part.height, part.duration_seconds
                        )),
                        modified_at: None,
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?;
            return Ok(DynamicListResult {
                has_more: start.saturating_add(items.len()) < total,
                items,
                pagination: DynamicPagination::Page { page },
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
                Ok(DirectoryItem {
                    name: item.title,
                    item_type,
                    source_config: Some(Self::directory_source_config_for_target(config, &target)?),
                    target,
                    size: None,
                    thumbnail: (!item.cover.trim().is_empty())
                        .then_some(DirectoryItemThumbnail::Url(item.cover)),
                    description: (!item.description.trim().is_empty()).then_some(item.description),
                    modified_at: (item.published_at > 0).then_some(item.published_at),
                })
            })
            .collect::<Result<Vec<_>, ProviderError>>()?;
        Ok(DynamicListResult {
            items,
            pagination: DynamicPagination::Page { page },
            has_more: response.has_more,
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
            let mut first: Option<DirectoryItem> = None;
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
            let mut first: Option<DirectoryItem> = None;
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
    use super::{BilibiliProvider, BilibiliSmsLoginSession, BilibiliSmsLoginTokenCodec};

    type TestResult<T = ()> = anyhow::Result<T>;

    fn provider_ok<T>(result: Result<T, super::ProviderError>) -> TestResult<T> {
        result.map_err(|error| anyhow::anyhow!(error.to_string()))
    }

    fn test_sms_login_secret() -> &'static [u8] {
        b"test-bilibili-sms-login-secret"
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

        let credential_owner_id = if config.shared {
            *ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?
        } else {
            *ctx.user_id().ok_or_else(|| {
                ProviderError::Internal("user_id not available in ProviderContext".to_string())
            })?
        };
        let (cookies, _) = resolve_optional_bilibili_cookies(ctx, credential_owner_id).await?;
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

    pub async fn get_hls_segment(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        target_url: String,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let headers = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .map_or_else(bilibili_headers, |info| {
                let headers = info
                    .medias
                    .first()
                    .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
                if headers.is_empty() {
                    bilibili_headers()
                } else {
                    headers
                }
            });
        super::playback_transport::transport_action_for_target_url(
            target_url,
            headers,
            range_header,
        )
    }

    pub async fn get_dash_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        manifest_mode: BilibiliDashManifestMode,
        request_context: Option<&super::ExecutionControl>,
        proxy_url_for: Option<&mut BilibiliDashProxyUrlMapper<'_>>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let dash = dash_manifest_from_metadata(&versioned.result, mode_name)?;
        let body = match manifest_mode {
            BilibiliDashManifestMode::Direct => {
                build_bilibili_mpd_manifest(&dash, |_index, url| url.to_string())?
            }
            BilibiliDashManifestMode::Proxy => {
                let proxy_url_for = proxy_url_for.ok_or_else(|| {
                    ProviderError::InvalidConfig(
                        "Proxy URL mapping is required for proxied DASH manifests".to_string(),
                    )
                })?;
                build_bilibili_mpd_manifest(&dash, proxy_url_for)?
            }
        };

        Ok(
            super::playback_transport::PlaybackTransportAction::DirectBody {
                body: body.into_bytes(),
                content_type: "application/dash+xml".to_string(),
                status: 200,
            },
        )
    }

    pub async fn get_dash_segment(
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
        let dash = dash_manifest_from_metadata(&versioned.result, mode_name)?;
        let urls = dash_playback_urls(&dash, "DASH segment")?;
        let url = urls.get(url_index).ok_or(ProviderError::NotFound)?;
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.clone(),
                headers: bilibili_headers(),
                range_header: range_header.map(ToString::to_string),
            },
        )
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

pub type BilibiliDashProxyUrlMapper<'a> = dyn FnMut(usize, &str) -> String + Send + 'a;

fn playback_media(
    name: String,
    format: String,
    expires_at: Option<i64>,
    provider: PlaybackMediaProvider,
) -> PlaybackMedia {
    PlaybackMedia {
        name,
        format,
        expire_at: expires_at.and_then(|timestamp| chrono::DateTime::from_timestamp(timestamp, 0)),
        metadata: None,
        provider,
    }
}

fn bilibili_subtitle_track(name: String, url: String) -> PlaybackSubtitle {
    PlaybackSubtitle {
        language: name.clone(),
        name,
        format: "json".to_string(),
        provider: PlaybackSubtitleProvider::External(PlaybackExternalSubtitle {
            url,
            headers: bilibili_headers(),
        }),
    }
}

#[allow(clippy::needless_pass_by_value)]
fn bilibili_direct_medias(
    name_prefix: &str,
    urls: Vec<String>,
    format: &str,
    expires_at: Option<i64>,
    headers: HashMap<String, String>,
) -> Vec<PlaybackMedia> {
    urls.into_iter()
        .enumerate()
        .map(|(index, url)| {
            let name = if index == 0 {
                name_prefix.to_string()
            } else {
                format!("{name_prefix} {}", index + 1)
            };
            playback_media(
                name,
                format.to_string(),
                expires_at,
                PlaybackMediaProvider::Bilibili(PlaybackBilibiliMedia::Direct {
                    url,
                    headers: headers.clone(),
                }),
            )
        })
        .collect()
}

fn bilibili_live_headers() -> HashMap<String, String> {
    let mut headers = bilibili_headers();
    headers.insert(
        "Referer".to_string(),
        "https://live.bilibili.com".to_string(),
    );
    headers
}

fn bilibili_live_danmaku_track(ctx: &ProviderContext<'_>) -> Option<PlaybackDanmaku> {
    let room_id = ctx.room_id()?;
    let media_id = ctx.media_id()?;
    Some(PlaybackDanmaku {
        name: LIVE_DANMAKU_TRACK_NAME.to_string(),
        format: Some(LIVE_DANMAKU_FORMAT.to_string()),
        provider: PlaybackDanmakuProvider::Bilibili(PlaybackBilibiliDanmaku::Live {
            room_id: *room_id,
            media_id: *media_id,
        }),
    })
}

fn bilibili_vod_danmaku_track(cid: u64) -> PlaybackDanmaku {
    PlaybackDanmaku {
        name: "Bilibili danmaku".to_string(),
        format: Some("xml".to_string()),
        provider: PlaybackDanmakuProvider::External(PlaybackExternalDanmaku {
            url: format!("https://api.bilibili.com/x/v1/dm/list.so?oid={cid}"),
            headers: bilibili_headers(),
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
) -> bilibili_upstream::GetLiveStreamsReq {
    bilibili_upstream::GetLiveStreamsReq {
        cid: room_id,
        hls: true,
        cookies,
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

                let request = bilibili_dash_video_url_request(
                    &sanitized_cookies,
                    aid,
                    request_bvid.clone(),
                    cid,
                );
                let dash_resp = match client.get_dash_video_url(request).await {
                    Ok(dash_resp) if dash_resp.dash.is_some() => Some(dash_resp),
                    Ok(_) => None,
                    Err(error) if is_bilibili_video_dash_unavailable(&error) => None,
                    Err(error) => return Err(error.into()),
                };

                let mut metadata = PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                    content_type: Some("video".to_string()),
                    bvid,
                    aid: Some(aid),
                    cid: Some(cid),
                    ..Default::default()
                });
                let mut subtitles = Vec::new();

                let subtitle_request =
                    bilibili_subtitles_request(&sanitized_cookies, aid, request_bvid.clone(), cid);
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
                            bvid = %request_bvid, aid = %aid, cid = %cid, error = %e,
                            "Failed to fetch Bilibili subtitles for video, continuing without subtitles"
                        );
                    }
                }

                let duration_seconds = dash_resp
                    .as_ref()
                    .and_then(|resp| resp.dash.as_ref())
                    .map(|dash| dash.duration);
                if let Some(d) = dash_resp.as_ref().and_then(|resp| resp.dash.as_ref()) {
                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.min_buffer_time = Some(d.min_buffer_time);
                    }
                }

                let expires_at = Some(crate::SystemClock.now().timestamp() + 2 * 3600);
                let mut playback_infos = HashMap::new();
                let danmakus = vec![bilibili_vod_danmaku_track(cid)];

                let default_mode = if let Some(dash_resp) = dash_resp {
                    let dash = dash_resp.dash.as_ref().ok_or_else(|| {
                        ProviderError::ApiError(
                            "Bilibili video playback response did not include DASH streams"
                                .to_string(),
                        )
                    })?;
                    let dash = dash_manifest_from_upstream(dash);
                    insert_dash_manifest_metadata(
                        &mut metadata,
                        BilibiliDashManifestSlot::Dash,
                        dash.clone(),
                    );
                    let dash_urls = dash_playback_urls(&dash, "video DASH")?;
                    let hevc_urls = dash_resp
                        .hevc_dash
                        .as_ref()
                        .filter(|dash| !dash.video_streams.is_empty())
                        .map(|dash| {
                            let dash = dash_manifest_from_upstream(dash);
                            insert_dash_manifest_metadata(
                                &mut metadata,
                                BilibiliDashManifestSlot::Hevc,
                                dash.clone(),
                            );
                            dash_playback_urls(&dash, "video HEVC DASH")
                        })
                        .transpose()?;

                    playback_infos.insert(
                        "dash".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "DASH",
                                dash_urls,
                                "mpd",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles.clone(),
                            danmakus.clone(),
                        ),
                    );
                    if let Some(hevc_urls) = hevc_urls {
                        playback_infos.insert(
                            "hevc".to_string(),
                            mode_info(
                                bilibili_direct_medias(
                                    "HEVC",
                                    hevc_urls,
                                    "mpd",
                                    expires_at,
                                    bilibili_headers(),
                                ),
                                subtitles,
                                danmakus,
                            ),
                        );
                    }
                    "dash".to_string()
                } else {
                    let request =
                        bilibili_video_url_request(&sanitized_cookies, aid, request_bvid, cid, 80);
                    let video_resp = client.get_video_url(request).await?;
                    let video_urls = non_empty_playback_urls(
                        video_resp
                            .segments
                            .iter()
                            .map(|segment| segment.url.clone()),
                        "video durl",
                    )?;

                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.fallback_format = Some("durl".to_string());
                        metadata.quality = Some(video_resp.current_quality);
                    }
                    playback_infos.insert(
                        "mp4".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "MP4",
                                video_urls,
                                "mp4",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles,
                            danmakus,
                        ),
                    );
                    "mp4".to_string()
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: Self::NAME.to_string(),
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds,
                    is_live: Some(false),
                    metadata: Some(metadata),
                })
            }

            BilibiliSourceConfig::Pgc(config) => {
                let epid = config.epid;
                let cid = config.cid;

                let request = bilibili_dash_pgc_url_request(&sanitized_cookies, epid, cid);
                let dash_resp = match client.get_dash_pgcurl(request).await {
                    Ok(dash_resp) if dash_resp.dash.is_some() => Some(dash_resp),
                    Ok(_) => None,
                    Err(error) if is_bilibili_pgc_dash_unavailable(&error) => None,
                    Err(error) => return Err(error.into()),
                };

                let mut metadata = PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                    content_type: Some("pgc".to_string()),
                    epid: Some(epid),
                    cid: Some(cid),
                    ..Default::default()
                });
                let mut subtitles = Vec::new();

                let subtitle_request =
                    bilibili_subtitles_request(&sanitized_cookies, 0, String::new(), cid);
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

                let duration_seconds = dash_resp
                    .as_ref()
                    .and_then(|resp| resp.dash.as_ref())
                    .map(|dash| dash.duration);
                if let Some(d) = dash_resp.as_ref().and_then(|resp| resp.dash.as_ref()) {
                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.min_buffer_time = Some(d.min_buffer_time);
                    }
                }

                let expires_at = Some(crate::SystemClock.now().timestamp() + 2 * 3600);
                let mut playback_infos = HashMap::new();
                let danmakus = vec![bilibili_vod_danmaku_track(cid)];

                let default_mode = if let Some(dash_resp) = dash_resp {
                    let dash = dash_resp.dash.as_ref().ok_or_else(|| {
                        ProviderError::ApiError(
                            "Bilibili PGC playback response did not include DASH streams"
                                .to_string(),
                        )
                    })?;
                    let dash = dash_manifest_from_upstream(dash);
                    insert_dash_manifest_metadata(
                        &mut metadata,
                        BilibiliDashManifestSlot::Dash,
                        dash.clone(),
                    );
                    let pgc_urls = dash_playback_urls(&dash, "PGC DASH")?;
                    let hevc_urls = dash_resp
                        .hevc_dash
                        .as_ref()
                        .filter(|dash| !dash.video_streams.is_empty())
                        .map(|dash| {
                            let dash = dash_manifest_from_upstream(dash);
                            insert_dash_manifest_metadata(
                                &mut metadata,
                                BilibiliDashManifestSlot::Hevc,
                                dash.clone(),
                            );
                            dash_playback_urls(&dash, "PGC HEVC DASH")
                        })
                        .transpose()?;

                    playback_infos.insert(
                        "dash".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "DASH",
                                pgc_urls,
                                "mpd",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles.clone(),
                            danmakus.clone(),
                        ),
                    );
                    if let Some(hevc_urls) = hevc_urls {
                        playback_infos.insert(
                            "hevc".to_string(),
                            mode_info(
                                bilibili_direct_medias(
                                    "HEVC",
                                    hevc_urls,
                                    "mpd",
                                    expires_at,
                                    bilibili_headers(),
                                ),
                                subtitles,
                                danmakus,
                            ),
                        );
                    }
                    "dash".to_string()
                } else {
                    let request = bilibili_pgc_url_request(&sanitized_cookies, epid, cid, 80);
                    let pgc_resp = client.get_pgcurl(request).await?;
                    let pgc_urls = non_empty_playback_urls(
                        pgc_resp.segments.iter().map(|segment| segment.url.clone()),
                        "PGC durl",
                    )?;

                    if let Some(metadata) = metadata.as_bilibili_mut() {
                        metadata.fallback_format = Some("durl".to_string());
                        metadata.quality = Some(pgc_resp.current_quality);
                    }
                    playback_infos.insert(
                        "mp4".to_string(),
                        mode_info(
                            bilibili_direct_medias(
                                "MP4",
                                pgc_urls,
                                "mp4",
                                expires_at,
                                bilibili_headers(),
                            ),
                            subtitles,
                            danmakus,
                        ),
                    );
                    "mp4".to_string()
                };

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: Self::NAME.to_string(),
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds,
                    is_live: Some(false),
                    metadata: Some(metadata),
                })
            }

            BilibiliSourceConfig::Live(config) => {
                let room_id = config.room_id;

                let request = bilibili_live_streams_request(sanitized_cookies, room_id);
                let live_resp = client.get_live_streams(request).await?;

                let mut playback_infos = HashMap::new();
                let metadata = PlaybackMetadata::Bilibili(BilibiliPlaybackMetadata {
                    content_type: Some("live".to_string()),
                    room_id: Some(room_id),
                    ..Default::default()
                });
                let live_expires_at = Some(crate::SystemClock.now().timestamp() + 120);

                for stream in live_resp.live_streams {
                    let quality_name = if stream.desc.is_empty() {
                        format!("quality_{}", stream.quality)
                    } else {
                        format!("{}_{}", stream.desc, stream.quality)
                    };
                    playback_infos.insert(
                        quality_name,
                        mode_info(
                            bilibili_direct_medias(
                                "Live HLS",
                                stream.urls,
                                "hls",
                                live_expires_at,
                                bilibili_live_headers(),
                            ),
                            Vec::new(),
                            bilibili_live_danmaku_track(ctx).into_iter().collect(),
                        ),
                    );
                }

                let default_mode = playback_infos
                    .keys()
                    .min()
                    .cloned()
                    .unwrap_or_else(|| "direct".to_string());

                Ok(PlaybackResult {
                    playback_infos,
                    default_mode,
                    provider: Self::NAME.to_string(),
                    provider_instance_name: provider_instance_name.map(str::to_string),
                    duration_seconds: None,
                    is_live: Some(true),
                    metadata: Some(metadata),
                })
            }
        }
    }
}
