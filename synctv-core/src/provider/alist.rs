//! Alist `MediaProvider` Adapter
//!
//! Adapter that calls `AlistProviderClient` to implement `MediaProvider` trait.
//! `ProviderClient` abstracts local/remote implementation, so `MediaProvider` doesn't need to know.

use super::upstream_transport::alist as alist_upstream;
use super::{
    access::{AlistAccess, AlistBinding},
    provider_client::{create_remote_alist_client, AlistClientArc, ProviderClientManager},
    DirectoryItem, DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult,
    DynamicPagination, DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem,
    PlaybackClientProfile, PlaybackInfo, PlaybackResult, PlaybackStreamPreference,
    PlaybackSubtitlePreference, PreparedSourceConfig, ProviderContext,
    ProviderCredentialDependency, ProviderError, SourceConfig, SourceCover,
};
use crate::models::media::{
    AlistPlaybackMetadata, AlistTranscodingTaskMetadata, AlistVideoPreviewMetadata,
    PlaybackAlistMedia, PlaybackAlistSubtitle, PlaybackExternalSubtitle, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::models::{
    normalize_provider_instance_name, validate_provider_instance_name, AlistMediaSourceConfig,
    AlistPlaylistSourceConfig, MediaSourceConfig, ProviderCredential, UserId,
    UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use crate::service::RemoteProviderManager;
use crate::validation::validate_path_for_traversal;
use async_trait::async_trait;
use hmac::{Hmac, KeyInit, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const LIST_PAGE_SIZE: usize = 50;
const SHUFFLE_MAX_ITEMS: usize = 200;
const RELATED_SUBTITLE_FETCH_LIMIT: usize = 32;
const ALIST_PASSWORD_HASH_SALT: &str = "https://github.com/alist-org/alist";

#[derive(Debug, Clone, Copy)]
pub struct AlistHlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct AlistFileInfo {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub raw_url: String,
    pub provider: String,
    pub thumb: String,
    pub related: Vec<AlistRelatedFile>,
}

impl From<alist_upstream::FsGetResp> for AlistFileInfo {
    fn from(data: alist_upstream::FsGetResp) -> Self {
        Self {
            name: data.name,
            size: data.size,
            is_dir: data.is_dir,
            raw_url: data.raw_url,
            provider: data.provider,
            thumb: data.thumb,
            related: data
                .related
                .into_iter()
                .map(|related| AlistRelatedFile {
                    name: related.name,
                    is_dir: related.is_dir,
                    raw_url: related.raw_url,
                    provider: related.provider,
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AlistRelatedFile {
    pub name: String,
    pub is_dir: bool,
    pub raw_url: String,
    pub provider: String,
}

#[derive(Debug, Clone)]
pub struct AlistVideoPreview {
    pub transcoding_tasks: Vec<AlistTranscodingTask>,
    pub subtitle_tasks: Vec<AlistSubtitleTask>,
    pub drive_id: String,
    pub file_id: String,
    pub provider: String,
    pub category: String,
    pub duration: f64,
    pub width: u64,
    pub height: u64,
}

impl AlistVideoPreview {
    #[must_use]
    pub fn from_fs_other_resp(other_data: alist_upstream::FsOtherResp) -> Option<Self> {
        other_data.video_preview_play_info.map(|preview| Self {
            transcoding_tasks: preview
                .live_transcoding_task_list
                .into_iter()
                .map(|task| AlistTranscodingTask {
                    template_name: task.template_name,
                    template_id: task.template_id,
                    template_width: task.template_width,
                    template_height: task.template_height,
                    stage: task.stage,
                    status: task.status,
                    url: task.url,
                })
                .collect(),
            subtitle_tasks: preview
                .live_transcoding_subtitle_task_list
                .into_iter()
                .map(|sub| AlistSubtitleTask {
                    language: sub.language,
                    url: sub.url,
                })
                .collect(),
            drive_id: other_data.drive_id,
            file_id: other_data.file_id,
            provider: other_data.provider,
            category: preview.category,
            duration: preview.meta.as_ref().map_or(0.0, |m| m.duration),
            width: preview.meta.as_ref().map_or(0, |m| m.width),
            height: preview.meta.as_ref().map_or(0, |m| m.height),
        })
    }
}

#[derive(Debug, Clone)]
pub struct AlistTranscodingTask {
    pub template_name: String,
    pub template_id: String,
    pub template_width: u64,
    pub template_height: u64,
    pub stage: String,
    pub status: String,
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct AlistSubtitleTask {
    pub language: String,
    pub url: String,
}

#[async_trait]
trait AlistClientExt {
    async fn get_video_preview(
        &self,
        host: &str,
        token: &str,
        path: &str,
        password: Option<&str>,
    ) -> Result<Option<AlistVideoPreview>, ProviderError>;
}

#[async_trait]
impl AlistClientExt for AlistClientArc {
    async fn get_video_preview(
        &self,
        host: &str,
        token: &str,
        path: &str,
        password: Option<&str>,
    ) -> Result<Option<AlistVideoPreview>, ProviderError> {
        let request = alist_upstream::FsOtherReq {
            host: host.to_string(),
            token: token.to_string(),
            path: path.to_string(),
            method: "video_preview".to_string(),
            password: password.unwrap_or("").to_string(),
        };

        let other_data = self
            .fs_other(request)
            .await
            .map_err(|e| ProviderError::NetworkError(e.to_string()))?;

        Ok(AlistVideoPreview::from_fs_other_resp(other_data))
    }
}

fn alist_headers() -> HashMap<String, String> {
    HashMap::from([(
        "User-Agent".to_string(),
        synctv_media_providers::PROVIDER_USER_AGENT.to_string(),
    )])
}

fn alist_login_request(req: AlistLoginRequest) -> alist_upstream::LoginReq {
    let credential = match req.credential {
        AlistLoginCredential::Password(password) => {
            alist_upstream::login_req::Credential::Password(password)
        }
        AlistLoginCredential::HashedPassword(hashed_password) => {
            alist_upstream::login_req::Credential::HashedPassword(hashed_password)
        }
    };
    alist_upstream::LoginReq {
        host: req.host,
        username: req.username,
        credential: Some(credential),
        otp_code: req.otp_code,
    }
}

fn alist_list_request(req: AlistListRequest) -> alist_upstream::FsListReq {
    alist_upstream::FsListReq {
        host: req.host,
        token: req.token,
        path: req.path,
        password: req.password,
        page: req.page,
        per_page: req.per_page,
        refresh: req.refresh,
    }
}

fn alist_search_request(req: &AlistSearchRequest) -> alist_upstream::FsSearchReq {
    alist_upstream::FsSearchReq {
        host: req.host.clone(),
        token: req.token.clone(),
        parent: req.parent.clone(),
        keywords: req.keywords.clone(),
        scope: req.scope,
        page: req.page,
        per_page: req.per_page,
        password: req.password.clone(),
    }
}

fn alist_search_fallback_list_request(req: &AlistSearchRequest) -> alist_upstream::FsListReq {
    alist_upstream::FsListReq {
        host: req.host.clone(),
        token: req.token.clone(),
        path: req.parent.clone(),
        password: req.password.clone(),
        page: req.page.max(1),
        per_page: req.per_page.max(1),
        refresh: false,
    }
}

fn alist_me_request(req: AlistMeRequest) -> alist_upstream::MeReq {
    alist_upstream::MeReq {
        host: req.host,
        token: req.token,
    }
}

fn alist_modified_to_i64(value: u64) -> Result<i64, ProviderError> {
    i64::try_from(value).map_err(|_| {
        ProviderError::ApiError(format!(
            "Alist file modified timestamp {value} exceeds i64::MAX"
        ))
    })
}

fn usize_to_u64(value: usize, field: &str) -> Result<u64, ProviderError> {
    u64::try_from(value)
        .map_err(|_| ProviderError::InvalidConfig(format!("{field} exceeds u64::MAX")))
}

fn alist_directory_item_type(name: &str, is_dir: bool) -> Option<ItemType> {
    if is_dir {
        return Some(ItemType::Playlist);
    }

    match name
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some(
            "mp4" | "mkv" | "avi" | "mov" | "flv" | "webm" | "m4v" | "wmv" | "m3u8" | "mp3"
            | "flac" | "wav" | "aac" | "m4a" | "ogg",
        ) => Some(ItemType::Media),
        _ => None,
    }
}

fn join_alist_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{}", parent.trim_end_matches('/'), name)
    }
}

fn alist_relative_path_from_base(base_path: &str, full_path: &str) -> Option<String> {
    let normalized_base = if base_path == "/" {
        String::new()
    } else {
        base_path.trim_end_matches('/').to_string()
    };
    let relative = full_path.strip_prefix(&normalized_base)?;
    if relative.starts_with('/') {
        Some(relative.to_string())
    } else if relative.is_empty() {
        Some("/".to_string())
    } else {
        Some(format!("/{relative}"))
    }
}

fn is_subtitle_filename(name: &str) -> bool {
    matches!(
        name.rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("srt" | "vtt" | "ass" | "ssa")
    )
}

fn is_alist_search_unavailable(error: &synctv_media_providers::ProviderClientError) -> bool {
    matches!(
        error,
        synctv_media_providers::ProviderClientError::Api { code: 404, message }
            if message.eq_ignore_ascii_case("search not available")
    )
}

fn subtitle_format_from_name(name: &str) -> String {
    name.rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .filter(|ext| matches!(ext.as_str(), "srt" | "vtt" | "ass" | "ssa"))
        .unwrap_or_else(|| "srt".to_string())
}

fn external_subtitle_language(name: &str) -> String {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let token = stem
        .rsplit(['.', '_'])
        .next()
        .map(str::trim)
        .unwrap_or_default();

    if token.is_empty() || token == stem || token.len() > 16 {
        return "und".to_string();
    }

    let looks_like_language = token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
        && token.chars().any(|ch| ch.is_ascii_alphabetic())
        && token.chars().filter(char::is_ascii_alphabetic).count() <= 8;

    if looks_like_language {
        token.to_string()
    } else {
        "und".to_string()
    }
}

fn subtitle_name_from_task(task: &AlistSubtitleTask, index: usize) -> String {
    let language = task.language.trim();
    if language.is_empty() {
        format!("Transcoded Subtitle {}", index + 1)
    } else {
        language.to_string()
    }
}

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

fn external_subtitle(
    name: String,
    language: String,
    url: String,
    headers: HashMap<String, String>,
    format: String,
) -> PlaybackSubtitle {
    PlaybackSubtitle {
        name,
        language,
        format,
        provider: PlaybackSubtitleProvider::External(PlaybackExternalSubtitle { url, headers }),
    }
}

fn subtitles_from_video_preview(preview: Option<&AlistVideoPreview>) -> Vec<PlaybackSubtitle> {
    let headers = alist_headers();
    preview.map_or_else(Vec::new, |preview| {
        preview
            .subtitle_tasks
            .iter()
            .enumerate()
            .filter(|(_, sub)| !sub.url.trim().is_empty())
            .map(|(idx, sub)| {
                let name = subtitle_name_from_task(sub, idx);
                external_subtitle(
                    name,
                    if sub.language.trim().is_empty() {
                        "und".to_string()
                    } else {
                        sub.language.clone()
                    },
                    sub.url.clone(),
                    headers.clone(),
                    "srt".to_string(),
                )
            })
            .collect()
    })
}

fn subtitles_from_related_files(related: &[AlistRelatedFile]) -> Vec<PlaybackSubtitle> {
    let headers = alist_headers();
    related
        .iter()
        .filter(|related| {
            !related.is_dir
                && !related.raw_url.trim().is_empty()
                && is_subtitle_filename(&related.name)
        })
        .map(|related| {
            external_subtitle(
                related.name.clone(),
                external_subtitle_language(&related.name),
                related.raw_url.clone(),
                headers.clone(),
                subtitle_format_from_name(&related.name),
            )
        })
        .collect()
}

fn merge_subtitles(
    mut primary: Vec<PlaybackSubtitle>,
    secondary: Vec<PlaybackSubtitle>,
) -> Vec<PlaybackSubtitle> {
    for subtitle in secondary {
        if !primary
            .iter()
            .any(|existing| existing.upstream_url() == subtitle.upstream_url())
        {
            primary.push(subtitle);
        }
    }
    primary
}

fn mark_alist_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    // Alist returns upstream playback modes and SyncTV proxy siblings in the
    // same result. The proxy default keeps clients independent from upstream
    // auth headers and HLS segment rewriting details.
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

        let proxy_mode_name = format!("proxy_{mode_name}");
        if result.playback_infos.contains_key(&proxy_mode_name) {
            continue;
        }

        let mut proxy_info = original_info.clone();
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
                    PlaybackMediaProvider::Alist(
                        if super::playback_media_is_hls(&mode_name, media) {
                            PlaybackAlistMedia::ProxyTranscodedHlsManifest {
                                version: version.to_string(),
                                expires_at,
                                mode_name: mode_name.clone(),
                                url_index,
                                url,
                                headers: media.upstream_headers(),
                            }
                        } else {
                            PlaybackAlistMedia::ProxyFile {
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
            })
            .collect();
        proxy_info.subtitles = original_info
            .subtitles
            .iter()
            .enumerate()
            .map(|(subtitle_index, subtitle)| PlaybackSubtitle {
                name: subtitle.name().to_string(),
                language: subtitle.language().to_string(),
                format: subtitle.format().to_string(),
                provider: PlaybackSubtitleProvider::Alist(PlaybackAlistSubtitle {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    subtitle_index,
                    url: subtitle.upstream_url().to_string(),
                    headers: subtitle.upstream_headers(),
                }),
            })
            .collect();

        result.playback_infos.insert(proxy_mode_name, proxy_info);
    }

    result.default_mode = format!("proxy_{original_default_mode}");
}

fn related_file_path(parent_path: &str, name: &str) -> Option<String> {
    if name.is_empty()
        || name.contains('/')
        || name.contains('\\')
        || validate_path_for_traversal(name).is_err()
    {
        return None;
    }

    let parent = parent_path.trim_end_matches('/');
    Some(if parent.is_empty() {
        format!("/{name}")
    } else {
        format!("{parent}/{name}")
    })
}

/// Alist `MediaProvider`
///
/// Holds a reference to `RemoteProviderManager` to select appropriate provider instance.
pub struct AlistProvider {
    provider_instance_manager: Arc<RemoteProviderManager>,
    client_manager: Arc<ProviderClientManager>,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

#[derive(Debug, Clone)]
pub enum AlistLoginCredential {
    Password(String),
    HashedPassword(String),
}

#[derive(Debug, Clone)]
pub struct AlistLoginRequest {
    pub host: String,
    pub username: String,
    pub credential: AlistLoginCredential,
    pub otp_code: String,
}

#[derive(Debug, Clone)]
pub struct AlistPersistedLoginResponse {
    pub token: String,
    pub server_id: String,
}

#[derive(Debug, Clone)]
pub struct AlistPersistLoginCredentialRequest {
    pub user_id: UserId,
    pub host: String,
    pub username: String,
    pub password: String,
    pub password_is_hashed: bool,
    pub otp_secret: Option<String>,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlistLoginAndPersistRequest {
    pub user_id: UserId,
    pub host: String,
    pub username: String,
    pub password: Option<String>,
    pub hashed_password: Option<String>,
    pub otp_code: String,
    pub otp_secret: Option<String>,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlistBind {
    pub id: i64,
    pub server_id: String,
    pub host: String,
    pub username: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AlistListRequest {
    pub host: String,
    pub token: String,
    pub path: String,
    pub password: String,
    pub page: u64,
    pub per_page: u64,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct AlistListItem {
    pub name: String,
    pub size: u64,
    pub is_dir: bool,
    pub modified: u64,
    pub sign: String,
    pub thumb: String,
    pub item_type: u64,
}

#[derive(Debug, Clone)]
pub struct AlistListResponse {
    pub content: Vec<AlistListItem>,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct AlistSearchRequest {
    pub host: String,
    pub token: String,
    pub parent: String,
    pub keywords: String,
    pub scope: u64,
    pub page: u64,
    pub per_page: u64,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct AlistSearchItem {
    pub parent: String,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub item_type: u64,
}

#[derive(Debug, Clone)]
pub struct AlistSearchResponse {
    pub content: Vec<AlistSearchItem>,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct AlistMeRequest {
    pub host: String,
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct AlistMeResponse {
    pub username: String,
    pub base_path: String,
}

impl AlistProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "alist";

    #[must_use]
    pub fn credential_server_id_for_instance(
        host: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        match normalize_provider_instance_name(provider_instance_name) {
            Some(instance_name) => hex::encode(Sha256::digest(
                format!("{host}\n{instance_name}").as_bytes(),
            )),
            None => hex::encode(Sha256::digest(host.as_bytes())),
        }
    }

    /// Create a new `AlistProvider` with `RemoteProviderManager`
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
            ProviderError::Internal("Alist credential repository is not configured".to_string())
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
    ) -> Result<AlistClientArc, ProviderError> {
        match instance_name {
            None => Ok(self.client_manager.local_alist_client()),
            Some(_) => {
                self.provider_instance_manager
                    .resolve_client_required_with_context(
                        instance_name,
                        request_context,
                        create_remote_alist_client,
                        || self.client_manager.local_alist_client(),
                    )
                    .await
            }
        }
    }

    /// Detect file format from extension
    fn detect_format(filename: &str) -> String {
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        match ext.as_str() {
            "mp4" | "m4v" | "mov" => "mp4",
            "mkv" => "mkv",
            "avi" => "avi",
            "flv" => "flv",
            "webm" => "webm",
            "m3u8" => "hls",
            _ => "video",
        }
        .to_string()
    }

    pub async fn login_with_context(
        &self,
        req: AlistLoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<String, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client
            .login(alist_login_request(req))
            .await
            .map_err(std::convert::Into::into)
    }

    #[must_use]
    pub fn hash_password_for_storage(password: &str) -> String {
        hex::encode(Sha256::digest(
            format!("{password}-{ALIST_PASSWORD_HASH_SALT}").as_bytes(),
        ))
    }

    pub fn resolve_login_credential(
        password: Option<&str>,
        hashed_password: Option<&str>,
    ) -> Result<(String, bool), ProviderError> {
        match (password, hashed_password) {
            (Some(password), None) => {
                if password.trim().is_empty() {
                    return Err(ProviderError::InvalidConfig(
                        "Alist login password must not be empty".to_string(),
                    ));
                }

                Ok((password.to_string(), false))
            }
            (None, Some(hashed_password)) => {
                if hashed_password.trim().is_empty() {
                    return Err(ProviderError::InvalidConfig(
                        "Alist login hashed_password must not be empty".to_string(),
                    ));
                }

                Ok((hashed_password.to_string(), true))
            }
            _ => Err(ProviderError::InvalidConfig(
                "Alist login requires exactly one credential".to_string(),
            )),
        }
    }

    pub fn otp_code_from_secret(otp_secret: Option<&str>) -> Result<String, ProviderError> {
        otp_secret.map_or_else(
            || Ok(String::new()),
            |secret| Self::current_otp_code(secret).map_err(ProviderError::InvalidConfig),
        )
    }

    pub fn normalize_otp_secret(otp_secret: Option<String>) -> Option<String> {
        otp_secret.and_then(|otp_secret| {
            let trimmed = otp_secret.trim();
            if trimmed.is_empty() {
                return None;
            }

            if trimmed
                .get(..10)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("otpauth://"))
            {
                return url::Url::parse(trimmed).ok().and_then(|url| {
                    url.query_pairs()
                        .find(|(key, _)| key.eq_ignore_ascii_case("secret"))
                        .map(|(_, value)| value.trim().to_string())
                        .filter(|value| !value.is_empty())
                });
            }

            Some(trimmed.to_string())
        })
    }

    pub fn current_otp_code(otp_secret: &str) -> Result<String, String> {
        Self::otp_code_at_timestamp(otp_secret, crate::SystemClock.now().timestamp())
    }

    pub fn otp_code_at_timestamp(otp_secret: &str, timestamp: i64) -> Result<String, String> {
        let secret = Self::normalize_otp_secret(Some(otp_secret.to_string()))
            .ok_or_else(|| "Alist OTP secret must not be empty".to_string())?;
        let key = decode_base32_secret(&secret)?;
        if key.is_empty() {
            return Err("Alist OTP secret must not decode to an empty key".to_string());
        }

        let counter = u64::try_from(timestamp.max(0) / 30)
            .map_err(|_| "Invalid Alist OTP timestamp".to_string())?;
        let mut mac = Hmac::<Sha1>::new_from_slice(&key)
            .map_err(|_| "Invalid Alist OTP secret key".to_string())?;
        mac.update(&counter.to_be_bytes());
        let digest = mac.finalize().into_bytes();
        let offset = usize::from(digest[digest.len() - 1] & 0x0f);
        let binary = ((u32::from(digest[offset]) & 0x7f) << 24)
            | (u32::from(digest[offset + 1]) << 16)
            | (u32::from(digest[offset + 2]) << 8)
            | u32::from(digest[offset + 3]);

        Ok(format!("{:06}", binary % 1_000_000))
    }

    pub fn resolve_login_otp_code(
        otp_code: &str,
        otp_secret: Option<&str>,
    ) -> Result<String, ProviderError> {
        let trimmed_code = otp_code.trim();
        if !trimmed_code.is_empty() {
            return Ok(trimmed_code.to_string());
        }

        Self::otp_code_from_secret(otp_secret)
    }

    pub async fn persist_login_credential(
        &self,
        request: AlistPersistLoginCredentialRequest,
    ) -> Result<String, ProviderError> {
        let provider_instance_name = request.provider_instance_name.as_deref();
        let server_id =
            Self::credential_server_id_for_instance(&request.host, provider_instance_name);
        let stored_password = if request.password_is_hashed {
            request.password
        } else {
            Self::hash_password_for_storage(&request.password)
        };
        let credential_data = ProviderCredential::Alist {
            host: request.host,
            username: request.username,
            password: stored_password,
            otp_secret: request.otp_secret,
        };
        let now = crate::SystemClock.now();
        let credential = UserProviderCredential {
            id: 0,
            user_id: request.user_id,
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
                ProviderError::Internal(format!("Failed to persist alist credential: {error}"))
            })?;

        Ok(server_id)
    }

    pub async fn login_and_persist_with_context(
        &self,
        request: AlistLoginAndPersistRequest,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<AlistPersistedLoginResponse, ProviderError> {
        let (password, hashed) = Self::resolve_login_credential(
            request.password.as_deref(),
            request.hashed_password.as_deref(),
        )?;
        let otp_secret = Self::normalize_otp_secret(request.otp_secret);
        let otp_code = Self::resolve_login_otp_code(&request.otp_code, otp_secret.as_deref())?;
        let provider_instance_name = request.provider_instance_name.as_deref();

        let token = self
            .login_with_context(
                AlistLoginRequest {
                    host: request.host.clone(),
                    username: request.username.clone(),
                    credential: if hashed {
                        AlistLoginCredential::HashedPassword(password.clone())
                    } else {
                        AlistLoginCredential::Password(password.clone())
                    },
                    otp_code,
                },
                provider_instance_name,
                request_context,
            )
            .await?;
        let server_id = self
            .persist_login_credential(AlistPersistLoginCredentialRequest {
                user_id: request.user_id,
                host: request.host,
                username: request.username,
                password,
                password_is_hashed: hashed,
                otp_secret,
                provider_instance_name: request.provider_instance_name,
            })
            .await?;

        Ok(AlistPersistedLoginResponse { token, server_id })
    }

    pub async fn delete_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let Some(existing) = self
            .credential_repo()?
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to query alist credential: {error}"))
            })?
        else {
            return Ok(false);
        };

        self.credential_repo()?
            .delete(existing.id)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to delete alist credential: {error}"))
            })?;
        Ok(true)
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<AlistBind>, ProviderError> {
        let requested_instance_name = match normalize_provider_instance_name(provider_instance_name)
        {
            Some(instance_name) => {
                validate_provider_instance_name(instance_name)
                    .map_err(ProviderError::InvalidConfig)?;
                Some(instance_name)
            }
            None => None,
        };
        let credentials = self
            .credential_repo()?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| {
                ProviderError::Internal(format!("Failed to query alist credentials: {error}"))
            })?;

        credentials
            .into_iter()
            .filter(|credential| {
                requested_instance_name.is_none_or(|requested| {
                    normalize_provider_instance_name(credential.provider_instance_name.as_deref())
                        == Some(requested)
                })
            })
            .map(|credential| {
                let ProviderCredential::Alist { host, username, .. } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                let host = host.trim();
                let username = username.trim();
                if host.is_empty() || username.is_empty() {
                    return Err(ProviderError::InvalidConfig(format!(
                        "Alist credential {} has empty bind fields",
                        credential.id
                    )));
                }

                Ok(AlistBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    host: host.to_string(),
                    username: username.to_string(),
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub fn binding_from_stored_credential(
        user_id: UserId,
        server_id: &str,
        credential: ProviderCredential,
        credential_revision: String,
        stored_provider_instance_name: Option<String>,
        requested_provider_instance_name: Option<&str>,
    ) -> Result<AlistBinding, ProviderError> {
        let provider_instance_name = requested_provider_instance_name
            .map(std::string::ToString::to_string)
            .or(stored_provider_instance_name);
        match credential {
            ProviderCredential::Alist { host, .. } => Ok(AlistBinding {
                host,
                server_id: server_id.to_string(),
                credential_owner_id: user_id.to_string(),
                credential_revision,
                provider_instance_name,
            }),
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    pub fn access_from_session(
        user_id: UserId,
        server_id: &str,
        credential_revision: String,
        provider_instance_name: Option<String>,
        host: String,
        token: String,
    ) -> AlistAccess {
        AlistAccess {
            host,
            token,
            server_id: server_id.to_string(),
            credential_owner_id: user_id.to_string(),
            credential_revision,
            provider_instance_name,
        }
    }

    pub async fn login_access_from_stored_credential(
        &self,
        user_id: UserId,
        server_id: &str,
        credential: ProviderCredential,
        credential_revision: String,
        provider_instance_name: Option<String>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<AlistAccess, ProviderError> {
        let ProviderCredential::Alist {
            host,
            username,
            password,
            otp_secret,
        } = credential
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        let otp_code = Self::otp_code_from_secret(otp_secret.as_deref())?;
        let token = self
            .login_with_context(
                AlistLoginRequest {
                    host: host.clone(),
                    username,
                    credential: AlistLoginCredential::HashedPassword(password),
                    otp_code,
                },
                provider_instance_name.as_deref(),
                request_context,
            )
            .await?;

        Ok(Self::access_from_session(
            user_id,
            server_id,
            credential_revision,
            provider_instance_name,
            host,
            token,
        ))
    }

    pub async fn fs_list_with_context(
        &self,
        req: AlistListRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<AlistListResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .fs_list(alist_list_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(AlistListResponse {
            content: resp
                .content
                .into_iter()
                .map(|item| AlistListItem {
                    name: item.name,
                    size: item.size,
                    is_dir: item.is_dir,
                    modified: item.modified,
                    sign: item.sign,
                    thumb: item.thumb,
                    item_type: item.r#type,
                })
                .collect(),
            total: resp.total,
        })
    }

    /// Search Alist files and directories.
    pub async fn fs_search_with_context(
        &self,
        req: AlistSearchRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<AlistSearchResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let provider_req = alist_search_request(&req);
        let resp = match client.fs_search(provider_req.clone()).await {
            Ok(resp) => Ok(Self::alist_search_response_from_provider(resp)),
            Err(error) if is_alist_search_unavailable(&error) => {
                Self::fs_search_fallback_to_listing(client, req).await
            }
            Err(error) => Err(error.into()),
        };
        resp
    }

    async fn fs_search_fallback_to_listing(
        client: AlistClientArc,
        req: AlistSearchRequest,
    ) -> Result<AlistSearchResponse, ProviderError> {
        let list_resp = client
            .fs_list(alist_search_fallback_list_request(&req))
            .await
            .map_err(ProviderError::from)?;

        let content = list_resp
            .content
            .into_iter()
            .filter(|item| match req.scope {
                1 => item.is_dir,
                2 => !item.is_dir,
                _ => true,
            })
            .map(|item| AlistSearchItem {
                parent: req.parent.clone(),
                name: item.name,
                is_dir: item.is_dir,
                size: item.size,
                item_type: item.r#type,
            })
            .collect::<Vec<_>>();

        Ok(AlistSearchResponse {
            total: usize_to_u64(content.len(), "Alist search fallback total")?,
            content,
        })
    }

    fn alist_search_response_from_provider(
        resp: alist_upstream::FsSearchResp,
    ) -> AlistSearchResponse {
        AlistSearchResponse {
            content: resp
                .content
                .into_iter()
                .map(|item| AlistSearchItem {
                    parent: item.parent,
                    name: item.name,
                    is_dir: item.is_dir,
                    size: item.size,
                    item_type: item.r#type,
                })
                .collect(),
            total: resp.total,
        }
    }

    pub async fn me_with_context(
        &self,
        req: AlistMeRequest,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<AlistMeResponse, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        let resp = client
            .me(alist_me_request(req))
            .await
            .map_err(ProviderError::from)?;
        Ok(AlistMeResponse {
            username: resp.username,
            base_path: resp.base_path,
        })
    }

    fn encode_target(relative_path: &str) -> Result<crate::models::ProviderTarget, ProviderError> {
        if relative_path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist target relative_path cannot be empty".to_string(),
            ));
        }
        Ok(crate::models::ProviderTarget::alist(
            relative_path.to_string(),
        ))
    }

    fn decode_target(
        target: Option<&crate::models::ProviderTarget>,
    ) -> Result<Option<String>, ProviderError> {
        let Some(target) = target else {
            return Ok(None);
        };
        let crate::models::ProviderTarget::Alist(payload) = target else {
            return Err(ProviderError::InvalidConfig(
                "Alist target must use alist payload".to_string(),
            ));
        };
        if payload.relative_path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist target relative_path cannot be empty".to_string(),
            ));
        }

        Ok(Some(payload.relative_path.clone()))
    }
}

fn decode_base32_secret(secret: &str) -> Result<Vec<u8>, String> {
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    let mut decoded = Vec::new();

    for ch in secret.chars() {
        if ch == '=' || ch.is_whitespace() {
            continue;
        }

        let value = match ch.to_ascii_uppercase() {
            'A'..='Z' => u32::from(ch.to_ascii_uppercase()) - u32::from('A'),
            '2'..='7' => u32::from(ch) - u32::from('2') + 26,
            _ => return Err("Alist OTP secret must be RFC 4648 base32".to_string()),
        };

        buffer = (buffer << 5) | value;
        bits += 5;

        if bits >= 8 {
            bits -= 8;
            let byte = u8::try_from((buffer >> bits) & 0xff)
                .map_err(|_| "Invalid Alist OTP base32 byte".to_string())?;
            decoded.push(byte);
            buffer &= (1_u32 << bits) - 1;
        }
    }

    Ok(decoded)
}

/// Resolved Alist configuration with credentials ready for API calls.
struct ResolvedAlistBinding {
    path: String,
    password: Option<String>,
    credential_owner_id: String,
    credential_revision: String,
}

struct ResolvedAlistConfig {
    host: String,
    token: String,
    server_id: String,
    path: String,
    password: Option<String>,
    credential_owner_id: String,
    credential_revision: String,
    provider_instance_name: Option<String>,
}

fn alist_fs_get_request(config: &ResolvedAlistConfig, path: String) -> alist_upstream::FsGetReq {
    alist_upstream::FsGetReq {
        host: config.host.clone(),
        token: config.token.clone(),
        path,
        password: config.password.clone().unwrap_or_default(),
        headers: alist_headers(),
    }
}

fn alist_resolved_list_request(
    config: &ResolvedAlistConfig,
    path: String,
    page: u64,
    per_page: u64,
    refresh: bool,
) -> alist_upstream::FsListReq {
    alist_upstream::FsListReq {
        host: config.host.clone(),
        token: config.token.clone(),
        path,
        password: config.password.clone().unwrap_or_default(),
        page,
        per_page,
        refresh,
    }
}

fn alist_resolved_search_request(
    config: &ResolvedAlistConfig,
    parent: String,
    keywords: String,
    page: u64,
    per_page: u64,
) -> alist_upstream::FsSearchReq {
    alist_upstream::FsSearchReq {
        host: config.host.clone(),
        token: config.token.clone(),
        parent,
        keywords,
        scope: 0,
        page,
        per_page,
        password: config.password.clone().unwrap_or_default(),
    }
}

#[derive(Debug, Clone)]
struct AlistSourceConfig {
    path: String,
    password: Option<String>,
    server_id: String,
}

impl From<AlistMediaSourceConfig> for AlistSourceConfig {
    fn from(config: AlistMediaSourceConfig) -> Self {
        Self {
            path: config.path,
            password: config.password,
            server_id: config.server_id,
        }
    }
}

impl From<AlistPlaylistSourceConfig> for AlistSourceConfig {
    fn from(config: AlistPlaylistSourceConfig) -> Self {
        Self {
            path: config.path,
            password: config.password,
            server_id: config.server_id,
        }
    }
}

impl AlistSourceConfig {
    fn media_from_config(value: &crate::models::MediaSourceConfig) -> Result<Self, ProviderError> {
        match value {
            crate::models::MediaSourceConfig::Alist(config) => Ok(config.clone().into()),
            _ => Err(ProviderError::InvalidConfig(
                "Alist media requires Alist source_config".to_string(),
            )),
        }
    }

    fn playlist_from_config(
        value: &crate::models::PlaylistSourceConfig,
    ) -> Result<Self, ProviderError> {
        match value {
            crate::models::PlaylistSourceConfig::Alist(config) => Ok(config.clone().into()),
            _ => Err(ProviderError::InvalidConfig(
                "Alist playlist requires Alist source_config".to_string(),
            )),
        }
    }

    fn from_source_config(value: SourceConfig<'_>) -> Result<Self, ProviderError> {
        match value {
            SourceConfig::Media(config) => Self::media_from_config(config),
            SourceConfig::DynamicPlaylist(config) => Self::playlist_from_config(config),
        }
    }
}

impl AlistProvider {
    fn playback_cache_key(
        server_id: &str,
        credential_owner_id: &str,
        credential_revision: &str,
        path: &str,
        password: Option<&str>,
        playback_profile_cache_key: &str,
    ) -> String {
        let mut owner_hasher = Sha256::new();
        owner_hasher.update(credential_owner_id.as_bytes());
        owner_hasher.update(b"\0");
        owner_hasher.update(credential_revision.as_bytes());
        let owner_hash: String = hex::encode(owner_hasher.finalize())
            .chars()
            .take(16)
            .collect();

        let mut hasher = Sha256::new();
        hasher.update(path.as_bytes());
        hasher.update(b"\0");
        hasher.update(password.unwrap_or("").as_bytes());
        hasher.update(b"\0");
        hasher.update(playback_profile_cache_key.as_bytes());
        let path_hash: String = hex::encode(hasher.finalize()).chars().take(16).collect();
        format!("playback:{server_id}:{owner_hash}:{path_hash}")
    }

    fn source_cover_cache_key(config: &ResolvedAlistConfig) -> String {
        let mut owner_hasher = Sha256::new();
        owner_hasher.update(config.credential_owner_id.as_bytes());
        owner_hasher.update(b"\0");
        owner_hasher.update(config.credential_revision.as_bytes());
        owner_hasher.update(b"\0");
        owner_hasher.update(
            config
                .provider_instance_name
                .as_deref()
                .unwrap_or_default()
                .as_bytes(),
        );
        let owner_hash: String = hex::encode(owner_hasher.finalize())
            .chars()
            .take(16)
            .collect();

        let mut path_hasher = Sha256::new();
        path_hasher.update(config.path.as_bytes());
        path_hasher.update(b"\0");
        path_hasher.update(config.password.as_deref().unwrap_or_default().as_bytes());
        let path_hash: String = hex::encode(path_hasher.finalize())
            .chars()
            .take(16)
            .collect();
        format!("source-cover:{}:{owner_hash}:{path_hash}", config.server_id)
    }

    /// Resolve AlistSourceConfig into a cached credential binding without logging in.
    async fn resolve_binding(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &crate::models::MediaSourceConfig,
    ) -> Result<ResolvedAlistBinding, ProviderError> {
        let config = AlistSourceConfig::media_from_config(source_config)?;
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;

        let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
            ProviderError::Internal(
                "provider_access_service not available in ProviderContext".to_string(),
            )
        })?;
        let binding = access_service
            .alist_binding(
                *credential_owner_id,
                &config.server_id,
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        Ok(ResolvedAlistBinding {
            path: config.path,
            password: config.password,
            credential_owner_id: binding.credential_owner_id,
            credential_revision: binding.credential_revision,
        })
    }

    /// Resolve AlistSourceConfig into credentials owned by the media/playlist creator.
    async fn resolve_config(
        &self,
        ctx: &ProviderContext<'_>,
        config: AlistSourceConfig,
    ) -> Result<ResolvedAlistConfig, ProviderError> {
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;

        let access_service = ctx.provider_access_service.as_ref().ok_or_else(|| {
            ProviderError::Internal(
                "provider_access_service not available in ProviderContext".to_string(),
            )
        })?;
        let access = access_service
            .alist_access(
                *credential_owner_id,
                &config.server_id,
                super::bound_provider_instance_name(ctx),
                ctx.request_context(),
            )
            .await?;
        Ok(ResolvedAlistConfig {
            host: access.host,
            token: access.token,
            server_id: access.server_id,
            path: config.path,
            password: config.password,
            credential_owner_id: access.credential_owner_id,
            credential_revision: access.credential_revision,
            provider_instance_name: access.provider_instance_name,
        })
    }

    async fn enrich_related_subtitles(
        client: &AlistClientArc,
        config: &ResolvedAlistConfig,
        file_info: &mut AlistFileInfo,
    ) {
        let parent_path =
            config.path.rsplit_once('/').map_or(
                "/",
                |(parent, _)| if parent.is_empty() { "/" } else { parent },
            );

        for related in file_info
            .related
            .iter_mut()
            .filter(|related| {
                !related.is_dir
                    && related.raw_url.trim().is_empty()
                    && is_subtitle_filename(&related.name)
            })
            .take(RELATED_SUBTITLE_FETCH_LIMIT)
        {
            let Some(path) = related_file_path(parent_path, &related.name) else {
                continue;
            };

            let request = alist_fs_get_request(config, path);

            if let Ok(subtitle_info) = client.fs_get(request).await {
                related.raw_url = subtitle_info.raw_url;
                related.provider = subtitle_info.provider;
            }
        }
    }

    /// Resolve playback from the Alist API (no caching layer).
    ///
    /// Contains the core API interaction logic, called by `generate_playback`
    /// after cache miss.
    async fn resolve_from_api(
        &self,
        config: &ResolvedAlistConfig,
        request_context: Option<&super::ExecutionControl>,
        playback_client_profile: Option<&PlaybackClientProfile>,
    ) -> Result<PlaybackResult, ProviderError> {
        // Get appropriate client based on instance_name from config
        let client = self
            .get_client_with_context(config.provider_instance_name.as_deref(), request_context)
            .await?;

        // Build upstream request
        let request = alist_fs_get_request(config, config.path.clone());

        // Call client (trait method - implementation handles local/remote)
        let fs_get_data = client.fs_get(request).await?;

        let mut file_info: AlistFileInfo = fs_get_data.into();

        if file_info.is_dir {
            return Err(ProviderError::UnsupportedFormat(
                "Cannot play directory, use browse() instead".to_string(),
            ));
        }

        Self::enrich_related_subtitles(&client, config, &mut file_info).await;

        // Try to get video preview info for transcoded URLs (optional).
        let (video_preview, video_preview_error) = match client
            .get_video_preview(
                &config.host,
                &config.token,
                &config.path,
                config.password.as_deref(),
            )
            .await
        {
            Ok(preview) => (preview, None),
            Err(err) => (None, Some(err.to_string())),
        };

        Ok(Self::build_playback_result(
            &file_info,
            video_preview.as_ref(),
            video_preview_error.as_deref(),
            playback_client_profile,
            config.provider_instance_name.clone(),
        ))
    }

    fn build_playback_result(
        file_info: &AlistFileInfo,
        video_preview: Option<&AlistVideoPreview>,
        video_preview_error: Option<&str>,
        playback_client_profile: Option<&PlaybackClientProfile>,
        provider_instance_name: Option<String>,
    ) -> PlaybackResult {
        let mut playback_infos = HashMap::new();
        let mut metadata = AlistPlaybackMetadata {
            name: Some(file_info.name.clone()),
            size: Some(file_info.size),
            provider: Some(file_info.provider.clone()),
            ..Default::default()
        };
        let thumbnail = (!file_info.thumb.is_empty()).then(|| file_info.thumb.clone());
        let mut duration_seconds = None;
        let related_subtitles = subtitles_from_related_files(&file_info.related);
        if !related_subtitles.is_empty() {
            metadata.external_subtitle_count = Some(related_subtitles.len());
        }

        if let Some(error) = video_preview_error {
            metadata.video_preview_error = Some(error.to_string());
        }

        let combined_subtitles = if matches!(
            playback_client_profile.map(|profile| profile.subtitle_preference),
            Some(PlaybackSubtitlePreference::None)
        ) {
            Vec::new()
        } else {
            let preview_subtitles = subtitles_from_video_preview(video_preview);
            merge_subtitles(preview_subtitles, related_subtitles)
        };

        let mut transcoded_modes = Vec::new();
        let headers = alist_headers();

        if let Some(preview) = video_preview {
            // Add transcoding quality options
            for (idx, task) in preview.transcoding_tasks.iter().enumerate() {
                if !task.url.is_empty() {
                    let quality_name = if task.template_name.is_empty() {
                        format!("quality_{idx}")
                    } else {
                        task.template_name.clone()
                    };
                    let mode_name = format!("transcoded_{quality_name}");
                    transcoded_modes.push((mode_name.clone(), task.template_height));

                    // AliyunDrive live transcoding URLs are requested from AList/OpenList
                    // with url_expire_sec=14400.
                    let task_expires_at = Some(crate::SystemClock.now().timestamp() + 4 * 60 * 60);
                    let info = playback_infos
                        .entry("transcoded".to_string())
                        .or_insert_with(|| PlaybackInfo {
                            thumbnail: thumbnail.clone(),
                            medias: Vec::new(),
                            default_media_index: None,
                            subtitles: combined_subtitles.clone(),
                            default_subtitle_index: None,
                            danmakus: Vec::new(),
                            default_danmaku_index: None,
                        });
                    info.medias.push(playback_media(
                        quality_name.clone(),
                        "hls".to_string(),
                        task_expires_at,
                        PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct {
                            url: task.url.clone(),
                            headers: headers.clone(),
                        }),
                    ));
                    metadata
                        .transcoding_tasks
                        .push(AlistTranscodingTaskMetadata {
                            mode_name: "transcoded".to_string(),
                            template_id: task.template_id.clone(),
                            template_name: task.template_name.clone(),
                            template_width: task.template_width,
                            template_height: task.template_height,
                            stage: task.stage.clone(),
                            status: task.status.clone(),
                        });
                }
            }

            metadata.video_preview = Some(AlistVideoPreviewMetadata {
                drive_id: (!preview.drive_id.is_empty()).then(|| preview.drive_id.clone()),
                file_id: (!preview.file_id.is_empty()).then(|| preview.file_id.clone()),
                provider: (!preview.provider.is_empty()).then(|| preview.provider.clone()),
                category: (!preview.category.is_empty()).then(|| preview.category.clone()),
                transcoding_count: preview.transcoding_tasks.len(),
                subtitle_count: preview.subtitle_tasks.len(),
            });
            metadata.width = Some(preview.width);
            metadata.height = Some(preview.height);
            if preview.duration.is_finite() && preview.duration > 0.0 {
                duration_seconds = Some(preview.duration);
            }
        }

        // Always add direct URL (raw_url) as fallback
        if !file_info.raw_url.is_empty() {
            // Alist raw URLs are provider-dependent. Use the same conservative
            // expiry window as AliyunDrive live transcoding when AList does not
            // return a per-URL expiry.
            let direct_expires_at = Some(crate::SystemClock.now().timestamp() + 4 * 60 * 60);

            playback_infos.insert(
                "direct".to_string(),
                PlaybackInfo {
                    thumbnail: thumbnail.clone(),
                    medias: vec![playback_media(
                        file_info.name.clone(),
                        Self::detect_format(&file_info.name),
                        direct_expires_at,
                        PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct {
                            url: file_info.raw_url.clone(),
                            headers: headers.clone(),
                        }),
                    )],
                    default_media_index: None,
                    subtitles: combined_subtitles,
                    default_subtitle_index: None,
                    danmakus: Vec::new(),
                    default_danmaku_index: None,
                },
            );
        }

        // Determine default mode
        let has_direct = playback_infos.contains_key("direct");
        let selected_mode =
            Self::choose_default_mode(&transcoded_modes, has_direct, playback_client_profile)
                .unwrap_or_else(|| "direct".to_string());
        let default_mode = if selected_mode.starts_with("transcoded_") {
            if let Some(info) = playback_infos.get_mut("transcoded") {
                info.default_media_index = transcoded_modes
                    .iter()
                    .position(|(mode, _)| mode == &selected_mode);
            }
            "transcoded".to_string()
        } else {
            selected_mode
        };

        PlaybackResult {
            playback_infos,
            default_mode,
            provider: Self::NAME.to_string(),
            provider_instance_name,
            duration_seconds,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: Some(PlaybackMetadata::Alist(metadata)),
        }
    }

    fn choose_default_mode(
        transcoded_modes: &[(String, u64)],
        has_direct: bool,
        playback_client_profile: Option<&PlaybackClientProfile>,
    ) -> Option<String> {
        let profile = playback_client_profile.cloned().unwrap_or_default();
        if profile.stream_preference == PlaybackStreamPreference::DirectPlay && has_direct {
            return Some("direct".to_string());
        }

        if transcoded_modes.is_empty() {
            return has_direct.then(|| "direct".to_string());
        }

        let max_height = profile
            .max_streaming_bitrate
            .and_then(Self::max_height_from_streaming_bitrate);

        let selected_transcode = max_height.map_or_else(
            || {
                transcoded_modes
                    .iter()
                    .max_by_key(|(_, height)| *height)
                    .map(|(mode, _)| mode.clone())
            },
            |height_limit| {
                transcoded_modes
                    .iter()
                    .filter(|(_, height)| *height == 0 || *height <= height_limit)
                    .max_by_key(|(_, height)| *height)
                    .or_else(|| transcoded_modes.iter().min_by_key(|(_, height)| *height))
                    .map(|(mode, _)| mode.clone())
            },
        );

        match profile.stream_preference {
            PlaybackStreamPreference::DirectPlay if has_direct => Some("direct".to_string()),
            PlaybackStreamPreference::DirectPlay => selected_transcode,
            PlaybackStreamPreference::Transcode | PlaybackStreamPreference::Auto => {
                selected_transcode.or_else(|| has_direct.then(|| "direct".to_string()))
            }
        }
    }

    fn max_height_from_streaming_bitrate(bitrate: i64) -> Option<u64> {
        if bitrate <= 0 {
            return None;
        }

        Some(match bitrate {
            0..=1_500_000 => 480,
            1_500_001..=3_500_000 => 720,
            3_500_001..=8_000_000 => 1080,
            8_000_001..=16_000_000 => 1440,
            _ => u64::MAX,
        })
    }
}

#[async_trait]
impl MediaProvider for AlistProvider {
    #[cfg(test)]
    fn test_client_manager_marker(&self) -> Option<usize> {
        Some(self.client_manager.marker())
    }

    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn validate_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let config = AlistSourceConfig::from_source_config(source_config)?;

        // Validate path is not empty and doesn't contain path traversal
        if config.path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not be empty".to_string(),
            ));
        }
        validate_path_for_traversal(&config.path).map_err(|e| {
            ProviderError::InvalidConfig(format!("Alist path must not contain path traversal: {e}"))
        })?;

        if config.server_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist server_id must not be empty".to_string(),
            ));
        }

        // Validate creator-owned credential exists
        if let Some(repo) = _ctx.credential_repo {
            let credential_owner_id = _ctx.credential_owner_id().ok_or_else(|| {
                ProviderError::Internal(
                    "credential_owner_id not available in ProviderContext".to_string(),
                )
            })?;
            let cred = repo
                .get_by_provider_and_server(*credential_owner_id, Self::NAME, &config.server_id)
                .await
                .map_err(|e| {
                    ProviderError::Internal(format!("Failed to verify credential reference: {e}"))
                })?;

            if cred.is_none() {
                return Err(ProviderError::CredentialNotFound(format!(
                    "Referenced alist credential not found for server_id '{}'",
                    config.server_id
                )));
            }
        }

        Ok(())
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let config = AlistSourceConfig::from_source_config(source_config)?;
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;

        Ok(vec![ProviderCredentialDependency::new(
            Self::NAME,
            credential_owner_id.to_string(),
            config.server_id,
        )])
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let config = AlistSourceConfig::from_source_config(source_config)?;
        let resolved = self.resolve_config(ctx, config).await?;
        let cache_key = Self::source_cover_cache_key(&resolved);
        super::cached_source_cover_or_fill(
            Self::NAME,
            &cache_key,
            Duration::from_mins(5),
            ctx,
            || async {
                let client = self
                    .get_client_with_context(
                        resolved.provider_instance_name.as_deref(),
                        ctx.request_context(),
                    )
                    .await?;
                let file_info: AlistFileInfo = client
                    .fs_get(alist_fs_get_request(&resolved, resolved.path.clone()))
                    .await?
                    .into();
                Ok(
                    (!file_info.thumb.trim().is_empty()).then_some(SourceCover::Url {
                        url: file_info.thumb,
                    }),
                )
            },
        )
        .await
    }

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<PreparedSourceConfig, ProviderError> {
        let _config = AlistSourceConfig::from_source_config(source_config)?;
        Ok(source_config.into())
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &crate::models::MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        // Resolve the credential binding first so playback cache hits do not
        // force an AList login/token refresh.
        let binding = self.resolve_binding(_ctx, source_config).await?;

        // Re-validate path at request time (defense-in-depth against traversal)
        validate_path_for_traversal(&binding.path).map_err(|e| {
            ProviderError::InvalidConfig(format!("Alist path must not contain path traversal: {e}"))
        })?;

        // Build cache key from server_id and path
        let config = AlistSourceConfig::media_from_config(source_config)?;
        let playback_client_profile = _ctx.playback_client_profile();
        let playback_profile_cache_key = playback_client_profile.map_or_else(
            || "default".to_string(),
            PlaybackClientProfile::cache_fingerprint,
        );
        let cache_key = Self::playback_cache_key(
            &config.server_id,
            &binding.credential_owner_id,
            &binding.credential_revision,
            &binding.path,
            binding.password.as_deref(),
            &playback_profile_cache_key,
        );
        let cache_ttl = Duration::from_mins(15);

        Box::pin(super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            mark_alist_playback_resources,
            || async {
                let resolved = self.resolve_config(_ctx, config.clone()).await?;
                self.resolve_from_api(&resolved, _ctx.request_context(), playback_client_profile)
                    .await
            },
        ))
        .await
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }
}

impl AlistProvider {
    pub async fn get_file_stream(
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
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: url.to_string(),
                headers: media.upstream_headers(),
                range_header: range_header.map(ToString::to_string),
            },
        )
    }

    pub async fn get_transcoded_hls_manifest(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        mode_name: &str,
        url_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(url_index))
            .ok_or(ProviderError::NotFound)?;
        let url = media.upstream_url().ok_or(ProviderError::NotFound)?;
        Ok(
            super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                url: url.to_string(),
                headers: media.upstream_headers(),
            },
        )
    }

    pub async fn get_transcoded_hls_resource(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        request: AlistHlsResourceRequest<'_>,
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
        if request.is_manifest {
            Ok(
                super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                    url: request.target_url.to_string(),
                    headers: media.upstream_headers(),
                },
            )
        } else {
            Ok(
                super::playback_transport::PlaybackTransportAction::FetchAndForward {
                    url: request.target_url.to_string(),
                    headers: media.upstream_headers(),
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
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url: subtitle.upstream_url().to_string(),
                headers: super::subtitle_headers_for_proxy(
                    &playback_info
                        .medias
                        .first()
                        .map_or_else(HashMap::new, PlaybackMedia::upstream_headers),
                    subtitle,
                ),
                range_header: None,
            },
        )
    }

    pub async fn get_thumbnail(
        &self,
        store: Option<&std::sync::Arc<dyn super::store::ProviderStore>>,
        version: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::playback_transport::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let url = versioned
            .result
            .playback_infos
            .values()
            .find_map(|info| {
                info.thumbnail
                    .as_deref()
                    .filter(|url| !url.trim().is_empty())
            })
            .ok_or(ProviderError::NotFound)?
            .to_string();
        let headers = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .and_then(|info| info.medias.first())
            .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
        Ok(
            super::playback_transport::PlaybackTransportAction::FetchAndForward {
                url,
                headers,
                range_header: None,
            },
        )
    }
}

/// Implement `DynamicPlaylistProvider` trait for Alist
///
/// Allows browsing Alist directories and getting next item for auto-play
#[async_trait]
impl DynamicPlaylistProvider for AlistProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&crate::models::ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        // Parse playlist's source_config to get base path
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = AlistSourceConfig::playlist_from_config(config)?;

        let resolved = self.resolve_config(ctx, base_config.clone()).await?;

        let relative_path = Self::decode_target(target)?;

        // Validate relative_path BEFORE any path concatenation to prevent traversal attacks
        if let Some(rel) = relative_path.as_deref() {
            validate_path_for_traversal(rel)
                .map_err(|e| ProviderError::InvalidConfig(format!("Invalid relative path: {e}")))?;
        }

        // Construct full path: base_path + relative_path
        let full_path = if let Some(rel) = relative_path.as_deref() {
            if rel.starts_with('/') {
                format!("{}{}", resolved.path.trim_end_matches('/'), rel)
            } else {
                format!("{}/{}", resolved.path.trim_end_matches('/'), rel)
            }
        } else {
            resolved.path.clone()
        };

        // Get appropriate client
        let client = self
            .get_client_with_context(
                resolved.provider_instance_name.as_deref(),
                ctx.request_context(),
            )
            .await?;

        let page = usize_to_u64(query.page().max(1), "Alist page")?;
        let per_page = usize_to_u64(query.page_size.max(1), "Alist page size")?;
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let items: Vec<DirectoryItem> = if let Some(keywords) = search {
            let search_resp = client
                .fs_search(alist_resolved_search_request(
                    &resolved,
                    full_path.clone(),
                    keywords.to_string(),
                    page,
                    per_page,
                ))
                .await?;

            search_resp
                .content
                .into_iter()
                .filter_map(|file_item| {
                    let item_type = alist_directory_item_type(&file_item.name, file_item.is_dir)?;
                    Some((file_item, item_type))
                })
                .map(|(file_item, item_type)| {
                    let full_item_path = join_alist_path(&file_item.parent, &file_item.name);
                    let item_relative_path =
                        alist_relative_path_from_base(&resolved.path, &full_item_path).ok_or_else(
                            || {
                                ProviderError::ApiError(format!(
                            "Alist search result path '{full_item_path}' is outside base path '{}'",
                            resolved.path
                        ))
                            },
                        )?;

                    Ok(DirectoryItem {
                        name: file_item.name,
                        item_type,
                        target: Self::encode_target(&item_relative_path)?,
                        size: Some(file_item.size),
                        thumbnail: None,
                        description: None,
                        modified_at: None,
                        source_config: None,
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?
        } else {
            let list_resp = client
                .fs_list(alist_resolved_list_request(
                    &resolved,
                    full_path.clone(),
                    page,
                    per_page,
                    query.refresh,
                ))
                .await?;

            list_resp
                .content
                .into_iter()
                .filter_map(|file_item| {
                    let item_type = alist_directory_item_type(&file_item.name, file_item.is_dir)?;
                    Some((file_item, item_type))
                })
                .map(|(file_item, item_type)| {
                    let item_relative_path = if let Some(rel) = relative_path.as_deref() {
                        format!("{}/{}", rel.trim_end_matches('/'), file_item.name)
                    } else {
                        format!("/{}", file_item.name)
                    };

                    Ok(DirectoryItem {
                        name: file_item.name,
                        item_type,
                        target: Self::encode_target(&item_relative_path)?,
                        size: Some(file_item.size),
                        thumbnail: if file_item.thumb.is_empty() {
                            None
                        } else {
                            Some(crate::provider::DirectoryItemThumbnail::Url(
                                file_item.thumb,
                            ))
                        },
                        description: None,
                        modified_at: Some(alist_modified_to_i64(file_item.modified)?),
                        source_config: None,
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?
        };

        Ok(DynamicListResult {
            has_more: items.len() >= query.page_size.max(1),
            items,
            pagination: DynamicPagination::Page {
                page: query.page().max(1),
            },
        })
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &crate::models::ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let relative_path = Self::decode_target(Some(target))?
            .ok_or_else(|| ProviderError::InvalidConfig("Alist target is required".to_string()))?;
        validate_path_for_traversal(&relative_path)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid relative path: {e}")))?;

        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = AlistSourceConfig::playlist_from_config(config)?;

        let build_next_source_config = |full_path: &str| -> MediaSourceConfig {
            MediaSourceConfig::Alist(AlistMediaSourceConfig {
                path: full_path.to_string(),
                password: base_config.password.clone(),
                server_id: base_config.server_id.clone(),
            })
        };

        let build_full_path = |item_path: &str| -> String {
            format!("{}{}", base_config.path.trim_end_matches('/'), item_path)
        };

        let parent_path = relative_path
            .rsplit_once('/')
            .map(|x| x.0)
            .filter(|&s| !s.is_empty());
        let parent_target = parent_path.map(Self::encode_target).transpose()?;

        let mut page = 1;
        loop {
            let page_items = self
                .list_playlist(
                    ctx,
                    playlist,
                    parent_target.as_ref(),
                    DynamicListQuery {
                        pagination: DynamicPagination::Page { page },
                        page_size: LIST_PAGE_SIZE,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            if page_items.is_empty() {
                return Ok(None);
            }

            if let Some(item) = page_items
                .iter()
                .find(|item| item.item_type == ItemType::Media && &item.target == target)
            {
                return Ok(Some(NextPlayItem {
                    name: item.name.clone(),
                    item_type: item.item_type,
                    source_config: build_next_source_config(&build_full_path(&relative_path)),
                    target: item.target.clone(),
                }));
            }

            if page_items.len() < LIST_PAGE_SIZE {
                return Ok(None);
            }
            page += 1;
        }
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &crate::models::ProviderTarget,
        play_mode: crate::models::PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        use crate::models::PlayMode;
        let relative_path = Self::decode_target(Some(target))?
            .ok_or_else(|| ProviderError::InvalidConfig("Alist target is required".to_string()))?;

        // Validate relative_path BEFORE any path operations to prevent traversal attacks
        validate_path_for_traversal(&relative_path)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid relative path: {e}")))?;

        // Parse base config and build helper for NextPlayItem source_configs
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = AlistSourceConfig::playlist_from_config(config)?;

        let build_next_source_config = |full_path: &str| -> MediaSourceConfig {
            MediaSourceConfig::Alist(AlistMediaSourceConfig {
                path: full_path.to_string(),
                password: base_config.password.clone(),
                server_id: base_config.server_id.clone(),
            })
        };

        let build_full_path = |item_path: &str| -> String {
            format!("{}{}", base_config.path.trim_end_matches('/'), item_path)
        };

        match play_mode {
            PlayMode::RepeatOne => Ok(None),
            PlayMode::Sequential | PlayMode::RepeatAll => {
                let parent_path = relative_path
                    .rsplit_once('/')
                    .map(|x| x.0)
                    .filter(|&s| !s.is_empty());
                let parent_target = parent_path.map(Self::encode_target).transpose()?;

                let mut found_current = false;
                let mut current_page = 1;

                loop {
                    let page_items = self
                        .list_playlist(
                            ctx,
                            playlist,
                            parent_target.as_ref(),
                            DynamicListQuery {
                                pagination: DynamicPagination::Page { page: current_page },
                                page_size: LIST_PAGE_SIZE,
                                ..DynamicListQuery::default()
                            },
                        )
                        .await?;

                    if page_items.is_empty() {
                        break;
                    }

                    if found_current {
                        if let Some(next) = page_items
                            .iter()
                            .find(|item| item.item_type == ItemType::Media)
                        {
                            return Ok(Some(NextPlayItem {
                                name: next.name.clone(),
                                item_type: next.item_type,
                                source_config: build_next_source_config(&build_full_path(
                                    &Self::decode_target(Some(&next.target))?.ok_or_else(|| {
                                        ProviderError::InvalidConfig(
                                            "Missing Alist item target".to_string(),
                                        )
                                    })?,
                                )),
                                target: next.target.clone(),
                            }));
                        }
                    } else if let Some(idx) =
                        page_items.iter().position(|item| &item.target == target)
                    {
                        found_current = true;
                        if let Some(next) = page_items
                            .iter()
                            .skip(idx + 1)
                            .find(|item| item.item_type == ItemType::Media)
                        {
                            return Ok(Some(NextPlayItem {
                                name: next.name.clone(),
                                item_type: next.item_type,
                                source_config: build_next_source_config(&build_full_path(
                                    &Self::decode_target(Some(&next.target))?.ok_or_else(|| {
                                        ProviderError::InvalidConfig(
                                            "Missing Alist item target".to_string(),
                                        )
                                    })?,
                                )),
                                target: next.target.clone(),
                            }));
                        }
                    }

                    if page_items.len() < LIST_PAGE_SIZE {
                        break;
                    }
                    current_page += 1;
                }

                if found_current && play_mode == PlayMode::RepeatAll {
                    let parent_path = relative_path
                        .rsplit_once('/')
                        .map(|x| x.0)
                        .filter(|&s| !s.is_empty());
                    let parent_target = parent_path.map(Self::encode_target).transpose()?;
                    let first_page = self
                        .list_playlist(
                            ctx,
                            playlist,
                            parent_target.as_ref(),
                            DynamicListQuery {
                                pagination: DynamicPagination::Page { page: 1 },
                                page_size: LIST_PAGE_SIZE,
                                ..DynamicListQuery::default()
                            },
                        )
                        .await?;
                    if let Some(first) = first_page
                        .iter()
                        .find(|item| item.item_type == ItemType::Media)
                    {
                        return Ok(Some(NextPlayItem {
                            name: first.name.clone(),
                            item_type: first.item_type,
                            source_config: build_next_source_config(&build_full_path(
                                &Self::decode_target(Some(&first.target))?.ok_or_else(|| {
                                    ProviderError::InvalidConfig(
                                        "Missing Alist item target".to_string(),
                                    )
                                })?,
                            )),
                            target: first.target.clone(),
                        }));
                    }
                }

                Ok(None)
            }
            PlayMode::Shuffle => {
                let parent_path = relative_path
                    .rsplit_once('/')
                    .map(|x| x.0)
                    .filter(|&s| !s.is_empty());
                let parent_target = parent_path.map(Self::encode_target).transpose()?;

                let mut all_items = Vec::with_capacity(SHUFFLE_MAX_ITEMS);
                let mut page = 1;
                loop {
                    let page_items = self
                        .list_playlist(
                            ctx,
                            playlist,
                            parent_target.as_ref(),
                            DynamicListQuery {
                                pagination: DynamicPagination::Page { page },
                                page_size: LIST_PAGE_SIZE,
                                ..DynamicListQuery::default()
                            },
                        )
                        .await?;
                    let is_last_page = page_items.len() < LIST_PAGE_SIZE;
                    all_items.extend(page_items);
                    if is_last_page || all_items.len() >= SHUFFLE_MAX_ITEMS {
                        break;
                    }
                    page += 1;
                }
                all_items.truncate(SHUFFLE_MAX_ITEMS);

                let videos: Vec<_> = all_items
                    .iter()
                    .filter(|item| item.item_type == ItemType::Media)
                    .collect();
                if videos.is_empty() {
                    return Ok(None);
                }

                let random_idx = rand::random_range(0..videos.len());
                let random_item = videos[random_idx];

                Ok(Some(NextPlayItem {
                    name: random_item.name.clone(),
                    item_type: random_item.item_type,
                    source_config: build_next_source_config(&build_full_path(
                        &Self::decode_target(Some(&random_item.target))?.ok_or_else(|| {
                            ProviderError::InvalidConfig("Missing Alist item target".to_string())
                        })?,
                    )),
                    target: random_item.target.clone(),
                }))
            }
        }
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        target: Option<&crate::models::ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(relative_path) = Self::decode_target(target)? else {
            return Ok(Vec::new());
        };

        let mut segments = Vec::new();
        for (index, segment) in relative_path
            .split('/')
            .filter(|segment| !segment.is_empty())
            .enumerate()
        {
            let target = Self::encode_target(
                &relative_path
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .take(index + 1)
                    .collect::<Vec<_>>()
                    .join("/"),
            )?;
            segments.push(DynamicBrowsePathSegment {
                name: segment.to_string(),
                target,
            });
        }

        Ok(segments)
    }
}

#[cfg(test)]
mod tests {
    use super::AlistProvider;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn ok<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> TestResult<T> {
        result.map_err(|error| anyhow::anyhow!("{context}: {error}"))
    }

    #[test]
    fn otp_code_matches_rfc6238_sha1_vector_truncated_to_six_digits() -> TestResult {
        let code = ok(
            AlistProvider::otp_code_at_timestamp("GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ", 59),
            "RFC test vector secret should decode",
        )?;

        assert_eq!(code, "287082");
        Ok(())
    }

    #[test]
    fn otp_secret_normalization_accepts_otpauth_uri() {
        assert_eq!(
            AlistProvider::normalize_otp_secret(Some(
                "otpauth://totp/Alist:admin?secret=JBSWY3DPEHPK3PXP&issuer=Alist".to_string()
            ))
            .as_deref(),
            Some("JBSWY3DPEHPK3PXP")
        );
    }
}
