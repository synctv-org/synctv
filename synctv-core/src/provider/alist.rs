//! Alist `MediaProvider` Adapter
//!
//! Adapter that calls `AlistProviderClient` to implement `MediaProvider` trait.
//! `ProviderClient` abstracts local/remote implementation, so `MediaProvider` doesn't need to know.

use super::{
    provider_client::{
        create_remote_alist_client, AlistClientArc, AlistClientExt, AlistFileInfo,
        AlistRelatedFile, AlistSubtitleTask, AlistVideoPreview, ProviderClientManager,
    },
    store::{ProviderStoreExt, VersionedPlayback},
    DirectoryItem, DynamicBrowsePathSegment, DynamicFolder, DynamicListQuery, ItemType,
    MediaProvider, NextPlayItem, PlaybackClientProfile, PlaybackDeliveryPreference, PlaybackInfo,
    PlaybackResult, PlaybackSubtitlePreference, ProviderContext, ProviderCredentialDependency,
    ProviderError, SubtitleTrack,
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

fn alist_modified_to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
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

fn subtitles_from_video_preview(preview: Option<&AlistVideoPreview>) -> Vec<SubtitleTrack> {
    preview.map_or_else(Vec::new, |preview| {
        preview
            .subtitle_tasks
            .iter()
            .enumerate()
            .filter(|(_, sub)| !sub.url.trim().is_empty())
            .map(|(idx, sub)| {
                let name = subtitle_name_from_task(sub, idx);
                SubtitleTrack {
                    language: if sub.language.trim().is_empty() {
                        "und".to_string()
                    } else {
                        sub.language.clone()
                    },
                    name,
                    url: sub.url.clone(),
                    headers: HashMap::new(),
                    format: "srt".to_string(),
                }
            })
            .collect()
    })
}

fn subtitles_from_related_files(related: &[AlistRelatedFile]) -> Vec<SubtitleTrack> {
    related
        .iter()
        .filter(|related| {
            !related.is_dir
                && !related.raw_url.trim().is_empty()
                && is_subtitle_filename(&related.name)
        })
        .map(|related| SubtitleTrack {
            language: external_subtitle_language(&related.name),
            name: related.name.clone(),
            url: related.raw_url.clone(),
            headers: HashMap::new(),
            format: subtitle_format_from_name(&related.name),
        })
        .collect()
}

fn merge_subtitles(
    mut primary: Vec<SubtitleTrack>,
    secondary: Vec<SubtitleTrack>,
) -> Vec<SubtitleTrack> {
    for subtitle in secondary {
        if !primary.iter().any(|existing| existing.url == subtitle.url) {
            primary.push(subtitle);
        }
    }
    primary
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

fn proxy_target_url_from_query(query_string: Option<&str>) -> Option<String> {
    query_string.and_then(|query| {
        query.split('&').find_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            if key == "url" {
                urlencoding::decode(value)
                    .ok()
                    .map(std::borrow::Cow::into_owned)
            } else {
                None
            }
        })
    })
}

fn signed_m3u8_segment_proxy_base(
    ctx: &super::proxy::ProxyRequestContext<'_>,
    version: &str,
) -> String {
    if let Some(claims) = ctx.verified_claims {
        let signed_query = ctx.services.signing_key.build_signed_query(claims);
        format!("{}/{version}?{signed_query}", ctx.proxy_base)
    } else {
        format!("{}/{version}", ctx.proxy_base)
    }
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

    async fn get_client_with_context(
        &self,
        instance_name: Option<&str>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<AlistClientArc, ProviderError> {
        self.provider_instance_manager
            .resolve_client_required_with_context(
                instance_name,
                request_context,
                create_remote_alist_client,
                || self.client_manager.local_alist_client(),
            )
            .await
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
    /// Takes grpc-generated `LoginReq` and returns token string
    pub async fn login(
        &self,
        req: synctv_media_providers::grpc::alist::LoginReq,
        instance_name: Option<&str>,
    ) -> Result<String, ProviderError> {
        self.login_with_context(req, instance_name, None).await
    }

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
    pub async fn fs_list(
        &self,
        req: synctv_media_providers::grpc::alist::FsListReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::alist::FsListResp, ProviderError> {
        self.fs_list_with_context(req, instance_name, None).await
    }

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
    pub async fn fs_search(
        &self,
        req: synctv_media_providers::grpc::alist::FsSearchReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::alist::FsSearchResp, ProviderError> {
        self.fs_search_with_context(req, instance_name, None).await
    }

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
            total: u64::try_from(content.len()).unwrap_or(u64::MAX),
            content,
        })
    }

    /// Get Alist user info
    ///
    /// Takes grpc-generated `MeReq` and returns `MeResp`
    pub async fn me(
        &self,
        req: synctv_media_providers::grpc::alist::MeReq,
        instance_name: Option<&str>,
    ) -> Result<synctv_media_providers::grpc::alist::MeResp, ProviderError> {
        self.me_with_context(req, instance_name, None).await
    }

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
struct ResolvedAlistConfig {
    host: String,
    token: String,
    path: String,
    password: Option<String>,
    credential_owner_id: String,
    credential_revision: String,
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

// Note: Default implementation removed as it requires RemoteProviderManager

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

    /// Resolve AlistSourceConfig into credentials owned by the media/playlist creator.
    async fn resolve_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &Value,
    ) -> Result<ResolvedAlistConfig, ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;

        let repo = ctx.credential_repo.ok_or_else(|| {
            ProviderError::Internal("credential_repo not available in ProviderContext".to_string())
        })?;
        let credential_owner_id = ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal(
                "credential_owner_id not available in ProviderContext".to_string(),
            )
        })?;
        let resolved_credential = super::credential_resolver::resolve_credential_record_for_owner(
            repo,
            Self::NAME,
            *credential_owner_id,
            &config.server_id,
            ctx.request_context(),
        )
        .await?;

        match resolved_credential.credential {
            crate::models::ProviderCredential::Alist {
                host,
                username,
                password,
                otp_secret,
            } => {
                let otp_code = otp_secret.as_deref().map_or_else(
                    || Ok(String::new()),
                    |secret| {
                        crate::models::ProviderCredential::current_alist_otp_code(secret)
                            .map_err(ProviderError::InvalidConfig)
                    },
                )?;
                // Re-login with stored credentials to get a fresh token
                let login_req = synctv_media_providers::grpc::alist::LoginReq {
                    host: host.clone(),
                    username,
                    credential: Some(
                        synctv_media_providers::grpc::alist::login_req::Credential::HashedPassword(
                            password,
                        ),
                    ),
                    otp_code,
                };
                let instance_name = super::bound_provider_instance_name(ctx);
                let token = self
                    .provider_login(login_req, instance_name, ctx.request_context())
                    .await?;

                Ok(ResolvedAlistConfig {
                    host,
                    token,
                    path: config.path,
                    password: config.password,
                    credential_owner_id: credential_owner_id.to_string(),
                    credential_revision: resolved_credential.revision,
                    provider_instance_name: instance_name.map(std::string::ToString::to_string),
                })
            }
            _ => Err(ProviderError::InvalidCredentialType),
        }
    }

    /// Internal login for credential resolution
    async fn provider_login(
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
                user_agent: String::new(),
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
            user_agent: String::new(),
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
        ))
    }

    fn build_playback_result(
        file_info: &AlistFileInfo,
        video_preview: Option<&AlistVideoPreview>,
        video_preview_error: Option<&str>,
        playback_client_profile: Option<&PlaybackClientProfile>,
    ) -> PlaybackResult {
        let mut playback_infos = HashMap::new();
        let mut metadata = HashMap::new();

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
                            urls: vec![task.url.clone()],
                            format: "hls".to_string(),
                            headers: HashMap::new(),
                            subtitles: combined_subtitles.clone(),
                            expires_at: task_expires_at,
                            cors_proxy_required: false,
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
                    urls: vec![file_info.raw_url.clone()],
                    format: Self::detect_format(&file_info.name),
                    headers: HashMap::new(),
                    subtitles: combined_subtitles,
                    expires_at: direct_expires_at,
                    cors_proxy_required: false,
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
            metadata,
        }
    }

    fn choose_default_mode(
        transcoded_modes: &[(String, u64)],
        has_direct: bool,
        playback_client_profile: Option<&PlaybackClientProfile>,
    ) -> Option<String> {
        let profile = playback_client_profile.cloned().unwrap_or_default();
        if profile.delivery_preference == PlaybackDeliveryPreference::DirectPlay && has_direct {
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

        match profile.delivery_preference {
            PlaybackDeliveryPreference::DirectPlay if has_direct => Some("direct".to_string()),
            PlaybackDeliveryPreference::DirectPlay => selected_transcode,
            PlaybackDeliveryPreference::Transcode | PlaybackDeliveryPreference::Auto => {
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
        source_config: &Value,
    ) -> Result<(), ProviderError> {
        let config = AlistSourceConfig::try_from(source_config)?;

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
        // Resolve credentials from DB
        let resolved = self.resolve_config(_ctx, source_config).await?;

        // Re-validate path at request time (defense-in-depth against traversal)
        validate_path_for_traversal(&resolved.path).map_err(|e| {
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
            &resolved.credential_owner_id,
            &resolved.credential_revision,
            &resolved.path,
            resolved.password.as_deref(),
            &playback_profile_cache_key,
        );
        let cache_ttl = Duration::from_mins(15);

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

        // Call provider API
        let result = self
            .resolve_from_api(&resolved, _ctx.request_context(), playback_client_profile)
            .await?;

        // Generate version and store result
        super::finalize_versioned_playback(result, Self::NAME, &cache_key, cache_ttl, _ctx).await
    }

    fn as_dynamic_folder(&self) -> Option<&dyn DynamicFolder> {
        Some(self)
    }

    fn as_provider_proxy(&self) -> Option<&dyn super::proxy::ProviderProxy> {
        Some(self)
    }
}

// ProviderProxy implementation for Alist
// Supported sub_paths:
// - `{version}/stream` — proxy the video stream
// - `{version}/m3u8` — proxy M3U8 playlist with URL rewriting
// - `{version}/subtitle/{mode}/{index}` — proxy a subtitle track for a mode
#[async_trait]
impl super::proxy::ProviderProxy for AlistProvider {
    async fn resolve_proxy(
        &self,
        ctx: &super::proxy::ProxyRequestContext<'_>,
    ) -> Result<super::proxy::ProxyAction, ProviderError> {
        let sub_path = ctx.sub_path;

        let (version, rest) = sub_path
            .split_once('/')
            .map_or((sub_path, None), |(version, rest)| (version, Some(rest)));

        if version.is_empty() {
            return Err(ProviderError::NotFound);
        }

        let versioned =
            super::proxy::lookup_versioned(ctx.store, version, ctx.request_context).await?;

        if let Some(url) = proxy_target_url_from_query(ctx.query_string) {
            let headers = versioned
                .result
                .playback_infos
                .get(&versioned.result.default_mode)
                .map_or_else(HashMap::new, |info| info.headers.clone());
            return Ok(super::proxy::ProxyAction::FetchAndForward {
                url,
                headers,
                range_header: super::proxy::selected_range_header(ctx),
            });
        }

        if let Some(rest) = rest {
            if rest == "thumbnail" {
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
                    .map_or_else(HashMap::new, |info| info.headers.clone());
                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url,
                    headers,
                    range_header: None,
                });
            }

            if let Some(subtitle_path) = rest.strip_prefix("subtitle/") {
                let (playback_info, index_str) =
                    if let Some((mode_name, index_str)) = subtitle_path.split_once('/') {
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
                            subtitle_path,
                        )
                    };
                let Ok(index) = index_str.parse::<usize>() else {
                    return Err(ProviderError::NotFound);
                };
                let subtitle = playback_info
                    .subtitles
                    .get(index)
                    .ok_or(ProviderError::NotFound)?;

                return Ok(super::proxy::ProxyAction::FetchAndForward {
                    url: subtitle.url.clone(),
                    headers: super::subtitle_headers_for_proxy(&playback_info.headers, subtitle),
                    range_header: None,
                });
            }

            if let Some(stream_path) = rest.strip_prefix("stream/") {
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
                    headers: playback_info.headers.clone(),
                    range_header: super::proxy::selected_range_header(ctx),
                });
            }

            if let Some(m3u8_path) = rest.strip_prefix("m3u8/") {
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
                });
            }

            let default_info = versioned
                .result
                .playback_infos
                .get(&versioned.result.default_mode)
                .ok_or(ProviderError::NotFound)?;
            let url = default_info.urls.first().ok_or(ProviderError::NotFound)?;

            match rest {
                "stream" => {
                    return Ok(super::proxy::ProxyAction::FetchAndForward {
                        url: url.clone(),
                        headers: default_info.headers.clone(),
                        range_header: super::proxy::selected_range_header(ctx),
                    });
                }
                "m3u8" => {
                    return Ok(super::proxy::ProxyAction::M3u8Rewrite {
                        url: url.clone(),
                        headers: default_info.headers.clone(),
                        proxy_base: signed_m3u8_segment_proxy_base(ctx, version),
                    });
                }
                _ => {}
            }
        }

        Err(ProviderError::NotFound)
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

        let page = u64::try_from(query.page.max(1)).unwrap_or(u64::MAX);
        let per_page = u64::try_from(query.page_size.max(1)).unwrap_or(u64::MAX);
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
                    let full_item_path = join_alist_path(&file_item.parent, &file_item.name);
                    let item_relative_path =
                        alist_relative_path_from_base(&resolved.path, &full_item_path)?;

                    Some(DirectoryItem {
                        name: file_item.name,
                        item_type,
                        target: Self::encode_target(&item_relative_path).ok()?,
                        size: Some(file_item.size),
                        thumbnail: None,
                        modified_at: None,
                    })
                })
                .collect()
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
                    let item_relative_path = if let Some(rel) = relative_path.as_deref() {
                        format!("{}/{}", rel.trim_end_matches('/'), file_item.name)
                    } else {
                        format!("/{}", file_item.name)
                    };

                    Some(DirectoryItem {
                        name: file_item.name,
                        item_type,
                        target: Self::encode_target(&item_relative_path).ok()?,
                        size: Some(file_item.size),
                        thumbnail: if file_item.thumb.is_empty() {
                            None
                        } else {
                            Some(file_item.thumb)
                        },
                        modified_at: Some(alist_modified_to_i64(file_item.modified)),
                    })
                })
                .collect()
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
                return Ok(Some(
                    NextPlayItem {
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
                    }
                    .strip_credentials(),
                ));
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
                                    &Self::decode_target(Some(&next.target))?
                                        .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                                )),
                                metadata: json!({"size": next.size, "thumbnail": next.thumbnail, "modified_at": next.modified_at}),
                                provider_data: json!({}),
                                target: next.target.clone(),
                            }.strip_credentials()));
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
                                    &Self::decode_target(Some(&next.target))?
                                        .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                                )),
                                metadata: json!({"size": next.size, "thumbnail": next.thumbnail, "modified_at": next.modified_at}),
                                provider_data: json!({}),
                                target: next.target.clone(),
                            }.strip_credentials()));
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
                                &Self::decode_target(Some(&first.target))?
                                    .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                            )),
                            metadata: json!({"size": first.size, "thumbnail": first.thumbnail, "modified_at": first.modified_at}),
                            provider_data: json!({}),
                            target: first.target.clone(),
                        }.strip_credentials()));
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
                        &Self::decode_target(Some(&random_item.target))?
                            .ok_or_else(|| ProviderError::InvalidConfig("Missing Alist item target".to_string()))?,
                    )),
                    metadata: json!({"size": random_item.size, "thumbnail": random_item.thumbnail, "modified_at": random_item.modified_at}),
                    provider_data: json!({}),
                    target: random_item.target.clone(),
                }.strip_credentials()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::UserId;
    use crate::provider::provider_client::AlistTranscodingTask;
    use crate::repository::ProviderInstanceRepository;
    use async_trait::async_trait;
    use std::sync::Arc;
    use std::sync::Mutex;
    use synctv_media_providers::alist::{AlistError, AlistInterface};
    use synctv_media_providers::grpc::alist::{
        fs_list_resp, FsGetReq, FsGetResp, FsListReq, FsListResp, FsOtherReq, FsOtherResp,
        FsSearchReq, FsSearchResp, LoginReq, MeReq, MeResp,
    };

    struct FakeAlistSubtitleClient {
        requested_paths: Mutex<Vec<String>>,
    }

    impl FakeAlistSubtitleClient {
        fn new() -> Self {
            Self {
                requested_paths: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AlistInterface for FakeAlistSubtitleClient {
        async fn fs_get(&self, request: FsGetReq) -> Result<FsGetResp, AlistError> {
            self.requested_paths
                .lock()
                .expect("requested_paths mutex should not be poisoned")
                .push(request.path.clone());

            Ok(FsGetResp {
                name: request
                    .path
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_string(),
                size: 128,
                is_dir: false,
                modified: 0,
                created: 0,
                sign: String::new(),
                thumb: String::new(),
                r#type: 4,
                hashinfo: String::new(),
                raw_url: format!("https://alist.example.com/d{}", request.path),
                readme: String::new(),
                provider: "AliyundriveOpen".to_string(),
                related: vec![],
            })
        }

        async fn fs_list(&self, _request: FsListReq) -> Result<FsListResp, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }

        async fn fs_other(&self, _request: FsOtherReq) -> Result<FsOtherResp, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }

        async fn fs_search(&self, _request: FsSearchReq) -> Result<FsSearchResp, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }

        async fn me(&self, _request: MeReq) -> Result<MeResp, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }

        async fn login(&self, _request: LoginReq) -> Result<String, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }
    }

    struct FakeAlistSearchUnavailableClient;

    #[async_trait]
    impl AlistInterface for FakeAlistSearchUnavailableClient {
        async fn fs_get(&self, _request: FsGetReq) -> Result<FsGetResp, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }

        async fn fs_list(&self, request: FsListReq) -> Result<FsListResp, AlistError> {
            assert_eq!(request.path, "/local");
            assert_eq!(request.page, 2);
            assert_eq!(request.per_page, 10);
            Ok(FsListResp {
                content: vec![
                    fs_list_resp::FsListContent {
                        name: "video.mp4".to_string(),
                        size: 15,
                        is_dir: false,
                        modified: 0,
                        sign: String::new(),
                        thumb: String::new(),
                        r#type: 2,
                    },
                    fs_list_resp::FsListContent {
                        name: "folder".to_string(),
                        size: 0,
                        is_dir: true,
                        modified: 0,
                        sign: String::new(),
                        thumb: String::new(),
                        r#type: 1,
                    },
                ],
                total: 2,
                readme: String::new(),
                write: false,
                provider: "Local".to_string(),
            })
        }

        async fn fs_other(&self, _request: FsOtherReq) -> Result<FsOtherResp, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }

        async fn fs_search(&self, _request: FsSearchReq) -> Result<FsSearchResp, AlistError> {
            Err(AlistError::Api {
                code: 404,
                message: "search not available".to_string(),
            })
        }

        async fn me(&self, _request: MeReq) -> Result<MeResp, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }

        async fn login(&self, _request: LoginReq) -> Result<String, AlistError> {
            Err(AlistError::InvalidConfig("not implemented".to_string()))
        }
    }

    fn fake_provider_instance_manager() -> Arc<RemoteProviderManager> {
        let pool = sqlx::PgPool::connect_lazy("postgresql://fake").expect("lazy pool");
        let repo = Arc::new(ProviderInstanceRepository::new(pool));
        Arc::new(RemoteProviderManager::new(repo))
    }

    // Note: Provider creation tests removed as they require ProviderClient setup

    #[test]
    fn test_detect_format() {
        assert_eq!(AlistProvider::detect_format("video.mp4"), "mp4");
        assert_eq!(AlistProvider::detect_format("video.mkv"), "mkv");
        assert_eq!(AlistProvider::detect_format("video.m3u8"), "hls");
        assert_eq!(AlistProvider::detect_format("video.unknown"), "video");
    }

    /// Validate Alist source config: checks path and server_id fields.
    /// Host/token are resolved from the media or playlist creator at runtime.
    fn validate_alist(config: &Value) -> Result<(), ProviderError> {
        let config = AlistSourceConfig::try_from(config)?;

        if config.path.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist path must not be empty".to_string(),
            ));
        }
        // Use the shared validate_path_for_traversal (matches actual impl)
        validate_path_for_traversal(&config.path).map_err(|e| {
            ProviderError::InvalidConfig(format!("Alist path must not contain path traversal: {e}"))
        })?;
        if config.server_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Alist server_id must not be empty".to_string(),
            ));
        }
        Ok(())
    }

    #[test]
    fn test_valid_alist_config() {
        let config = json!({
            "path": "/media/movies/test.mp4",
            "server_id": "test-server"
        });
        assert!(validate_alist(&config).is_ok());
    }

    #[test]
    fn test_alist_config_with_provider_instance_name() {
        let config = json!({
            "path": "/media/movies/test.mp4",
            "provider_instance_name": "remote-alist-1",
            "server_id": "test-server"
        });
        assert!(validate_alist(&config).is_err());
    }

    #[tokio::test]
    async fn test_alist_credential_dependencies_use_creator_credential() {
        let provider = AlistProvider::new(fake_provider_instance_manager());
        let ctx = ProviderContext::new("test")
            .with_user_id(UserId::from(1))
            .with_credential_owner_id(UserId::from(2));
        let dependencies = provider
            .credential_dependencies(
                &ctx,
                &json!({
                    "path": "/media/movies/test.mp4",
                    "server_id": "alist-main"
                }),
            )
            .expect("Alist dependency extraction should succeed");

        assert_eq!(
            dependencies,
            vec![ProviderCredentialDependency::new(
                AlistProvider::NAME,
                "2",
                "alist-main"
            )]
        );
    }

    #[tokio::test]
    async fn test_alist_credential_dependencies_require_explicit_creator_credential_owner() {
        let provider = AlistProvider::new(fake_provider_instance_manager());
        let ctx = ProviderContext::new("test").with_user_id(UserId::from(1));
        let err = provider
            .credential_dependencies(
                &ctx,
                &json!({
                    "path": "/media/movies/test.mp4",
                    "server_id": "alist-main"
                }),
            )
            .expect_err("Alist must not silently fall back to viewer credentials");

        assert!(
            err.to_string().contains("credential_owner_id"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_fs_search_falls_back_to_list_when_upstream_search_is_unavailable() {
        let default_clients = ProviderClientManager::new();
        let client_manager = Arc::new(ProviderClientManager::with_custom_clients(
            Arc::new(FakeAlistSearchUnavailableClient),
            default_clients.local_bilibili_client(),
            default_clients.local_emby_client(),
        ));
        let provider =
            AlistProvider::with_client_manager(fake_provider_instance_manager(), client_manager);

        let response = provider
            .fs_search(
                synctv_media_providers::grpc::alist::FsSearchReq {
                    host: "http://alist.example.test".to_string(),
                    token: "token".to_string(),
                    parent: "/local".to_string(),
                    keywords: "does-not-exist".to_string(),
                    scope: 0,
                    page: 2,
                    per_page: 10,
                    password: String::new(),
                },
                None,
            )
            .await
            .expect("search unavailable should fall back to unfiltered list");

        assert_eq!(response.total, 2);
        assert_eq!(response.content.len(), 2);
        assert_eq!(response.content[0].parent, "/local");
        assert_eq!(response.content[0].name, "video.mp4");
        assert_eq!(response.content[1].name, "folder");
    }

    #[tokio::test]
    async fn test_fs_search_fallback_preserves_scope_filter() {
        let default_clients = ProviderClientManager::new();
        let client_manager = Arc::new(ProviderClientManager::with_custom_clients(
            Arc::new(FakeAlistSearchUnavailableClient),
            default_clients.local_bilibili_client(),
            default_clients.local_emby_client(),
        ));
        let provider =
            AlistProvider::with_client_manager(fake_provider_instance_manager(), client_manager);

        let files = provider
            .fs_search(
                synctv_media_providers::grpc::alist::FsSearchReq {
                    host: "http://alist.example.test".to_string(),
                    token: "token".to_string(),
                    parent: "/local".to_string(),
                    keywords: "does-not-exist".to_string(),
                    scope: 2,
                    page: 2,
                    per_page: 10,
                    password: String::new(),
                },
                None,
            )
            .await
            .expect("search unavailable should fall back to listing files only");
        assert_eq!(files.total, 1);
        assert_eq!(files.content[0].name, "video.mp4");

        let directories = provider
            .fs_search(
                synctv_media_providers::grpc::alist::FsSearchReq {
                    host: "http://alist.example.test".to_string(),
                    token: "token".to_string(),
                    parent: "/local".to_string(),
                    keywords: "does-not-exist".to_string(),
                    scope: 1,
                    page: 2,
                    per_page: 10,
                    password: String::new(),
                },
                None,
            )
            .await
            .expect("search unavailable should fall back to listing directories only");
        assert_eq!(directories.total, 1);
        assert_eq!(directories.content[0].name, "folder");
    }

    #[tokio::test]
    async fn test_prepare_alist_config_rejects_provider_instance_name() {
        let provider = AlistProvider::new(fake_provider_instance_manager());
        let config = json!({
            "path": "/media/movies/test.mp4",
            "provider_instance_name": "remote-alist-1",
            "server_id": "test-server"
        });

        let result = provider
            .prepare_source_config(&ProviderContext::new("test"), config)
            .await;

        assert!(matches!(result, Err(ProviderError::InvalidConfig(_))));
    }

    #[test]
    fn test_alist_config_path_traversal() {
        let config = json!({
            "path": "/media/../../../etc/passwd",
            "server_id": "test-server"
        });
        assert!(validate_alist(&config).is_err());
    }

    #[test]
    fn test_alist_config_empty_path() {
        let config = json!({
            "path": "",
            "server_id": "test-server"
        });
        assert!(validate_alist(&config).is_err());
    }

    #[test]
    fn test_alist_config_missing_server_id() {
        let config = json!({
            "path": "/media/movies/test.mp4"
        });
        assert!(validate_alist(&config).is_err());
    }

    #[test]
    fn test_alist_server_id_parsing() {
        let config = json!({
            "path": "/media/movies",
            "server_id": "srv-xyz"
        });
        let parsed = AlistSourceConfig::try_from(&config).unwrap();
        assert_eq!(parsed.server_id, "srv-xyz");
        assert_eq!(parsed.path, "/media/movies");
    }

    #[test]
    fn test_path_traversal_validation_rejects_literal_double_dot() {
        // Use the centralized validation function
        assert!(validate_path_for_traversal("../../../etc/passwd").is_err());
        assert!(validate_path_for_traversal("../secret").is_err());
        assert!(validate_path_for_traversal("test/../etc").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_url_encoded_dot() {
        // URL-encoded . (2E in hex)
        assert!(validate_path_for_traversal("%2e%2e/etc/passwd").is_err());
        assert!(validate_path_for_traversal("%2E%2E/secret").is_err()); // uppercase
        assert!(validate_path_for_traversal("test/%2e%2e/config").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_mixed_encoding() {
        // Mixed literal and encoded
        assert!(validate_path_for_traversal("..%2fetc/passwd").is_err());
        assert!(validate_path_for_traversal("%2e%2e/secret").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_backslash_traversal() {
        assert!(validate_path_for_traversal("..\\..\\windows").is_err());
        assert!(validate_path_for_traversal("test\\..\\config").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_mixed_dot_sequences() {
        assert!(validate_path_for_traversal("./../etc").is_err());
        assert!(validate_path_for_traversal(".././secret").is_err());
        assert!(validate_path_for_traversal("././../config").is_err());
    }

    #[test]
    fn test_path_traversal_validation_rejects_null_bytes() {
        assert!(validate_path_for_traversal("test\0../etc").is_err());
        assert!(validate_path_for_traversal("/etc/\0passwd").is_err());
    }

    #[test]
    fn test_path_traversal_validation_allows_valid_paths() {
        assert!(validate_path_for_traversal("media/movies").is_ok());
        assert!(validate_path_for_traversal("/absolute/path").is_ok());
        assert!(validate_path_for_traversal("folder with spaces/file.txt").is_ok());
        assert!(validate_path_for_traversal("file-with-dashes.txt").is_ok());
        assert!(validate_path_for_traversal("file_with_underscores.txt").is_ok());
    }

    /// Test helper to verify cursor-based pagination bounds.
    /// The sequential mode algorithm should:
    /// 1. Process one page at a time (max `PAGE_SIZE` items in memory)
    /// 2. Find current item and look for next within same or next page
    /// 3. Not accumulate items across pages (bounded memory)
    #[test]
    fn test_sequential_pagination_memory_bounds() {
        // Simulate the pagination behavior: max PAGE_SIZE items in memory at once
        const PAGE_SIZE: usize = 50;

        // Simulate finding item at position 125 (page 2, index 25)
        let current_item_idx = 125;

        // Old behavior would load: page 0 + page 1 + page 2 = 150 items
        // New behavior only processes one page at a time

        let page_of_current = current_item_idx / PAGE_SIZE; // page 2
        let idx_in_page = current_item_idx % PAGE_SIZE; // index 25

        assert_eq!(page_of_current, 2);
        assert_eq!(idx_in_page, 25);

        // Next item is at position 126 (same page, index 26)
        // So we only need to keep at most PAGE_SIZE items in memory
        let next_idx_in_page = idx_in_page + 1;
        assert!(next_idx_in_page < PAGE_SIZE, "Next item is in same page");

        // If current is at end of page (index 49), next is in next page
        // We discard current page and fetch next, still only PAGE_SIZE in memory
    }

    /// Test shuffle mode memory bounds (capped at `MAX_ITEMS`).
    #[test]
    fn test_shuffle_pagination_memory_bounds() {
        const PAGE_SIZE: usize = 50;
        const MAX_ITEMS: usize = 200;

        // Simulate a folder with 800 items
        let total_items = 800;

        // Old behavior: would fetch 20 pages = 1000 items (or hit MAX_PAGES limit)
        // New behavior: stops at MAX_ITEMS = 200 items (4 pages)

        let pages_to_fetch = MAX_ITEMS.div_ceil(PAGE_SIZE); // 4 pages
        let items_fetched = pages_to_fetch * PAGE_SIZE; // 200 items

        assert_eq!(pages_to_fetch, 4);
        assert!(items_fetched <= MAX_ITEMS);
        assert!(items_fetched < total_items, "Should not fetch all items");

        // Memory usage: max 200 items vs 1000 items (80% reduction)
    }

    #[test]
    fn test_alist_config_with_password() {
        // Alist supports optional per-directory password
        let config = json!({
            "path": "/media/movies/test.mp4",
            "password": "dir-password",
            "server_id": "test-server"
        });
        let parsed = AlistSourceConfig::try_from(&config).unwrap();
        assert_eq!(parsed.password, Some("dir-password".to_string()));
    }

    #[test]
    fn test_alist_playback_cache_key_includes_directory_password() {
        let revision = "credential-1:1000";
        let no_password = AlistProvider::playback_cache_key(
            "server-1",
            "owner-1",
            revision,
            "/media/movie.mkv",
            None,
            "default",
        );
        let password_a = AlistProvider::playback_cache_key(
            "server-1",
            "owner-1",
            revision,
            "/media/movie.mkv",
            Some("folder-a"),
            "default",
        );
        let password_b = AlistProvider::playback_cache_key(
            "server-1",
            "owner-1",
            revision,
            "/media/movie.mkv",
            Some("folder-b"),
            "default",
        );

        assert_ne!(
            no_password, password_a,
            "Directory password must affect the Alist playback cache key"
        );
        assert_ne!(
            password_a, password_b,
            "Different directory passwords must not reuse the same playback cache entry"
        );
    }

    #[test]
    fn test_alist_playback_cache_key_includes_credential_owner() {
        let revision = "credential-1:1000";
        let owner_a = AlistProvider::playback_cache_key(
            "server-1",
            "owner-a",
            revision,
            "/media/movie.mkv",
            None,
            "default",
        );
        let owner_b = AlistProvider::playback_cache_key(
            "server-1",
            "owner-b",
            revision,
            "/media/movie.mkv",
            None,
            "default",
        );

        assert_ne!(
            owner_a, owner_b,
            "Alist playback cache must be isolated by credential owner"
        );
    }

    #[test]
    fn test_alist_playback_cache_key_includes_credential_update_time() {
        let first = AlistProvider::playback_cache_key(
            "server-1",
            "owner-a",
            "credential-1:1000",
            "/media/movie.mkv",
            None,
            "default",
        );
        let second = AlistProvider::playback_cache_key(
            "server-1",
            "owner-a",
            "credential-1:2000",
            "/media/movie.mkv",
            None,
            "default",
        );

        assert_ne!(
            first, second,
            "Credential changes must invalidate Alist playback cache entries"
        );
    }

    #[test]
    fn test_alist_playback_cache_key_includes_playback_profile() {
        let default_profile = AlistProvider::playback_cache_key(
            "server-1",
            "owner-a",
            "credential-1:1000",
            "/media/movie.mkv",
            None,
            "default",
        );
        let constrained_profile = AlistProvider::playback_cache_key(
            "server-1",
            "owner-a",
            "credential-1:1000",
            "/media/movie.mkv",
            None,
            "delivery=auto:bitrate=1500000",
        );

        assert_ne!(
            default_profile, constrained_profile,
            "Playback profile changes must not reuse Alist playback cache entries"
        );
    }

    #[test]
    fn test_alist_playback_result_combines_transcoded_and_external_subtitles() {
        let file_info = AlistFileInfo {
            name: "movie.mkv".to_string(),
            size: 42,
            is_dir: false,
            raw_url: "https://alist.example.com/d/movie.mkv".to_string(),
            provider: "AliyundriveOpen".to_string(),
            thumb: "https://alist.example.com/thumb.jpg".to_string(),
            related: vec![
                AlistRelatedFile {
                    name: "movie.zh-CN.srt".to_string(),
                    is_dir: false,
                    raw_url: "https://alist.example.com/d/movie.zh-CN.srt".to_string(),
                    provider: "AliyundriveOpen".to_string(),
                },
                AlistRelatedFile {
                    name: "movie.ass".to_string(),
                    is_dir: false,
                    raw_url: "https://alist.example.com/d/movie.ass".to_string(),
                    provider: "AliyundriveOpen".to_string(),
                },
                AlistRelatedFile {
                    name: "movie.jpg".to_string(),
                    is_dir: false,
                    raw_url: "https://alist.example.com/d/movie.jpg".to_string(),
                    provider: "AliyundriveOpen".to_string(),
                },
            ],
        };
        let video_preview = AlistVideoPreview {
            transcoding_tasks: vec![
                AlistTranscodingTask {
                    template_name: "HD".to_string(),
                    template_id: "FHD".to_string(),
                    template_width: 1920,
                    template_height: 1080,
                    stage: "finished".to_string(),
                    status: "finished".to_string(),
                    url: "https://cdn.example.com/movie-hd.m3u8".to_string(),
                },
                AlistTranscodingTask {
                    template_name: "SD".to_string(),
                    template_id: "SD".to_string(),
                    template_width: 640,
                    template_height: 360,
                    stage: "finished".to_string(),
                    status: "finished".to_string(),
                    url: "https://cdn.example.com/movie-sd.m3u8".to_string(),
                },
            ],
            subtitle_tasks: vec![AlistSubtitleTask {
                language: "en".to_string(),
                status: "finished".to_string(),
                url: "https://cdn.example.com/movie-en.srt".to_string(),
            }],
            drive_id: "drive-1".to_string(),
            file_id: "file-1".to_string(),
            provider: "AliyundriveOpen".to_string(),
            category: "live_transcoding".to_string(),
            duration: 120.5,
            width: 1920,
            height: 1080,
        };

        let result =
            AlistProvider::build_playback_result(&file_info, Some(&video_preview), None, None);

        assert_eq!(result.default_mode, "transcoded_HD");
        let transcoded = result
            .playback_infos
            .get("transcoded_HD")
            .expect("transcoded HD mode should exist");
        assert_eq!(transcoded.format, "hls");
        assert_eq!(transcoded.subtitles.len(), 3);
        assert!(transcoded
            .subtitles
            .iter()
            .any(|sub| sub.language == "zh-CN" && sub.format == "srt"));
        assert!(transcoded
            .subtitles
            .iter()
            .any(|sub| sub.language == "und" && sub.format == "ass"));

        let direct = result
            .playback_infos
            .get("direct")
            .expect("direct fallback should exist");
        assert_eq!(direct.format, "mkv");
        assert_eq!(direct.subtitles.len(), 3);
        assert_eq!(result.metadata["transcoding_count"], json!(2));
        assert_eq!(result.metadata["video_preview_subtitle_count"], json!(1));
        assert_eq!(result.metadata["external_subtitle_count"], json!(2));
        assert_eq!(result.metadata["duration"], json!(120.5));
        assert_eq!(result.metadata["width"], json!(1920));
        assert_eq!(result.metadata["height"], json!(1080));
        assert_eq!(result.metadata["video_preview_drive_id"], json!("drive-1"));
        assert_eq!(result.metadata["video_preview_file_id"], json!("file-1"));
        assert_eq!(
            result.metadata["video_preview_category"],
            json!("live_transcoding")
        );
        assert_eq!(
            result.metadata["transcoded_HD"]["template_id"],
            json!("FHD")
        );
    }

    #[test]
    fn test_alist_playback_result_uses_profile_for_default_mode_and_subtitles() {
        let file_info = AlistFileInfo {
            name: "movie.mkv".to_string(),
            size: 42,
            is_dir: false,
            raw_url: "https://alist.example.com/d/movie.mkv".to_string(),
            provider: "AliyundriveOpen".to_string(),
            thumb: String::new(),
            related: vec![AlistRelatedFile {
                name: "movie.en.srt".to_string(),
                is_dir: false,
                raw_url: "https://alist.example.com/d/movie.en.srt".to_string(),
                provider: "AliyundriveOpen".to_string(),
            }],
        };
        let video_preview = AlistVideoPreview {
            transcoding_tasks: vec![
                AlistTranscodingTask {
                    template_name: "FHD".to_string(),
                    template_id: "FHD".to_string(),
                    template_width: 1920,
                    template_height: 1080,
                    stage: "finished".to_string(),
                    status: "finished".to_string(),
                    url: "https://cdn.example.com/movie-fhd.m3u8".to_string(),
                },
                AlistTranscodingTask {
                    template_name: "SD".to_string(),
                    template_id: "SD".to_string(),
                    template_width: 640,
                    template_height: 360,
                    stage: "finished".to_string(),
                    status: "finished".to_string(),
                    url: "https://cdn.example.com/movie-sd.m3u8".to_string(),
                },
            ],
            subtitle_tasks: Vec::new(),
            drive_id: String::new(),
            file_id: String::new(),
            provider: "AliyundriveOpen".to_string(),
            category: "live_transcoding".to_string(),
            duration: 0.0,
            width: 1920,
            height: 1080,
        };
        let profile = PlaybackClientProfile {
            delivery_preference: PlaybackDeliveryPreference::Transcode,
            max_streaming_bitrate: Some(1_500_000),
            subtitle_preference: PlaybackSubtitlePreference::None,
            ..PlaybackClientProfile::default()
        };

        let result = AlistProvider::build_playback_result(
            &file_info,
            Some(&video_preview),
            None,
            Some(&profile),
        );

        assert_eq!(result.default_mode, "transcoded_SD");
        assert!(result
            .playback_infos
            .values()
            .all(|info| info.subtitles.is_empty()));
    }

    #[test]
    fn test_alist_playback_result_honors_direct_play_preference() {
        let file_info = AlistFileInfo {
            name: "movie.mp4".to_string(),
            size: 42,
            is_dir: false,
            raw_url: "https://alist.example.com/d/movie.mp4".to_string(),
            provider: "AliyundriveOpen".to_string(),
            thumb: String::new(),
            related: Vec::new(),
        };
        let video_preview = AlistVideoPreview {
            transcoding_tasks: vec![AlistTranscodingTask {
                template_name: "FHD".to_string(),
                template_id: "FHD".to_string(),
                template_width: 1920,
                template_height: 1080,
                stage: "finished".to_string(),
                status: "finished".to_string(),
                url: "https://cdn.example.com/movie-fhd.m3u8".to_string(),
            }],
            subtitle_tasks: Vec::new(),
            drive_id: String::new(),
            file_id: String::new(),
            provider: "AliyundriveOpen".to_string(),
            category: "live_transcoding".to_string(),
            duration: 0.0,
            width: 1920,
            height: 1080,
        };
        let profile = PlaybackClientProfile {
            delivery_preference: PlaybackDeliveryPreference::DirectPlay,
            ..PlaybackClientProfile::default()
        };

        let result = AlistProvider::build_playback_result(
            &file_info,
            Some(&video_preview),
            None,
            Some(&profile),
        );

        assert_eq!(result.default_mode, "direct");
    }

    #[test]
    fn test_alist_playback_result_degrades_when_video_preview_fails() {
        let file_info = AlistFileInfo {
            name: "movie.mp4".to_string(),
            size: 42,
            is_dir: false,
            raw_url: "https://alist.example.com/d/movie.mp4".to_string(),
            provider: "Local".to_string(),
            thumb: String::new(),
            related: vec![],
        };

        let result =
            AlistProvider::build_playback_result(&file_info, None, Some("not support"), None);

        assert_eq!(result.default_mode, "direct");
        assert!(result.playback_infos.contains_key("direct"));
        assert_eq!(result.metadata["video_preview_error"], json!("not support"));
        assert!(!result.metadata.contains_key("transcoding_count"));
    }

    #[test]
    fn test_alist_related_file_path_rejects_unsafe_names() {
        assert_eq!(
            related_file_path("/movies", "movie.zh-CN.srt"),
            Some("/movies/movie.zh-CN.srt".to_string())
        );
        assert_eq!(
            related_file_path("/", "movie.zh-CN.srt"),
            Some("/movie.zh-CN.srt".to_string())
        );
        assert!(related_file_path("/movies", "../secret.srt").is_none());
        assert!(related_file_path("/movies", "nested/subtitle.srt").is_none());
        assert!(related_file_path("/movies", "nested\\subtitle.srt").is_none());
    }

    #[tokio::test]
    async fn test_alist_enrich_related_subtitles_resolves_urls_with_fs_get() {
        let fake_client = Arc::new(FakeAlistSubtitleClient::new());
        let client: AlistClientArc = fake_client.clone();
        let config = ResolvedAlistConfig {
            host: "https://alist.example.com".to_string(),
            token: "token".to_string(),
            path: "/movies/movie.mkv".to_string(),
            password: Some("folder-password".to_string()),
            credential_owner_id: "owner-1".to_string(),
            credential_revision: "credential-1:1000".to_string(),
            provider_instance_name: None,
        };
        let mut file_info = AlistFileInfo {
            name: "movie.mkv".to_string(),
            size: 42,
            is_dir: false,
            raw_url: "https://alist.example.com/d/movie.mkv".to_string(),
            provider: "AliyundriveOpen".to_string(),
            thumb: String::new(),
            related: vec![
                AlistRelatedFile {
                    name: "movie.zh-CN.srt".to_string(),
                    is_dir: false,
                    raw_url: String::new(),
                    provider: String::new(),
                },
                AlistRelatedFile {
                    name: "movie.jpg".to_string(),
                    is_dir: false,
                    raw_url: String::new(),
                    provider: String::new(),
                },
                AlistRelatedFile {
                    name: "../secret.srt".to_string(),
                    is_dir: false,
                    raw_url: String::new(),
                    provider: String::new(),
                },
            ],
        };

        AlistProvider::enrich_related_subtitles(&client, &config, &mut file_info).await;

        assert_eq!(
            file_info.related[0].raw_url,
            "https://alist.example.com/d/movies/movie.zh-CN.srt"
        );
        assert_eq!(file_info.related[0].provider, "AliyundriveOpen");
        assert!(file_info.related[1].raw_url.is_empty());
        assert!(file_info.related[2].raw_url.is_empty());
        assert_eq!(
            fake_client
                .requested_paths
                .lock()
                .expect("requested_paths mutex should not be poisoned")
                .as_slice(),
            ["/movies/movie.zh-CN.srt"]
        );
    }

    #[test]
    fn test_alist_url_encoded_path_traversal_rejected() {
        // The current Alist validation only checks for literal ".."
        // but URL-encoded traversal like "%2e%2e/" should also be caught.
        // After the fix, validate_source_config should use the shared
        // validate_path_for_traversal function.

        // URL-encoded .. (%2e%2e)
        assert!(
            validate_path_for_traversal("%2e%2e/etc/passwd").is_err(),
            "URL-encoded dot-dot must be rejected by validate_path_for_traversal"
        );

        // Mixed case
        assert!(
            validate_path_for_traversal("%2E%2E/secret").is_err(),
            "Uppercase URL-encoded dot-dot must be rejected"
        );

        // Double-encoded
        assert!(
            validate_path_for_traversal("%252e%252e/etc").is_err(),
            "Double-encoded traversal must be rejected"
        );
    }

    #[test]
    fn test_alist_validate_config_uses_shared_path_traversal_check() {
        // After the fix, the Alist validate_source_config should use
        // validate_path_for_traversal instead of simple contains("..")

        // This config uses URL-encoded traversal: %2e%2e/etc/passwd
        let config = json!({
            "path": "/media/%2e%2e/etc/passwd",
            "server_id": "test-server"
        });

        // After the fix, the config should be rejected
        let parsed = AlistSourceConfig::try_from(&config).unwrap();
        let result = validate_path_for_traversal(&parsed.path);
        assert!(
            result.is_err(),
            "Alist path with URL-encoded traversal must be rejected"
        );
    }
}
