//! QNAP QTS / QuTS hero File Station media provider.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use rand::seq::IndexedRandom;
use sha2::{Digest, Sha256};

use super::{
    DirectoryItem, DirectoryItemThumbnail, DynamicBrowsePathSegment, DynamicListQuery,
    DynamicListResult, DynamicPagination, DynamicPlaylistProvider, ItemType, MediaProvider,
    NextPlayItem, PlaybackInfo, PlaybackResult, ProviderContext, ProviderCredentialDependency,
    ProviderError, SourceConfig, SourceCover,
};
use crate::models::{
    detect_direct_url_format, normalize_provider_instance_name,
    normalize_provider_instance_name_owned, MediaSourceConfig, PlayMode, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata, PlaybackQnapMedia, PlaybackQnapSubtitle,
    PlaybackSubtitle, PlaybackSubtitleProvider, PlaylistSourceConfig, ProviderCredential,
    ProviderTarget, QnapMediaSourceConfig, QnapPlaybackMetadata, QnapPlaybackMode,
    QnapPlaybackResource, QnapPlaylistSourceConfig, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::qnap::{
    QnapClient, QnapFile, QnapHardwareTranscode, QnapLogin, QnapTranscodeResolution,
};

const PLAYBACK_CACHE_TTL: Duration = Duration::from_hours(2);
const DYNAMIC_MAX_SHUFFLE_ITEMS: usize = 200;
const RELATED_SUBTITLE_LIMIT: usize = 32;

#[derive(Debug, Clone)]
pub struct QnapBind {
    pub id: i64,
    pub server_id: String,
    pub endpoint: String,
    pub username: String,
    pub server_name: String,
    pub version: Option<String>,
    pub support_rtt: bool,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct QnapListItem {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_at: u64,
    pub file_type: u64,
    pub pre_transcoded_heights: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct QnapListResponse {
    pub content: Vec<QnapListItem>,
    pub total: u64,
    pub page: usize,
    pub has_more: bool,
    pub realtime_transcode: bool,
}

#[derive(Debug, Clone)]
pub struct QnapCapabilities {
    pub support_rtt: bool,
    pub hardware_transcode: bool,
    pub qtranscode: bool,
    pub multimedia_codec: bool,
    pub hd_station_support: bool,
}

#[derive(Clone)]
struct AuthenticatedQnap {
    client: QnapClient,
    login: QnapLogin,
    instance_name: Option<String>,
}

pub struct QnapProvider {
    http_client: reqwest::Client,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

impl QnapProvider {
    pub const NAME: &'static str = "qnap";

    #[must_use]
    pub fn with_http_client(http_client: reqwest::Client) -> Self {
        Self {
            http_client,
            credential_repo: None,
        }
    }

    #[must_use]
    pub fn with_credential_repo(
        &self,
        credential_repo: Arc<UserProviderCredentialRepository>,
    ) -> Self {
        Self {
            http_client: self.http_client.clone(),
            credential_repo: Some(credential_repo),
        }
    }

    fn credential_repo(&self) -> Result<&UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().ok_or_else(|| {
            ProviderError::Internal("QNAP credential repository is unavailable".to_string())
        })
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Result<&'a UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().or(fallback).ok_or_else(|| {
            ProviderError::Internal("QNAP credential repository is unavailable".to_string())
        })
    }

    fn client(&self, endpoint: &str) -> Result<QnapClient, ProviderError> {
        QnapClient::with_http_client(endpoint, self.http_client.clone()).map_err(Into::into)
    }

    #[must_use]
    pub fn credential_server_id_for_instance(
        endpoint: &str,
        provider_instance_name: Option<&str>,
    ) -> String {
        let endpoint = endpoint.trim().trim_end_matches('/');
        let instance = normalize_provider_instance_name(provider_instance_name).unwrap_or_default();
        hex::encode(Sha256::digest(format!("{endpoint}\n{instance}").as_bytes()))
    }

    pub async fn login_and_persist(
        &self,
        user_id: UserId,
        endpoint: String,
        username: String,
        password: String,
        provider_instance_name: Option<String>,
    ) -> Result<(String, QnapLogin), ProviderError> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "QNAP username and password are required".to_string(),
            ));
        }
        let client = self.client(&endpoint)?;
        let login = client.login(&username, &password).await?;
        let provider_instance_name = normalize_provider_instance_name_owned(provider_instance_name);
        let server_id =
            Self::credential_server_id_for_instance(&endpoint, provider_instance_name.as_deref());
        let now = Utc::now();
        self.credential_repo()?
            .upsert_by_user_provider_server(&UserProviderCredential {
                id: 0,
                user_id,
                provider: Self::NAME.to_string(),
                server_id: server_id.clone(),
                provider_instance_name,
                credential_data: ProviderCredential::Qnap {
                    endpoint,
                    username,
                    password,
                    sid: login.sid.clone(),
                    server_name: login.servername.clone(),
                    version: login.version.clone(),
                    support_rtt: login.support_rtt != 0,
                },
                expires_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok((server_id, login))
    }

    async fn authenticated_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedQnap, ProviderError> {
        let mut credential = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let instance_name = credential.provider_instance_name.clone();
        let ProviderCredential::Qnap {
            endpoint,
            username,
            password,
            sid,
            server_name,
            version,
            support_rtt,
        } = credential.credential_data.clone()
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        let client = self.client(&endpoint)?;
        let login = if client.shares(&sid).await.is_ok() {
            QnapLogin {
                status: 1,
                sid,
                servername: server_name,
                username: username.clone(),
                admingroup: 0,
                support_rtt: i64::from(support_rtt),
                version,
                build: None,
            }
        } else {
            let login = client.login(&username, &password).await?;
            credential.credential_data = ProviderCredential::Qnap {
                endpoint,
                username,
                password,
                sid: login.sid.clone(),
                server_name: login.servername.clone(),
                version: login.version.clone(),
                support_rtt: login.support_rtt != 0,
            };
            credential.updated_at = Utc::now();
            repo.upsert_by_user_provider_server(&credential)
                .await
                .map_err(|error| ProviderError::Internal(error.to_string()))?;
            login
        };
        Ok(AuthenticatedQnap {
            client,
            login,
            instance_name,
        })
    }

    async fn authenticated(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedQnap, ProviderError> {
        self.authenticated_with_repo(self.credential_repo()?, user_id, server_id)
            .await
    }

    pub async fn list(
        &self,
        user_id: UserId,
        server_id: &str,
        path: &str,
        page: usize,
        page_size: usize,
        search: Option<&str>,
    ) -> Result<(QnapListResponse, Option<String>), ProviderError> {
        self.list_with_repo(
            self.credential_repo()?,
            user_id,
            server_id,
            path,
            page,
            page_size,
            search,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn list_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
        path: &str,
        page: usize,
        page_size: usize,
        search: Option<&str>,
    ) -> Result<(QnapListResponse, Option<String>), ProviderError> {
        validate_path(path)?;
        let auth = self
            .authenticated_with_repo(repo, user_id, server_id)
            .await?;
        let page = page.max(1);
        let page_size = page_size.clamp(1, u32::MAX as usize);
        if path.trim_matches('/').is_empty() {
            let mut shares = auth.client.shares(&auth.login.sid).await?;
            let search = search.map(str::to_ascii_lowercase);
            shares.retain(|share| {
                search
                    .as_ref()
                    .is_none_or(|value| share.text.to_ascii_lowercase().contains(value))
            });
            let total = shares.len() as u64;
            let start = page.saturating_sub(1).saturating_mul(page_size);
            let content = shares
                .into_iter()
                .skip(start)
                .take(page_size)
                .map(|share| {
                    let name = if share.text.trim().is_empty() {
                        share.id.trim_matches('/').to_string()
                    } else {
                        share.text
                    };
                    let path = if share.id.starts_with('/') {
                        share.id
                    } else {
                        format!("/{name}")
                    };
                    QnapListItem {
                        name,
                        path,
                        is_dir: true,
                        size: share.real_total,
                        modified_at: 0,
                        file_type: 0,
                        pre_transcoded_heights: Vec::new(),
                    }
                })
                .collect();
            return Ok((
                QnapListResponse {
                    content,
                    total,
                    page,
                    has_more: u64::try_from(start.saturating_add(page_size)).unwrap_or(u64::MAX)
                        < total,
                    realtime_transcode: auth.login.support_rtt != 0,
                },
                auth.instance_name,
            ));
        }
        let offset = page.saturating_sub(1).saturating_mul(page_size) as u64;
        let response = auth
            .client
            .list(
                &auth.login.sid,
                path,
                offset,
                u32::try_from(page_size).unwrap_or(u32::MAX),
                search,
            )
            .await?;
        let total = response.total;
        let realtime_transcode = auth.login.support_rtt != 0 && response.rtt_support != 0;
        let content = response
            .datas
            .into_iter()
            .map(|file| map_list_item(path, file))
            .collect();
        Ok((
            QnapListResponse {
                content,
                total,
                page,
                has_more: offset.saturating_add(page_size as u64) < total,
                realtime_transcode,
            },
            auth.instance_name,
        ))
    }

    pub async fn capabilities(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(QnapCapabilities, Option<String>), ProviderError> {
        let auth = self.authenticated(user_id, server_id).await?;
        let hardware = auth.client.hardware_transcode(&auth.login.sid).await?;
        Ok((
            capabilities_from(&auth.login, &hardware),
            auth.instance_name,
        ))
    }

    pub async fn delete_credential(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let Some(credential) = self
            .credential_repo()?
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(false);
        };
        if let ProviderCredential::Qnap { endpoint, sid, .. } = &credential.credential_data {
            if let Ok(client) = self.client(endpoint) {
                let _ = client.logout(sid).await;
            }
        }
        self.credential_repo()?
            .delete(credential.id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(true)
    }

    pub async fn list_binds(
        &self,
        user_id: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<QnapBind>, ProviderError> {
        let requested = normalize_provider_instance_name(provider_instance_name);
        self.credential_repo()?
            .get_readable_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .filter(|credential| {
                requested
                    .is_none_or(|name| credential.provider_instance_name.as_deref() == Some(name))
            })
            .map(|credential| {
                let ProviderCredential::Qnap {
                    endpoint,
                    username,
                    server_name,
                    version,
                    support_rtt,
                    ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(QnapBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    endpoint,
                    username,
                    server_name,
                    version,
                    support_rtt,
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub async fn get_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Qnap(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "QNAP cached playback resource is invalid".to_string(),
            ));
        };
        let (owner, server_id, resource) = match provider {
            PlaybackQnapMedia::Refresh {
                credential_owner_id,
                server_id,
                resource,
            }
            | PlaybackQnapMedia::Proxy {
                credential_owner_id,
                server_id,
                resource,
                ..
            } => (credential_owner_id, server_id, resource),
        };
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        let url = playback_url(&auth.client, &auth.login.sid, resource)?;
        super::playback_transport::transport_action_for_target_url(
            url,
            QnapClient::auth_headers(),
            range_header,
        )
    }

    pub async fn get_subtitle(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        subtitle_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let subtitle = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.subtitles.get(subtitle_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackSubtitleProvider::Qnap(subtitle) = &subtitle.provider else {
            return Err(ProviderError::InvalidConfig(
                "QNAP cached subtitle resource is invalid".to_string(),
            ));
        };
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.first())
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Qnap(provider) = &media.provider else {
            return Err(ProviderError::NotFound);
        };
        let (owner, server_id) = match provider {
            PlaybackQnapMedia::Refresh {
                credential_owner_id,
                server_id,
                ..
            }
            | PlaybackQnapMedia::Proxy {
                credential_owner_id,
                server_id,
                ..
            } => (credential_owner_id, server_id),
        };
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            auth.client.download_url(&auth.login.sid, &subtitle.path)?,
            QnapClient::auth_headers(),
            None,
        )
    }

    pub async fn get_thumbnail(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(&versioned.result.default_mode)
            .or_else(|| versioned.result.playback_infos.values().next())
            .and_then(|info| info.medias.first())
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Qnap(provider) = &media.provider else {
            return Err(ProviderError::NotFound);
        };
        let (owner, server_id, path) = match provider {
            PlaybackQnapMedia::Refresh {
                credential_owner_id,
                server_id,
                resource,
            }
            | PlaybackQnapMedia::Proxy {
                credential_owner_id,
                server_id,
                resource,
                ..
            } => (credential_owner_id, server_id, &resource.path),
        };
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            auth.client.thumbnail_url(&auth.login.sid, path, 640)?,
            QnapClient::auth_headers(),
            None,
        )
    }

    pub async fn thumbnail_action(
        &self,
        user_id: UserId,
        server_id: &str,
        path: &str,
        size: u32,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        validate_file_path(path)?;
        let auth = self.authenticated(user_id, server_id).await?;
        super::playback_transport::transport_action_for_target_url(
            auth.client
                .thumbnail_url(&auth.login.sid, path, size.clamp(1, 640))?,
            QnapClient::auth_headers(),
            None,
        )
    }

    fn media_config(config: &MediaSourceConfig) -> Result<&QnapMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::Qnap(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected QNAP media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&QnapPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Qnap(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected QNAP playlist source_config".to_string(),
            )),
        }
    }

    fn source_server_id(source: SourceConfig<'_>) -> Result<&str, ProviderError> {
        match source {
            SourceConfig::Media(config) => Ok(&Self::media_config(config)?.server_id),
            SourceConfig::DynamicPlaylist(config) => Ok(&Self::playlist_config(config)?.server_id),
        }
    }
}

#[async_trait]
impl MediaProvider for QnapProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = Self::media_config(source_config)?;
        validate_file_path(&config.path)?;
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let auth = self
            .authenticated_with_repo(repo, owner, &config.server_id)
            .await?;
        let (parent, name) = split_path(&config.path)?;
        let listing = auth
            .client
            .list(&auth.login.sid, parent, 0, 10_000, None)
            .await?;
        let file = listing
            .datas
            .iter()
            .find(|file| file.filename == name && !file.is_dir())
            .ok_or(ProviderError::NotFound)?;
        let hardware = auth
            .client
            .hardware_transcode(&auth.login.sid)
            .await
            .unwrap_or(QnapHardwareTranscode {
                media_library_hardware: 0,
                qtranscode: 0,
                multimedia_codec: 0,
                hd_station_support: 0,
            });
        let ready = file.available_mp4_resolutions();
        let subtitles = listing
            .datas
            .iter()
            .filter(|candidate| is_related_subtitle(&file.filename, candidate))
            .take(RELATED_SUBTITLE_LIMIT)
            .map(|candidate| PlaybackSubtitle {
                name: candidate.filename.clone(),
                language: String::new(),
                format: candidate
                    .filename
                    .rsplit('.')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                provider: PlaybackSubtitleProvider::Qnap(PlaybackQnapSubtitle {
                    version: String::new(),
                    expires_at: 0,
                    mode_name: String::new(),
                    subtitle_index: 0,
                    path: join_path(parent, &candidate.filename),
                }),
            })
            .collect::<Vec<_>>();
        let mut playback_infos = HashMap::new();
        insert_mode(
            &mut playback_infos,
            "original",
            &file.filename,
            &config.path,
            QnapPlaybackMode::Original,
            owner,
            &config.server_id,
            subtitles.clone(),
        );
        for resolution in &ready {
            insert_mode(
                &mut playback_infos,
                &format!("transcoded_{}", resolution.label()),
                resolution.label(),
                &config.path,
                QnapPlaybackMode::PreTranscoded {
                    height: resolution_height(*resolution),
                },
                owner,
                &config.server_id,
                subtitles.clone(),
            );
        }
        let prefer_transcode = !ready.is_empty()
            && super::playback_profile_prefers_transcode(
                ctx.playback_client_profile(),
                detect_direct_url_format(&config.path),
            );
        if prefer_transcode {
            if let Some(info) = playback_infos.get_mut("transcoded") {
                info.default_media_index = info.medias.len().checked_sub(1);
            }
        }
        let capabilities = capabilities_from(&auth.login, &hardware);
        let result = PlaybackResult {
            playback_infos,
            default_mode: if prefer_transcode {
                "transcoded"
            } else {
                "original"
            }
            .to_string(),
            provider: Self::NAME.to_string(),
            provider_instance_name: auth.instance_name,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: Some(PlaybackMetadata::Qnap(QnapPlaybackMetadata {
                name: file.filename.clone(),
                path: config.path.clone(),
                size: file.filesize,
                modified_at: file.epochmt,
                file_type: file.filetype,
                realtime_transcode: false,
                hardware_transcode: capabilities.hardware_transcode,
                multimedia_codec: capabilities.multimedia_codec,
                pre_transcoded_heights: ready.into_iter().map(resolution_height).collect(),
                realtime_heights: Vec::new(),
            })),
        };
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!(
                "playback:{owner}:{}:room:{}:{}:profile:{}",
                config.server_id,
                ctx.room_id
                    .map_or_else(|| "none".to_string(), |room| room.to_string()),
                config.path,
                super::playback_profile_cache_token(ctx.playback_client_profile())
            ),
            PLAYBACK_CACHE_TTL,
            ctx,
            mark_qnap_playback_resources,
            || async { Ok(result) },
        )
        .await
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let server_id = Self::source_server_id(source_config)?;
        match source_config {
            SourceConfig::Media(config) => validate_file_path(&Self::media_config(config)?.path)?,
            SourceConfig::DynamicPlaylist(config) => {
                validate_path(&Self::playlist_config(config)?.path)?;
            }
        }
        let owner = *ctx.credential_owner_id().ok_or_else(|| {
            ProviderError::Internal("credential_owner_id is unavailable".to_string())
        })?;
        let exists = self
            .credential_repo_or(ctx.credential_repo)?
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .is_some();
        if !exists {
            return Err(ProviderError::CredentialNotFound(format!(
                "Referenced QNAP credential not found for server_id '{server_id}'"
            )));
        }
        Ok(())
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let (server_id, path, is_playlist) = match source_config {
            SourceConfig::Media(config) => {
                let config = Self::media_config(config)?;
                (config.server_id.clone(), config.path.clone(), false)
            }
            SourceConfig::DynamicPlaylist(config) => {
                let config = Self::playlist_config(config)?;
                (config.server_id.clone(), config.path.clone(), true)
            }
        };
        let path = if is_playlist {
            let repo = self.credential_repo_or(ctx.credential_repo)?;
            let mut folders = VecDeque::from([path]);
            let mut visited = 0usize;
            let mut cover = None;
            while let Some(folder) = folders.pop_front() {
                if visited >= 32 {
                    break;
                }
                visited += 1;
                let (listing, _) = self
                    .list_with_repo(repo, owner, &server_id, &folder, 1, 200, None)
                    .await?;
                if let Some(item) = listing
                    .content
                    .iter()
                    .find(|item| !item.is_dir && is_playable(&item.name))
                {
                    cover = Some(item.path.clone());
                    break;
                }
                folders.extend(
                    listing
                        .content
                        .into_iter()
                        .filter(|item| item.is_dir)
                        .map(|item| item.path),
                );
            }
            let Some(path) = cover else {
                return Ok(None);
            };
            path
        } else {
            path
        };
        Ok(Some(SourceCover::Qnap {
            server_id,
            credential_owner_id: owner,
            path,
        }))
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    fn credential_dependencies(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Vec<ProviderCredentialDependency>, ProviderError> {
        let owner = ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        Ok(vec![ProviderCredentialDependency::new(
            Self::NAME,
            owner.to_string(),
            Self::source_server_id(source_config)?.to_string(),
        )])
    }
}

#[async_trait]
impl DynamicPlaylistProvider for QnapProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let config =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let DynamicPagination::Page { page } = query.pagination else {
            return Err(ProviderError::InvalidConfig(
                "QNAP uses page pagination".to_string(),
            ));
        };
        let relative = decode_target(target)?;
        let path = relative
            .as_deref()
            .map_or_else(|| config.path.clone(), |path| join_path(&config.path, path));
        let (response, _) = self
            .list_with_repo(
                self.credential_repo_or(ctx.credential_repo)?,
                owner,
                &config.server_id,
                &path,
                page.max(1),
                query.page_size.max(1),
                query.search.as_deref(),
            )
            .await?;
        let items = response
            .content
            .into_iter()
            .filter_map(|item| {
                map_directory_item(&config.path, item, owner, &config.server_id).ok()
            })
            .collect();
        Ok(DynamicListResult {
            items,
            pagination: DynamicPagination::Page {
                page: response.page,
            },
            has_more: response.has_more,
        })
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let config =
            Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                ProviderError::InvalidConfig("Missing source_config".to_string())
            })?)?;
        let Some(relative) = decode_target(Some(target))? else {
            return Ok(None);
        };
        let path = join_path(&config.path, &relative);
        Ok(Some(NextPlayItem {
            name: path.rsplit('/').next().unwrap_or("QNAP media").to_string(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::Qnap(QnapMediaSourceConfig {
                server_id: config.server_id.clone(),
                path,
            }),
            target: target.clone(),
        }))
    }

    async fn next(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
        play_mode: PlayMode,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        if play_mode == PlayMode::RepeatOne {
            return self.resolve_item(ctx, playlist, target).await;
        }
        let relative = decode_target(Some(target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("QNAP playback target is required".to_string())
        })?;
        let parent = relative.rsplit_once('/').map(|(parent, _)| parent);
        let parent_target = parent
            .filter(|parent| !parent.is_empty())
            .map(|parent| ProviderTarget::qnap(parent.to_string()));
        let mut media = Vec::new();
        let mut page = 1;
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    parent_target.as_ref(),
                    DynamicListQuery {
                        pagination: DynamicPagination::Page { page },
                        page_size: 100,
                        ..DynamicListQuery::default()
                    },
                )
                .await?;
            media.extend(
                result
                    .items
                    .into_iter()
                    .filter(|item| item.item_type == ItemType::Media),
            );
            let enough = has_enough_media_for_next(&media, target, play_mode);
            if enough || !result.has_more {
                break;
            }
            page = page.saturating_add(1);
        }
        let selected = select_next_media(&media, target, play_mode);
        let Some(selected) = selected else {
            return Ok(None);
        };
        self.resolve_item(ctx, playlist, &selected.target).await
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(path) = decode_target(target)? else {
            return Ok(Vec::new());
        };
        let mut current = String::new();
        Ok(path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| {
                current = join_path(&current, part);
                DynamicBrowsePathSegment {
                    name: part.to_string(),
                    target: ProviderTarget::qnap(current.clone()),
                }
            })
            .collect())
    }
}

fn has_enough_media_for_next(
    media: &[DirectoryItem],
    target: &ProviderTarget,
    play_mode: PlayMode,
) -> bool {
    match play_mode {
        PlayMode::Sequential | PlayMode::RepeatAll => media
            .iter()
            .position(|item| &item.target == target)
            .is_some_and(|index| media.get(index + 1).is_some()),
        PlayMode::Shuffle => media.len() >= DYNAMIC_MAX_SHUFFLE_ITEMS,
        PlayMode::RepeatOne => true,
    }
}

fn select_next_media<'a>(
    media: &'a [DirectoryItem],
    target: &ProviderTarget,
    play_mode: PlayMode,
) -> Option<&'a DirectoryItem> {
    match play_mode {
        PlayMode::Sequential => media
            .iter()
            .position(|item| &item.target == target)
            .and_then(|index| media.get(index + 1)),
        PlayMode::RepeatAll => media
            .iter()
            .position(|item| &item.target == target)
            .and_then(|index| media.get(index + 1))
            .or_else(|| media.first()),
        PlayMode::Shuffle => media.choose(&mut rand::rng()),
        PlayMode::RepeatOne => media.iter().find(|item| &item.target == target),
    }
}

const QNAP_RESOLUTIONS: [QnapTranscodeResolution; 5] = [
    QnapTranscodeResolution::P240,
    QnapTranscodeResolution::P360,
    QnapTranscodeResolution::P480,
    QnapTranscodeResolution::P720,
    QnapTranscodeResolution::P1080,
];

fn capabilities_from(login: &QnapLogin, hardware: &QnapHardwareTranscode) -> QnapCapabilities {
    QnapCapabilities {
        support_rtt: login.support_rtt != 0,
        hardware_transcode: hardware.media_library_hardware != 0,
        qtranscode: hardware.qtranscode != 0,
        multimedia_codec: hardware.multimedia_codec != 0,
        hd_station_support: hardware.hd_station_support != 0,
    }
}

fn resolution_height(resolution: QnapTranscodeResolution) -> u32 {
    match resolution {
        QnapTranscodeResolution::P240 => 240,
        QnapTranscodeResolution::P360 => 360,
        QnapTranscodeResolution::P480 => 480,
        QnapTranscodeResolution::P720 => 720,
        QnapTranscodeResolution::P1080 => 1080,
    }
}

fn resolution_from_height(height: u32) -> Result<QnapTranscodeResolution, ProviderError> {
    QNAP_RESOLUTIONS
        .into_iter()
        .find(|resolution| resolution_height(*resolution) == height)
        .ok_or_else(|| ProviderError::InvalidConfig(format!("Unsupported QNAP height {height}")))
}

fn playback_url(
    client: &QnapClient,
    sid: &str,
    resource: &QnapPlaybackResource,
) -> Result<String, ProviderError> {
    match resource.mode {
        QnapPlaybackMode::Original => client.download_url(sid, &resource.path).map_err(Into::into),
        QnapPlaybackMode::PreTranscoded { height } => client
            .viewer_url(
                sid,
                &resource.path,
                Some(resolution_from_height(height)?),
                false,
                None,
            )
            .map_err(Into::into),
    }
}

#[allow(clippy::too_many_arguments)]
fn insert_mode(
    infos: &mut HashMap<String, PlaybackInfo>,
    mode_name: &str,
    media_name: &str,
    path: &str,
    mode: QnapPlaybackMode,
    owner: UserId,
    server_id: &str,
    subtitles: Vec<PlaybackSubtitle>,
) {
    let route_name = if mode_name.starts_with("transcoded_") {
        "transcoded"
    } else {
        mode_name
    };
    let info = infos
        .entry(route_name.to_string())
        .or_insert_with(|| PlaybackInfo {
            thumbnail: Some(path.to_string()),
            medias: Vec::new(),
            default_media_index: Some(0),
            subtitles,
            default_subtitle_index: None,
            danmakus: Vec::new(),
            default_danmaku_index: None,
        });
    info.medias.push(PlaybackMedia {
        name: media_name.to_string(),
        format: match mode {
            QnapPlaybackMode::Original => detect_direct_url_format(path).to_string(),
            QnapPlaybackMode::PreTranscoded { .. } => "mp4".to_string(),
        },
        expire_at: None,
        metadata: None,
        provider: PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Refresh {
            credential_owner_id: owner.to_string(),
            server_id: server_id.to_string(),
            resource: QnapPlaybackResource {
                path: path.to_string(),
                mode,
            },
        }),
    });
}

fn mark_qnap_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    for (mode_name, info) in &mut result.playback_infos {
        for (media_index, media) in info.medias.iter_mut().enumerate() {
            if let PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Refresh {
                credential_owner_id,
                server_id,
                resource,
            }) = &media.provider
            {
                media.provider = PlaybackMediaProvider::Qnap(PlaybackQnapMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                    credential_owner_id: credential_owner_id.clone(),
                    server_id: server_id.clone(),
                    resource: resource.clone(),
                });
            }
        }
        for (subtitle_index, subtitle) in info.subtitles.iter_mut().enumerate() {
            if let PlaybackSubtitleProvider::Qnap(resource) = &mut subtitle.provider {
                resource.version = version.to_string();
                resource.expires_at = expires_at;
                resource.mode_name.clone_from(mode_name);
                resource.subtitle_index = subtitle_index;
            }
        }
    }
}

fn validate_path(path: &str) -> Result<(), ProviderError> {
    if path.split('/').any(|segment| segment == "..") {
        return Err(ProviderError::InvalidConfig(
            "QNAP path must not contain traversal".to_string(),
        ));
    }
    Ok(())
}

fn validate_file_path(path: &str) -> Result<(), ProviderError> {
    validate_path(path)?;
    if path.trim_matches('/').is_empty() {
        return Err(ProviderError::InvalidConfig(
            "QNAP media path must identify a file".to_string(),
        ));
    }
    Ok(())
}

fn split_path(path: &str) -> Result<(&str, &str), ProviderError> {
    validate_file_path(path)?;
    let (parent, name) = path.rsplit_once('/').unwrap_or(("/", path));
    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

fn join_path(base: &str, relative: &str) -> String {
    let base = base.trim_end_matches('/');
    let relative = relative.trim_start_matches('/');
    if base.is_empty() {
        format!("/{relative}")
    } else if relative.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{relative}")
    }
}

fn relative_path(base: &str, full: &str) -> Option<String> {
    let base = base.trim_end_matches('/');
    if base.is_empty() || base == "/" {
        return Some(full.trim_start_matches('/').to_string());
    }
    full.strip_prefix(base)?
        .strip_prefix('/')
        .map(str::to_string)
}

fn map_list_item(parent: &str, file: QnapFile) -> QnapListItem {
    let path = join_path(parent, &file.filename);
    let is_dir = file.is_dir();
    let pre_transcoded_heights = file
        .available_mp4_resolutions()
        .into_iter()
        .map(resolution_height)
        .collect();
    QnapListItem {
        name: file.filename,
        path,
        is_dir,
        size: file.filesize,
        modified_at: file.epochmt,
        file_type: file.filetype,
        pre_transcoded_heights,
    }
}

fn is_playable(name: &str) -> bool {
    matches!(
        name.rsplit('.')
            .next()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some(
            "mp4"
                | "mkv"
                | "avi"
                | "mov"
                | "flv"
                | "webm"
                | "m4v"
                | "wmv"
                | "m3u8"
                | "mp3"
                | "flac"
                | "wav"
                | "aac"
                | "m4a"
                | "ogg"
        )
    )
}

fn is_related_subtitle(video_name: &str, file: &QnapFile) -> bool {
    if file.is_dir() {
        return false;
    }
    let Some((video_stem, _)) = video_name.rsplit_once('.') else {
        return false;
    };
    let Some((subtitle_stem, extension)) = file.filename.rsplit_once('.') else {
        return false;
    };
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "srt" | "vtt" | "ass" | "ssa"
    ) && (subtitle_stem == video_stem
        || subtitle_stem
            .strip_prefix(video_stem)
            .is_some_and(|suffix| suffix.starts_with('.') || suffix.starts_with('_')))
}

fn map_directory_item(
    base: &str,
    item: QnapListItem,
    owner: UserId,
    server_id: &str,
) -> Result<DirectoryItem, ProviderError> {
    let item_type = if item.is_dir {
        ItemType::Playlist
    } else if is_playable(&item.name) {
        ItemType::Media
    } else {
        return Err(ProviderError::NotFound);
    };
    let relative = relative_path(base, &item.path).ok_or_else(|| {
        ProviderError::ApiError(format!(
            "QNAP item path '{}' is outside playlist path '{base}'",
            item.path
        ))
    })?;
    Ok(DirectoryItem {
        name: item.name,
        item_type,
        target: ProviderTarget::qnap(relative),
        size: (!item.is_dir).then_some(item.size),
        thumbnail: (!item.is_dir).then(|| DirectoryItemThumbnail::Qnap {
            server_id: server_id.to_string(),
            credential_owner_id: owner,
            path: item.path,
        }),
        description: None,
        modified_at: i64::try_from(item.modified_at)
            .ok()
            .filter(|modified_at| *modified_at > 0),
        source_config: None,
    })
}

fn decode_target(target: Option<&ProviderTarget>) -> Result<Option<String>, ProviderError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let ProviderTarget::Qnap(target) = target else {
        return Err(ProviderError::InvalidConfig(
            "QNAP target must use qnap payload".to_string(),
        ));
    };
    validate_path(&target.relative_path)?;
    Ok(Some(target.relative_path.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_transcoded_resolutions_share_one_mode() {
        let owner = UserId::new();
        let mut infos = HashMap::new();
        insert_mode(
            &mut infos,
            "transcoded_720p",
            "720p",
            "/Movies/Movie.mkv",
            QnapPlaybackMode::PreTranscoded { height: 720 },
            owner,
            "server",
            Vec::new(),
        );
        insert_mode(
            &mut infos,
            "transcoded_1080p",
            "1080p",
            "/Movies/Movie.mkv",
            QnapPlaybackMode::PreTranscoded { height: 1080 },
            owner,
            "server",
            Vec::new(),
        );

        assert_eq!(infos.len(), 1);
        assert_eq!(infos["transcoded"].medias.len(), 2);
        assert_eq!(infos["transcoded"].medias[0].name, "720p");
        assert_eq!(infos["transcoded"].medias[1].name, "1080p");
    }

    #[test]
    fn related_subtitles_accept_language_suffixes() {
        let subtitle = QnapFile {
            filename: "Movie.zh-CN.ass".to_string(),
            isfolder: 0,
            filesize: 1,
            epochmt: 0,
            filetype: 0,
            mp4_240: 0,
            mp4_360: 0,
            mp4_480: 0,
            mp4_720: 0,
            mp4_1080: 0,
            transcode_queued: 0,
            play: 0,
        };
        assert!(is_related_subtitle("Movie.mkv", &subtitle));
    }

    #[test]
    fn target_paths_remain_relative_to_playlist_root() {
        let item = QnapListItem {
            name: "Movie.mkv".to_string(),
            path: "/Multimedia/Films/Movie.mkv".to_string(),
            is_dir: false,
            size: 1,
            modified_at: 0,
            file_type: 2,
            pre_transcoded_heights: vec![720],
        };
        let mapped = map_directory_item("/Multimedia", item, UserId::new(), "server")
            .expect("item should map");
        assert_eq!(
            mapped.target,
            ProviderTarget::qnap("Films/Movie.mkv".to_string())
        );
    }

    #[test]
    fn sequential_autoplay_scans_past_two_hundred_items() {
        let media = (1..=250)
            .map(|index| DirectoryItem {
                name: format!("Movie {index}"),
                item_type: ItemType::Media,
                target: ProviderTarget::qnap(format!("Movie-{index}.mkv")),
                size: Some(1),
                thumbnail: None,
                description: None,
                modified_at: None,
                source_config: None,
            })
            .collect::<Vec<_>>();
        let item_200 = ProviderTarget::qnap("Movie-200.mkv".to_string());
        let item_250 = ProviderTarget::qnap("Movie-250.mkv".to_string());

        assert!(!has_enough_media_for_next(
            &media[..200],
            &item_200,
            PlayMode::Sequential
        ));
        assert!(has_enough_media_for_next(
            &media[..201],
            &item_200,
            PlayMode::Sequential
        ));
        assert_eq!(
            select_next_media(&media, &item_200, PlayMode::Sequential)
                .expect("item 200 should advance")
                .target,
            ProviderTarget::qnap("Movie-201.mkv".to_string())
        );
        assert_eq!(
            select_next_media(&media, &item_250, PlayMode::RepeatAll)
                .expect("repeat all should wrap")
                .target,
            ProviderTarget::qnap("Movie-1.mkv".to_string())
        );
        assert!(has_enough_media_for_next(
            &media[..DYNAMIC_MAX_SHUFFLE_ITEMS],
            &item_250,
            PlayMode::Shuffle
        ));
    }
}
