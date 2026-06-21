//! Alist `MediaProvider` Adapter
//!
//! Adapter that calls `AlistProviderClient` to implement `MediaProvider` trait.
//! `ProviderClient` abstracts local/remote implementation, so `MediaProvider` doesn't need to know.

use super::{
    provider_client::{
        create_remote_alist_client, AlistClientArc, AlistClientExt, AlistFileInfo,
        AlistRelatedFile, AlistSubtitleTask, AlistVideoPreview, ProviderClientManager,
    },
    DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, DynamicListQuery, ItemType,
    MediaProvider, NextPlayItem, PlaybackClientProfile, PlaybackInfo, PlaybackResult,
    PlaybackStreamPreference, PlaybackSubtitlePreference, ProviderContext,
    ProviderCredentialDependency, ProviderError, SourceConfig,
};
use crate::models::media::{
    PlaybackAlistMedia, PlaybackAlistSubtitle, PlaybackExternalSubtitle, PlaybackMedia,
    PlaybackMediaProvider, PlaybackSubtitle, PlaybackSubtitleProvider,
};
use crate::service::RemoteProviderManager;
use crate::validation::validate_path_for_traversal;
use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const LIST_PAGE_SIZE: usize = 50;
const SHUFFLE_MAX_ITEMS: usize = 200;
const RELATED_SUBTITLE_FETCH_LIMIT: usize = 32;

fn alist_headers() -> HashMap<String, String> {
    HashMap::from([(
        "User-Agent".to_string(),
        synctv_media_providers::PROVIDER_USER_AGENT.to_string(),
    )])
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
    if let Some(thumbnail) = result
        .metadata
        .get("thumbnail")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
        .filter(|thumbnail| !thumbnail.trim().is_empty())
    {
        result.metadata.insert(
            "proxy_thumbnail_resource".to_string(),
            serde_json::json!({
                "version": version,
                "expires_at": expires_at,
                "resource": "thumbnail",
            }),
        );
        result
            .metadata
            .insert("thumbnail".to_string(), serde_json::json!(thumbnail));
    }

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
                    PlaybackMediaProvider::Alist(if proxy_is_hls {
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
                    }),
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
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct AlistBrowseTarget {
    relative_path: String,
}

impl AlistProvider {
    /// Provider type name constant.
    pub const NAME: &'static str = "alist";

    /// Create a new `AlistProvider` with `RemoteProviderManager`
    pub fn new(
        provider_instance_manager: Arc<RemoteProviderManager>,
    ) -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager,
            client_manager: Arc::new(ProviderClientManager::new()?),
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
        }
    }

    #[cfg(test)]
    pub fn new_local_only() -> Result<Self, ProviderError> {
        Ok(Self {
            provider_instance_manager:
                crate::service::remote_provider_manager::empty_provider_instance_manager(),
            client_manager: Arc::new(ProviderClientManager::new()?),
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

    /// Login to Alist
    ///
    /// Login to Alist and return a token string.
    ///
    /// Takes grpc-generated `LoginReq` and returns token string
    pub async fn login_with_context(
        &self,
        req: synctv_media_providers::grpc::alist::LoginReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<String, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.login(req).await.map_err(std::convert::Into::into)
    }

    /// List Alist directory
    ///
    /// Takes grpc-generated `FsListReq` and returns `FsListResp`
    pub async fn fs_list_with_context(
        &self,
        req: synctv_media_providers::grpc::alist::FsListReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::alist::FsListResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.fs_list(req).await.map_err(std::convert::Into::into)
    }

    /// Search Alist files and directories.
    pub async fn fs_search_with_context(
        &self,
        req: synctv_media_providers::grpc::alist::FsSearchReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::alist::FsSearchResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        match client.fs_search(req.clone()).await {
            Ok(resp) => Ok(resp),
            Err(error) if is_alist_search_unavailable(&error) => {
                Self::fs_search_fallback_to_listing(client, req).await
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn fs_search_fallback_to_listing(
        client: AlistClientArc,
        req: synctv_media_providers::grpc::alist::FsSearchReq,
    ) -> Result<synctv_media_providers::grpc::alist::FsSearchResp, ProviderError> {
        use synctv_media_providers::grpc::alist::fs_search_resp::FsSearchContent;
        use synctv_media_providers::grpc::alist::{FsListReq, FsSearchResp};

        let list_resp = client
            .fs_list(FsListReq {
                host: req.host,
                token: req.token,
                path: req.parent.clone(),
                password: req.password,
                page: req.page.max(1),
                per_page: req.per_page.max(1),
                refresh: false,
            })
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
            .map(|item| FsSearchContent {
                parent: req.parent.clone(),
                name: item.name,
                is_dir: item.is_dir,
                size: item.size,
                r#type: item.r#type,
            })
            .collect::<Vec<_>>();

        Ok(FsSearchResp {
            total: usize_to_u64(content.len(), "Alist search fallback total")?,
            content,
        })
    }

    /// Get Alist user info
    ///
    /// Takes grpc-generated `MeReq` and returns `MeResp`
    pub async fn me_with_context(
        &self,
        req: synctv_media_providers::grpc::alist::MeReq,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<synctv_media_providers::grpc::alist::MeResp, ProviderError> {
        let client = self
            .get_client_with_context(instance_name, request_context)
            .await?;
        client.me(req).await.map_err(std::convert::Into::into)
    }

    fn encode_target(relative_path: &str) -> Result<Vec<u8>, ProviderError> {
        serde_json::to_vec(&AlistBrowseTarget {
            relative_path: relative_path.to_string(),
        })
        .map_err(|e| ProviderError::InvalidConfig(format!("Failed to encode Alist target: {e}")))
    }

    fn decode_target(target: Option<&[u8]>) -> Result<Option<String>, ProviderError> {
        let Some(target) = target else {
            return Ok(None);
        };
        if target.is_empty() {
            return Ok(None);
        }

        let payload: AlistBrowseTarget = serde_json::from_slice(target)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid Alist target: {e}")))?;

        if payload.relative_path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist target relative_path cannot be empty".to_string(),
            ));
        }

        Ok(Some(payload.relative_path))
    }
}

/// Alist source configuration
#[derive(Debug, Deserialize, Serialize)]
struct AlistSourceConfig {
    path: String,
    #[serde(default)]
    password: Option<String>,
    /// Saved Alist credential server identifier.
    server_id: String,
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
    path: String,
    password: Option<String>,
    provider_instance_name: Option<String>,
}

impl TryFrom<&Value> for AlistSourceConfig {
    type Error = ProviderError;

    fn try_from(value: &Value) -> Result<Self, Self::Error> {
        super::reject_source_config_provider_instance_name(value, "Alist")?;
        super::reject_source_config_credential_ref(value, "Alist")?;
        super::parse_source_config(value, "Alist")
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

    /// Resolve AlistSourceConfig into a cached credential binding without logging in.
    async fn resolve_binding(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<ResolvedAlistBinding, ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;
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
        source_config: &Value,
    ) -> Result<ResolvedAlistConfig, ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;
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
            path: config.path,
            password: config.password,
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

            let request = synctv_media_providers::grpc::alist::FsGetReq {
                host: config.host.clone(),
                token: config.token.clone(),
                path,
                password: config.password.clone().unwrap_or_default(),
                headers: alist_headers(),
            };

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

        // Build proto request
        let request = synctv_media_providers::grpc::alist::FsGetReq {
            host: config.host.clone(),
            token: config.token.clone(),
            path: config.path.clone(),
            password: config.password.clone().unwrap_or_default(),
            headers: alist_headers(),
        };

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
        let mut metadata = HashMap::new();
        let mut duration_seconds = None;

        // Add basic metadata
        metadata.insert("name".to_string(), json!(&file_info.name));
        metadata.insert("size".to_string(), json!(file_info.size));
        metadata.insert("provider".to_string(), json!(&file_info.provider));
        if !file_info.thumb.is_empty() {
            metadata.insert("thumbnail".to_string(), json!(&file_info.thumb));
        }
        let related_subtitles = subtitles_from_related_files(&file_info.related);
        if !related_subtitles.is_empty() {
            metadata.insert(
                "external_subtitle_count".to_string(),
                json!(related_subtitles.len()),
            );
        }

        if let Some(error) = video_preview_error {
            metadata.insert("video_preview_error".to_string(), json!(error));
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
                    let task_expires_at = Some(Utc::now().timestamp() + 4 * 60 * 60);
                    let task_metadata = json!({
                        "template_id": task.template_id,
                        "template_name": task.template_name,
                        "template_width": task.template_width,
                        "template_height": task.template_height,
                        "stage": task.stage,
                        "status": task.status,
                    });

                    playback_infos.insert(
                        mode_name.clone(),
                        PlaybackInfo {
                            medias: vec![playback_media(
                                quality_name.clone(),
                                "hls".to_string(),
                                task_expires_at,
                                PlaybackMediaProvider::Alist(PlaybackAlistMedia::Direct {
                                    url: task.url.clone(),
                                    headers: headers.clone(),
                                }),
                            )],
                            default_media_index: None,
                            subtitles: combined_subtitles.clone(),
                            default_subtitle_index: None,
                            danmakus: Vec::new(),
                            default_danmaku_index: None,
                        },
                    );
                    metadata.insert(mode_name, task_metadata);
                }
            }

            // Add video metadata
            if !preview.drive_id.is_empty() {
                metadata.insert(
                    "video_preview_drive_id".to_string(),
                    json!(&preview.drive_id),
                );
            }
            if !preview.file_id.is_empty() {
                metadata.insert("video_preview_file_id".to_string(), json!(&preview.file_id));
            }
            if !preview.provider.is_empty() {
                metadata.insert(
                    "video_preview_provider".to_string(),
                    json!(&preview.provider),
                );
            }
            if !preview.category.is_empty() {
                metadata.insert(
                    "video_preview_category".to_string(),
                    json!(&preview.category),
                );
            }
            metadata.insert(
                "transcoding_count".to_string(),
                json!(preview.transcoding_tasks.len()),
            );
            metadata.insert(
                "video_preview_subtitle_count".to_string(),
                json!(preview.subtitle_tasks.len()),
            );
            metadata.insert("duration".to_string(), json!(preview.duration));
            metadata.insert("width".to_string(), json!(preview.width));
            metadata.insert("height".to_string(), json!(preview.height));
            if preview.duration.is_finite() && preview.duration > 0.0 {
                duration_seconds = Some(preview.duration);
            }
        }

        // Always add direct URL (raw_url) as fallback
        if !file_info.raw_url.is_empty() {
            // Alist raw URLs are provider-dependent. Use the same conservative
            // expiry window as AliyunDrive live transcoding when AList does not
            // return a per-URL expiry.
            let direct_expires_at = Some(Utc::now().timestamp() + 4 * 60 * 60);

            playback_infos.insert(
                "direct".to_string(),
                PlaybackInfo {
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
        let default_mode =
            Self::choose_default_mode(&transcoded_modes, has_direct, playback_client_profile)
                .unwrap_or_else(|| "direct".to_string());

        PlaybackResult {
            playback_infos,
            default_mode,
            provider: Self::NAME.to_string(),
            provider_instance_name,
            duration_seconds,
            metadata,
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
        let config = AlistSourceConfig::try_from(source_config.value())?;

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
        source_config: &Value,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;
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

    async fn prepare_source_config(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: Value,
    ) -> Result<Value, ProviderError> {
        super::reject_source_config_provider_instance_name(&source_config, "Alist")?;
        super::reject_source_config_credential_ref(&source_config, "Alist")?;

        Ok(source_config)
    }

    async fn generate_playback(
        &self,
        _ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<PlaybackResult, ProviderError> {
        // Resolve the credential binding first so playback cache hits do not
        // force an AList login/token refresh.
        let binding = self.resolve_binding(_ctx, source_config).await?;

        // Re-validate path at request time (defense-in-depth against traversal)
        validate_path_for_traversal(&binding.path).map_err(|e| {
            ProviderError::InvalidConfig(format!("Alist path must not contain path traversal: {e}"))
        })?;

        // Build cache key from server_id and path
        let config = AlistSourceConfig::try_from(source_config)?;
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

        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &cache_key,
            cache_ttl,
            _ctx,
            mark_alist_playback_resources,
            || async {
                let resolved = self.resolve_config(_ctx, source_config).await?;
                self.resolve_from_api(&resolved, _ctx.request_context(), playback_client_profile)
                    .await
            },
        )
        .await
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
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
            super::playback_transport::PlaybackTransportAction::M3u8Rewrite {
                url: url.to_string(),
                headers: media.upstream_headers(),
            },
        )
    }

    pub async fn get_transcoded_hls_segment(
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
            .and_then(|info| info.medias.first())
            .map_or_else(HashMap::new, PlaybackMedia::upstream_headers);
        super::playback_transport::transport_action_for_target_url(
            target_url,
            headers,
            range_header,
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
            .metadata
            .get("thumbnail")
            .and_then(serde_json::Value::as_str)
            .filter(|url| !url.trim().is_empty())
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

/// Implement `DynamicFolder` trait for Alist
///
/// Allows browsing Alist directories and getting next item for auto-play
#[async_trait]
impl DynamicFolder for AlistProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&[u8]>,
        query: DynamicListQuery,
    ) -> Result<Vec<DirectoryItem>, ProviderError> {
        // Parse playlist's source_config to get base path
        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;

        let resolved = self.resolve_config(ctx, config).await?;

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

        let page = usize_to_u64(query.page.max(1), "Alist page")?;
        let per_page = usize_to_u64(query.page_size.max(1), "Alist page size")?;
        let password = resolved.password.clone().unwrap_or_default();
        let search = query
            .search
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty());

        let items: Vec<DirectoryItem> = if let Some(keywords) = search {
            let search_resp = client
                .fs_search(synctv_media_providers::grpc::alist::FsSearchReq {
                    host: resolved.host.clone(),
                    token: resolved.token.clone(),
                    parent: full_path.clone(),
                    keywords: keywords.to_string(),
                    scope: 0,
                    page,
                    per_page,
                    password,
                })
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
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?
        } else {
            let list_resp = client
                .fs_list(synctv_media_providers::grpc::alist::FsListReq {
                    host: resolved.host.clone(),
                    token: resolved.token.clone(),
                    path: full_path.clone(),
                    password,
                    page,
                    per_page,
                    refresh: query.refresh,
                })
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
                            Some(file_item.thumb)
                        },
                        description: None,
                        modified_at: Some(alist_modified_to_i64(file_item.modified)?),
                    })
                })
                .collect::<Result<Vec<_>, ProviderError>>()?
        };

        Ok(items)
    }

    async fn resolve_item(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &[u8],
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let relative_path = Self::decode_target(Some(target))?
            .ok_or_else(|| ProviderError::InvalidConfig("Alist target is required".to_string()))?;
        validate_path_for_traversal(&relative_path)
            .map_err(|e| ProviderError::InvalidConfig(format!("Invalid relative path: {e}")))?;

        let config = playlist
            .source_config
            .as_ref()
            .ok_or_else(|| ProviderError::InvalidConfig("Missing source_config".to_string()))?;
        let base_config = AlistSourceConfig::try_from(config)?;

        let build_next_source_config = |full_path: &str| -> Value {
            json!({
                "path": full_path,
                "password": base_config.password,
                "server_id": base_config.server_id,
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
                    parent_target.as_deref(),
                    DynamicListQuery {
                        page,
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
                .find(|item| item.item_type == ItemType::Media && item.target == target)
            {
                return Ok(Some(NextPlayItem {
                    name: item.name.clone(),
                    item_type: item.item_type,
                    source_config: build_next_source_config(&build_full_path(&relative_path)),
                    metadata: json!({
                        "size": item.size,
                        "thumbnail": item.thumbnail,
                        "modified_at": item.modified_at
                    }),
                    provider_data: json!({}),
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
        _playing_media: &crate::models::Media,
        target: &[u8],
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
        let base_config = AlistSourceConfig::try_from(config)?;

        let build_next_source_config = |full_path: &str| -> Value {
            json!({
                "path": full_path,
                "password": base_config.password,
                "server_id": base_config.server_id,
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
                            parent_target.as_deref(),
                            DynamicListQuery {
                                page: current_page,
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
                                metadata: json!({"size": next.size, "thumbnail": next.thumbnail, "modified_at": next.modified_at}),
                                provider_data: json!({}),
                                target: next.target.clone(),
                            }));
                        }
                    } else if let Some(idx) =
                        page_items.iter().position(|item| item.target == target)
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
                                metadata: json!({"size": next.size, "thumbnail": next.thumbnail, "modified_at": next.modified_at}),
                                provider_data: json!({}),
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
                            parent_target.as_deref(),
                            DynamicListQuery {
                                page: 1,
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
                            metadata: json!({"size": first.size, "thumbnail": first.thumbnail, "modified_at": first.modified_at}),
                            provider_data: json!({}),
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
                            parent_target.as_deref(),
                            DynamicListQuery {
                                page,
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
                    metadata: json!({"size": random_item.size, "thumbnail": random_item.thumbnail, "modified_at": random_item.modified_at}),
                    provider_data: json!({}),
                    target: random_item.target.clone(),
                }))
            }
        }
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        target: Option<&[u8]>,
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
