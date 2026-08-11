//! TrueNAS filesystem media provider.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use rand::seq::IndexedRandom;
use sha2::{Digest, Sha256};

use super::{
    DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPagination,
    DynamicPlaylistItem, DynamicPlaylistProvider, ItemType, MediaProvider, NextPlayItem,
    PlaybackInfo, PlaybackResult, ProviderContext, ProviderCredentialDependency, ProviderError,
    SourceConfig, SourceCover,
};
use crate::models::{
    detect_direct_url_format, normalize_provider_instance_name,
    normalize_provider_instance_name_owned, MediaSourceConfig, PlayMode, PlaybackMedia,
    PlaybackMediaProvider, PlaybackMetadata, PlaybackSubtitle, PlaybackSubtitleProvider,
    PlaybackTrueNasMedia, PlaybackTrueNasSubtitle, PlaylistSourceConfig, ProviderCredential,
    ProviderTarget, TrueNasMediaSourceConfig, TrueNasPlaybackMetadata, TrueNasPlaylistSource,
    TrueNasPlaylistSourceConfig, UserId, UserProviderCredential,
};
use crate::repository::UserProviderCredentialRepository;
use synctv_media_providers::truenas::{TrueNasClient, TrueNasFileItem, TrueNasList};

const PLAYBACK_CACHE_TTL: Duration = Duration::from_hours(2);
const SHUFFLE_LIMIT: usize = 200;
const RELATED_SUBTITLE_LIMIT: usize = 32;

#[derive(Debug, Clone)]
pub struct TrueNasBind {
    pub id: i64,
    pub server_id: String,
    pub endpoint: String,
    pub hostname: String,
    pub version: String,
    pub system_product: String,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct TrueNasHlsResourceRequest<'a> {
    pub version: &'a str,
    pub mode_name: &'a str,
    pub media_index: usize,
    pub target_url: &'a str,
    pub is_manifest: bool,
    pub range_header: Option<&'a str>,
}

#[derive(Debug, Clone)]
pub struct TrueNasListResponse {
    pub content: Vec<TrueNasFileItem>,
    pub total: u64,
    pub page: usize,
    pub has_more: bool,
}

#[derive(Clone)]
struct AuthenticatedTrueNas {
    client: TrueNasClient,
    api_key: String,
    instance_name: Option<String>,
}

pub struct TrueNasProvider {
    http_client: reqwest::Client,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

impl TrueNasProvider {
    pub const NAME: &'static str = "truenas";

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
            ProviderError::Internal("TrueNAS credential repository is unavailable".to_string())
        })
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Result<&'a UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().or(fallback).ok_or_else(|| {
            ProviderError::Internal("TrueNAS credential repository is unavailable".to_string())
        })
    }

    fn client(&self, endpoint: &str) -> Result<TrueNasClient, ProviderError> {
        TrueNasClient::with_http_client(endpoint, self.http_client.clone()).map_err(Into::into)
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
        owner: UserId,
        endpoint: String,
        api_key: String,
        provider_instance_name: Option<String>,
    ) -> Result<(String, synctv_media_providers::truenas::TrueNasSystemInfo), ProviderError> {
        if api_key.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS API key is required".to_string(),
            ));
        }
        let client = self.client(&endpoint)?;
        let info = client.system_info(api_key.trim()).await?;
        let provider_instance_name = normalize_provider_instance_name_owned(provider_instance_name);
        let server_id =
            Self::credential_server_id_for_instance(&endpoint, provider_instance_name.as_deref());
        let now = Utc::now();
        self.credential_repo()?
            .upsert_by_user_provider_server(&UserProviderCredential {
                id: 0,
                user_id: owner,
                provider: Self::NAME.to_string(),
                server_id: server_id.clone(),
                provider_instance_name,
                credential_data: ProviderCredential::TrueNas {
                    endpoint,
                    api_key: api_key.trim().to_string(),
                    hostname: info.hostname.clone(),
                    version: info.version.clone(),
                    system_product: info.system_product.clone().unwrap_or_default(),
                },
                expires_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok((server_id, info))
    }

    async fn authenticated_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        owner: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedTrueNas, ProviderError> {
        let credential = repo
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let ProviderCredential::TrueNas {
            endpoint, api_key, ..
        } = credential.credential_data
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        Ok(AuthenticatedTrueNas {
            client: self.client(&endpoint)?,
            api_key,
            instance_name: credential.provider_instance_name,
        })
    }

    async fn authenticated(
        &self,
        owner: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedTrueNas, ProviderError> {
        self.authenticated_with_repo(self.credential_repo()?, owner, server_id)
            .await
    }

    pub async fn list(
        &self,
        owner: UserId,
        server_id: &str,
        path: &str,
        page: usize,
        page_size: usize,
        search: Option<&str>,
    ) -> Result<(TrueNasListResponse, Option<String>), ProviderError> {
        validate_path(path)?;
        let auth = self.authenticated(owner, server_id).await?;
        let list = auth
            .client
            .list(
                &auth.api_key,
                path,
                page as u64,
                u32::try_from(page_size).unwrap_or(u32::MAX),
                search,
            )
            .await?;
        Ok((map_list_response(list), auth.instance_name))
    }

    pub async fn list_binds(
        &self,
        owner: UserId,
        provider_instance_name: Option<&str>,
    ) -> Result<Vec<TrueNasBind>, ProviderError> {
        let requested = normalize_provider_instance_name(provider_instance_name);
        self.credential_repo()?
            .get_readable_by_provider(owner, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .filter(|credential| {
                requested
                    .is_none_or(|name| credential.provider_instance_name.as_deref() == Some(name))
            })
            .map(|credential| {
                let ProviderCredential::TrueNas {
                    endpoint,
                    hostname,
                    version,
                    system_product,
                    ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(TrueNasBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    endpoint,
                    hostname,
                    version,
                    system_product,
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub async fn delete_credential(
        &self,
        owner: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let Some(credential) = self
            .credential_repo()?
            .get_by_provider_and_server(owner, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
        else {
            return Ok(false);
        };
        self.credential_repo()?
            .delete(credential.id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(true)
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
        let PlaybackMediaProvider::TrueNas(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS cached playback resource is invalid".to_string(),
            ));
        };
        let (credential_owner_id, server_id, path) = resource_descriptor(provider)?;
        let owner = credential_owner_id
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        let ticket = auth.client.download_ticket(&auth.api_key, path).await?;
        super::playback_transport::transport_action_for_target_url(
            ticket.url.to_string(),
            HashMap::new(),
            range_header,
        )
    }

    pub async fn get_hls_manifest(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        mode_name: &str,
        media_index: usize,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        if !super::playback_media_is_hls(mode_name, media) {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS HLS manifest request references a non-HLS media resource".to_string(),
            ));
        }
        let PlaybackMediaProvider::TrueNas(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS cached HLS resource is invalid".to_string(),
            ));
        };
        let (owner, server_id, path) = resource_descriptor(provider)?;
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        let ticket = auth.client.download_ticket(&auth.api_key, path).await?;
        super::playback_transport::transport_action_for_storage_hls_target(
            ticket.url.to_string(),
            HashMap::new(),
            path,
            true,
            None,
        )
    }

    pub async fn get_hls_resource(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        request: TrueNasHlsResourceRequest<'_>,
        request_context: Option<&super::ExecutionControl>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, request.version, request_context)
                .await?;
        let media = versioned
            .result
            .playback_infos
            .get(request.mode_name)
            .and_then(|info| info.medias.get(request.media_index))
            .ok_or(ProviderError::NotFound)?;
        if !super::playback_media_is_hls(request.mode_name, media) {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS HLS child request references a non-HLS media resource".to_string(),
            ));
        }
        let PlaybackMediaProvider::TrueNas(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS cached HLS resource is invalid".to_string(),
            ));
        };
        let (owner, server_id, root_path) = resource_descriptor(provider)?;
        let path =
            super::playback_transport::storage_hls_resource_path(root_path, request.target_url)?;
        validate_file_path(&path)?;
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        let ticket = auth.client.download_ticket(&auth.api_key, &path).await?;
        super::playback_transport::transport_action_for_storage_hls_target(
            ticket.url.to_string(),
            HashMap::new(),
            &path,
            request.is_manifest,
            request.range_header,
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
        let info = versioned
            .result
            .playback_infos
            .get(mode_name)
            .ok_or(ProviderError::NotFound)?;
        let subtitle = info
            .subtitles
            .get(subtitle_index)
            .ok_or(ProviderError::NotFound)?;
        let PlaybackSubtitleProvider::TrueNas(subtitle) = &subtitle.provider else {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS cached subtitle resource is invalid".to_string(),
            ));
        };
        let media = info.medias.first().ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::TrueNas(provider) = &media.provider else {
            return Err(ProviderError::NotFound);
        };
        let (owner, server_id, _) = resource_descriptor(provider)?;
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        let ticket = auth
            .client
            .download_ticket(&auth.api_key, &subtitle.path)
            .await?;
        super::playback_transport::transport_action_for_target_url(
            ticket.url.to_string(),
            HashMap::new(),
            None,
        )
    }

    fn media_config(
        config: &MediaSourceConfig,
    ) -> Result<&TrueNasMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::TrueNas(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected TrueNAS media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&TrueNasPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::TrueNas(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected TrueNAS playlist source_config".to_string(),
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
impl MediaProvider for TrueNasProvider {
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
        let auth = self
            .authenticated_with_repo(
                self.credential_repo_or(ctx.credential_repo)?,
                owner,
                &config.server_id,
            )
            .await?;
        let stat = auth.client.stat(&auth.api_key, &config.path).await?;
        if stat.kind != "FILE" {
            return Err(ProviderError::InvalidConfig(
                "TrueNAS media path must identify a regular file".to_string(),
            ));
        }
        let name = stat
            .realpath
            .rsplit('/')
            .next()
            .unwrap_or("TrueNAS media")
            .to_string();
        let media_swarm_descriptor = format!(
            "server:{}:path:{}:device:{}:inode:{}:size:{}:mtime:{}",
            config.server_id,
            config.path,
            stat.dev,
            stat.inode,
            stat.size,
            stat.mtime.to_bits()
        );
        let metadata = TrueNasPlaybackMetadata {
            realpath: stat.realpath,
            size: stat.size,
            allocation_size: stat.allocation_size,
            mode: stat.mode,
            mount_id: stat.mount_id,
            uid: stat.uid,
            gid: stat.gid,
            atime: stat.atime,
            mtime: stat.mtime,
            ctime: stat.ctime,
            btime: stat.btime,
            dev: stat.dev,
            inode: stat.inode,
            nlink: stat.nlink,
            acl: stat.acl,
            is_mountpoint: stat.is_mountpoint,
            is_ctldir: stat.is_ctldir,
            attributes: stat.attributes,
            user: stat.user,
            group: stat.group,
        };
        let subtitles = discover_subtitles(&auth, &config.server_id, &config.path).await?;
        let mut playback_infos = HashMap::new();
        playback_infos.insert(
            "original".to_string(),
            PlaybackInfo {
                thumbnail: None,
                medias: vec![PlaybackMedia {
                    name,
                    format: detect_direct_url_format(&config.path).to_string(),
                    expire_at: None,
                    metadata: None,
                    p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                        Self::NAME,
                        auth.instance_name.as_deref(),
                        "media",
                        &media_swarm_descriptor,
                    )),
                    provider: PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Refresh {
                        credential_owner_id: owner.to_string(),
                        server_id: config.server_id.clone(),
                        path: config.path.clone(),
                    }),
                }],
                default_media_index: Some(0),
                subtitles,
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            },
        );
        let result = PlaybackResult {
            playback_infos,
            default_mode: "original".to_string(),
            provider: Self::NAME.to_string(),
            provider_instance_name: auth.instance_name,
            duration_seconds: None,
            playback_kind: Some(crate::models::PlaybackKind::Regular),
            metadata: Some(PlaybackMetadata::TrueNas(metadata)),
        };
        let mut result = result;
        if config.proxy_mode == crate::models::PlaybackProxyMode::Prefer {
            let ticket = auth
                .client
                .download_ticket(&auth.api_key, &config.path)
                .await?;
            let direct = result
                .playback_infos
                .get("original")
                .cloned()
                .ok_or_else(|| {
                    ProviderError::Internal("TrueNAS playback mode missing".to_string())
                })?
                .medias
                .into_iter()
                .map(|mut media| {
                    media.provider = PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Direct {
                        url: ticket.url.to_string(),
                        headers: HashMap::new(),
                    });
                    media
                })
                .collect::<Vec<_>>();
            if let Some(info) = result.playback_infos.get("original").cloned() {
                result.playback_infos.insert(
                    "direct".to_string(),
                    PlaybackInfo {
                        medias: direct,
                        ..info
                    },
                );
            }
        }
        super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!(
                "playback:{owner}:{}:room:{}:{}:proxy:{}",
                config.server_id,
                ctx.room_id
                    .map_or_else(|| "none".to_string(), |room| room.to_string()),
                config.path,
                config.proxy_mode.as_str()
            ),
            PLAYBACK_CACHE_TTL,
            ctx,
            |result, version, expires_at| {
                mark_playback_resources(result, version, expires_at);
                super::apply_provider_playback_policy(result, config.proxy_mode, true);
            },
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
                validate_playlist_source(&Self::playlist_config(config)?.source)?;
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
                "Referenced TrueNAS credential not found for server_id '{server_id}'"
            )));
        }
        Ok(())
    }

    async fn source_cover(
        &self,
        _ctx: &ProviderContext<'_>,
        _source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        Ok(None)
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
impl DynamicPlaylistProvider for TrueNasProvider {
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
                "TrueNAS uses page pagination".to_string(),
            ));
        };
        let auth = self
            .authenticated_with_repo(
                self.credential_repo_or(ctx.credential_repo)?,
                owner,
                &config.server_id,
            )
            .await?;
        let target_path = decode_target(target)?.map(|target| target.path);
        let (path, search) = if let Some(path) = target_path.as_deref() {
            (path, query.search.as_deref())
        } else {
            match &config.source {
                TrueNasPlaylistSource::Folder { path } => (path.as_str(), query.search.as_deref()),
                TrueNasPlaylistSource::Search { path, query } => {
                    (path.as_str(), Some(query.as_str()))
                }
            }
        };
        let page = page.max(1);
        let list = auth
            .client
            .list(
                &auth.api_key,
                path,
                page as u64,
                u32::try_from(query.page_size.clamp(1, 200)).unwrap_or(200),
                search,
            )
            .await?;
        let has_more = list.page.saturating_mul(u64::from(list.page_size)) < list.total;
        Ok(DynamicListResult {
            items: list
                .items
                .into_iter()
                .filter_map(map_directory_item)
                .collect(),
            pagination: DynamicPagination::Page { page },
            has_more,
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
        let target = decode_target(Some(target))?.ok_or_else(|| {
            ProviderError::InvalidConfig("TrueNAS target is required".to_string())
        })?;
        validate_file_path(&target.path)?;
        Ok(Some(NextPlayItem {
            name: target
                .path
                .rsplit('/')
                .next()
                .unwrap_or("TrueNAS media")
                .to_string(),
            item_type: ItemType::Media,
            source_config: MediaSourceConfig::TrueNas(TrueNasMediaSourceConfig {
                server_id: config.server_id.clone(),
                path: target.path.clone(),
                proxy_mode: config.proxy_mode,
            }),
            target: ProviderTarget::truenas(target.path),
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
        let mut media = Vec::new();
        let mut page = 1;
        loop {
            let result = self
                .list_playlist(
                    ctx,
                    playlist,
                    None,
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
            if enough_for_next(&media, target, play_mode) || !result.has_more {
                break;
            }
            page = page.saturating_add(1);
        }
        let selected = match play_mode {
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
        };
        match selected {
            Some(item) => self.resolve_item(ctx, playlist, &item.target).await,
            None => Ok(None),
        }
    }

    async fn browse_path(
        &self,
        _ctx: &ProviderContext<'_>,
        _playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
    ) -> Result<Vec<DynamicBrowsePathSegment>, ProviderError> {
        let Some(target) = decode_target(target)? else {
            return Ok(Vec::new());
        };
        let mut current = String::new();
        Ok(target
            .path
            .split('/')
            .filter(|part| !part.is_empty())
            .map(|part| {
                current.push('/');
                current.push_str(part);
                DynamicBrowsePathSegment {
                    name: part.to_string(),
                    target: ProviderTarget::truenas(current.clone()),
                }
            })
            .collect())
    }
}

fn map_list_response(response: TrueNasList) -> TrueNasListResponse {
    let has_more = response.page.saturating_mul(u64::from(response.page_size)) < response.total;
    TrueNasListResponse {
        content: response.items,
        total: response.total,
        page: usize::try_from(response.page).unwrap_or(usize::MAX),
        has_more,
    }
}

fn map_directory_item(item: TrueNasFileItem) -> Option<DynamicPlaylistItem> {
    let is_directory = item.is_directory();
    let item_type = if is_directory {
        ItemType::Playlist
    } else if is_playable(&item.name) {
        ItemType::Media
    } else {
        return None;
    };
    let description = item
        .zfs_attrs
        .as_deref()
        .into_iter()
        .flatten()
        .chain(item.attributes.iter())
        .next()
        .cloned();
    Some(DynamicPlaylistItem {
        name: item.name,
        item_type,
        target: ProviderTarget::truenas(item.path),
        size: (!is_directory).then_some(item.size),
        thumbnail: None,
        description,
        modified_at: None,
        source_config: None,
        metadata: None,
    })
}

fn mark_playback_resources(result: &mut PlaybackResult, version: &str, expires_at: i64) {
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(name, info)| (name.clone(), info.clone()))
        .collect::<Vec<_>>();
    for (mode_name, original_info) in original_modes {
        if mode_name.starts_with("proxy_") {
            continue;
        }
        let mut proxy_info = original_info.clone();
        proxy_info.medias = original_info
            .medias
            .iter()
            .enumerate()
            .filter_map(|(media_index, media)| {
                let PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Refresh {
                    credential_owner_id,
                    server_id,
                    path,
                }) = &media.provider
                else {
                    return None;
                };
                let mut proxy = media.clone();
                proxy.provider = PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Proxy {
                    version: version.to_string(),
                    expires_at,
                    mode_name: mode_name.clone(),
                    media_index,
                    credential_owner_id: credential_owner_id.clone(),
                    server_id: server_id.clone(),
                    path: path.clone(),
                });
                Some(proxy)
            })
            .collect();
        if proxy_info.medias.is_empty() {
            continue;
        }
        for (subtitle_index, subtitle) in proxy_info.subtitles.iter_mut().enumerate() {
            if let PlaybackSubtitleProvider::TrueNas(resource) = &mut subtitle.provider {
                resource.version = version.to_string();
                resource.expires_at = expires_at;
                resource.mode_name.clone_from(&mode_name);
                resource.subtitle_index = subtitle_index;
            }
        }
        result
            .playback_infos
            .insert(format!("proxy_{mode_name}"), proxy_info);
        if original_info.medias.iter().all(|media| {
            matches!(
                media.provider,
                PlaybackMediaProvider::TrueNas(PlaybackTrueNasMedia::Refresh { .. })
            )
        }) {
            result.playback_infos.remove(&mode_name);
        }
    }
}

async fn discover_subtitles(
    auth: &AuthenticatedTrueNas,
    server_id: &str,
    media_path: &str,
) -> Result<Vec<PlaybackSubtitle>, ProviderError> {
    let parent = parent_path(media_path);
    Ok(auth
        .client
        .list_all(&auth.api_key, &parent, None)
        .await?
        .into_iter()
        .filter(|item| related_subtitle(media_path, item))
        .take(RELATED_SUBTITLE_LIMIT)
        .map(|item| PlaybackSubtitle {
            name: item.name.clone(),
            language: subtitle_language(media_path, &item.name),
            format: subtitle_format(&item.name).unwrap_or_default().to_string(),
            p2p_swarm_id: Some(super::provider_p2p_swarm_id(
                TrueNasProvider::NAME,
                auth.instance_name.as_deref(),
                "subtitle",
                &format!(
                    "server:{server_id}:path:{}:size:{}:allocation:{}",
                    item.path, item.size, item.allocation_size
                ),
            )),
            provider: PlaybackSubtitleProvider::TrueNas(PlaybackTrueNasSubtitle {
                version: String::new(),
                expires_at: 0,
                mode_name: String::new(),
                subtitle_index: 0,
                path: item.path,
            }),
        })
        .collect())
}

fn related_subtitle(media_path: &str, item: &TrueNasFileItem) -> bool {
    !item.is_directory()
        && subtitle_format(&item.name).is_some()
        && related_stem(
            media_path.rsplit('/').next().unwrap_or_default(),
            &item.name,
        )
}

fn related_stem(media_name: &str, subtitle_name: &str) -> bool {
    let media_stem = media_name
        .rsplit_once('.')
        .map_or(media_name, |(stem, _)| stem)
        .to_ascii_lowercase();
    let subtitle_stem = subtitle_name
        .rsplit_once('.')
        .map_or(subtitle_name, |(stem, _)| stem)
        .to_ascii_lowercase();
    subtitle_stem == media_stem
        || subtitle_stem
            .strip_prefix(&media_stem)
            .is_some_and(|suffix| suffix.starts_with(['.', '_', '-']))
}

fn subtitle_format(name: &str) -> Option<&str> {
    let extension = name.rsplit_once('.')?.1;
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "srt" | "vtt" | "ass" | "ssa" | "sub" | "ttml"
    )
    .then_some(extension)
}

fn subtitle_language(media_path: &str, subtitle_name: &str) -> String {
    let media_name = media_path.rsplit('/').next().unwrap_or_default();
    let media_stem = media_name
        .rsplit_once('.')
        .map_or(media_name, |(stem, _)| stem);
    let subtitle_stem = subtitle_name
        .rsplit_once('.')
        .map_or(subtitle_name, |(stem, _)| stem);
    subtitle_stem
        .strip_prefix(media_stem)
        .unwrap_or_default()
        .trim_start_matches(['.', '_', '-'])
        .to_string()
}

fn parent_path(path: &str) -> String {
    path.rsplit_once('/').map_or_else(
        || "/mnt".to_string(),
        |(parent, _)| {
            if parent.is_empty() {
                "/mnt".to_string()
            } else {
                parent.to_string()
            }
        },
    )
}

fn decode_target(
    target: Option<&ProviderTarget>,
) -> Result<Option<crate::models::TrueNasTarget>, ProviderError> {
    let Some(target) = target else {
        return Ok(None);
    };
    let ProviderTarget::TrueNas(target) = target else {
        return Err(ProviderError::InvalidConfig(
            "TrueNAS target must use truenas payload".to_string(),
        ));
    };
    validate_path(&target.path)?;
    Ok(Some(target.clone()))
}

fn validate_playlist_source(source: &TrueNasPlaylistSource) -> Result<(), ProviderError> {
    match source {
        TrueNasPlaylistSource::Folder { path } => validate_path(path),
        TrueNasPlaylistSource::Search { path, query } => {
            validate_path(path)?;
            if query.trim().is_empty() {
                return Err(ProviderError::InvalidConfig(
                    "TrueNAS search query is required".to_string(),
                ));
            }
            Ok(())
        }
    }
}

fn validate_path(path: &str) -> Result<(), ProviderError> {
    let path = path.trim();
    if path.is_empty() || path == "/mnt" {
        return Ok(());
    }
    if !path.starts_with("/mnt/") || path.split('/').any(|segment| segment == "..") {
        return Err(ProviderError::InvalidConfig(
            "TrueNAS path must remain under /mnt".to_string(),
        ));
    }
    Ok(())
}

fn validate_file_path(path: &str) -> Result<(), ProviderError> {
    validate_path(path)?;
    if path.trim().is_empty() || path.trim_end_matches('/') == "/mnt" || path.ends_with('/') {
        return Err(ProviderError::InvalidConfig(
            "TrueNAS media path must identify a file".to_string(),
        ));
    }
    Ok(())
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

fn enough_for_next(
    media: &[DynamicPlaylistItem],
    target: &ProviderTarget,
    play_mode: PlayMode,
) -> bool {
    match play_mode {
        PlayMode::Sequential | PlayMode::RepeatAll => media
            .iter()
            .position(|item| &item.target == target)
            .is_some_and(|index| media.get(index + 1).is_some()),
        PlayMode::Shuffle => media.len() >= SHUFFLE_LIMIT,
        PlayMode::RepeatOne => true,
    }
}

fn resource_descriptor(
    provider: &PlaybackTrueNasMedia,
) -> Result<(&str, &str, &str), ProviderError> {
    match provider {
        PlaybackTrueNasMedia::Refresh {
            credential_owner_id,
            server_id,
            path,
        }
        | PlaybackTrueNasMedia::Proxy {
            credential_owner_id,
            server_id,
            path,
            ..
        } => Ok((credential_owner_id, server_id, path)),
        PlaybackTrueNasMedia::Direct { .. } => Err(ProviderError::InvalidConfig(
            "TrueNAS direct media has no provider resource descriptor".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{
        InMemoryProviderStore, ProviderStore, ProviderStoreExt, VersionedPlayback,
    };

    #[tokio::test]
    async fn resource_action_accepts_refresh_media_from_version_mapping() {
        let version = "truenas-refresh";
        let store: Arc<dyn ProviderStore> = Arc::new(InMemoryProviderStore::new(16));
        let media = PlaybackTrueNasMedia::Refresh {
            credential_owner_id: "42".to_string(),
            server_id: "truenas-main".to_string(),
            path: "/mnt/tank/Videos/Movie.mkv".to_string(),
        };
        let versioned = VersionedPlayback {
            version: version.to_string(),
            result: PlaybackResult {
                playback_infos: HashMap::from([(
                    "proxy".to_string(),
                    PlaybackInfo {
                        thumbnail: None,
                        medias: vec![PlaybackMedia {
                            name: "Movie".to_string(),
                            format: "mkv".to_string(),
                            expire_at: None,
                            metadata: None,
                            p2p_swarm_id: None,
                            provider: PlaybackMediaProvider::TrueNas(media),
                        }],
                        default_media_index: Some(0),
                        subtitles: Vec::new(),
                        default_subtitle_index: None,
                        danmakus: Vec::new(),
                        default_danmaku_index: None,
                    },
                )]),
                default_mode: "proxy".to_string(),
                provider: TrueNasProvider::NAME.to_string(),
                provider_instance_name: None,
                duration_seconds: None,
                playback_kind: Some(crate::models::PlaybackKind::Regular),
                metadata: None,
            },
            expires_at: crate::SystemClock.now().timestamp() + 60,
            playback_context: None,
        };
        store
            .set(&format!("v:{version}"), &versioned, Duration::from_mins(1))
            .await
            .expect("version mapping should be stored");

        let provider = TrueNasProvider::with_http_client(reqwest::Client::new());
        let Err(error) = provider
            .get_resource(Some(&store), version, "proxy", 0, None, None)
            .await
        else {
            panic!("missing credentials should prevent a successful action");
        };

        assert!(matches!(
            error,
            ProviderError::Internal(message)
                if message == "TrueNAS credential repository is unavailable"
        ));
    }

    #[test]
    fn resource_descriptor_accepts_refresh_and_proxy_media() {
        let refresh = PlaybackTrueNasMedia::Refresh {
            credential_owner_id: "42".to_string(),
            server_id: "truenas-main".to_string(),
            path: "/mnt/tank/Videos/Movie.mkv".to_string(),
        };
        let proxy = PlaybackTrueNasMedia::Proxy {
            version: "version".to_string(),
            expires_at: 1_800_000_000,
            mode_name: "direct".to_string(),
            media_index: 0,
            credential_owner_id: "42".to_string(),
            server_id: "truenas-main".to_string(),
            path: "/mnt/tank/Videos/Movie.mkv".to_string(),
        };

        let expected = ("42", "truenas-main", "/mnt/tank/Videos/Movie.mkv");
        assert_eq!(
            resource_descriptor(&refresh).expect("refresh descriptor"),
            expected
        );
        assert_eq!(
            resource_descriptor(&proxy).expect("proxy descriptor"),
            expected
        );
    }

    #[test]
    fn discovers_only_related_subtitle_names() {
        assert!(related_stem("Movie.mkv", "movie.zh-CN.ass"));
        assert!(related_stem("movie.mp4", "MOVIE_en.srt"));
        assert!(related_stem("movie.mkv", "movie.vtt"));
        assert!(!related_stem("movie.mkv", "movie2.srt"));
        assert!(!related_stem("movie.mkv", "trailer.srt"));
        assert_eq!(subtitle_format("movie.zh-CN.ASS"), Some("ASS"));
        assert_eq!(subtitle_format("movie.txt"), None);
        assert_eq!(
            subtitle_language("/mnt/tank/Movie.mkv", "Movie.zh-CN.ass"),
            "zh-CN"
        );
    }
}
