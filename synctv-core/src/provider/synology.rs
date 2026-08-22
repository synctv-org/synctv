//! Synology DSM File Station and Video Station media provider.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use rand::seq::IndexedRandom;
use sha2::{Digest, Sha256};

use super::{
    DynamicBrowsePathSegment, DynamicListQuery, DynamicListResult, DynamicPagination,
    DynamicPlaylistItem, DynamicPlaylistItemThumbnail, DynamicPlaylistProvider, ItemType,
    MediaProvider, NextPlayItem, PlaybackInfo, PlaybackProxyAutoPolicy, PlaybackProxyAutoReason,
    PlaybackProxyPolicy, PlaybackResult, ProviderContext, ProviderCredentialDependency,
    ProviderError, ProviderPlaybackSessionLifecycle, SourceConfig, SourceCover,
};
use crate::models::{
    detect_direct_url_format, normalize_provider_instance_name,
    normalize_provider_instance_name_owned, MediaSourceConfig, PlayMode, PlaybackMedia,
    PlaybackMediaMetadata, PlaybackMediaProvider, PlaybackMetadata, PlaybackSubtitle,
    PlaybackSubtitleProvider, PlaylistSourceConfig, ProviderCredential, ProviderPlaybackSession,
    ProviderTarget, SynologyApiBinding, SynologyAudioTrackMetadata, SynologyLibraryItemKind,
    SynologyMediaSource, SynologyMediaSourceConfig, SynologyPlaybackMetadata,
    SynologyPlaybackProfile, SynologyPlaybackResource, SynologyPlaybackSession,
    SynologyPlaylistSource, SynologyPlaylistSourceConfig, SynologySubtitleMetadata, SynologyTarget,
    UserId, UserProviderCredential,
};
use crate::repository::{NewProviderPlaybackSession, UserProviderCredentialRepository};
use synctv_media_providers::synology::{
    SynologyApiInfo, SynologyApiMap, SynologyClient, SynologyFile, SynologyFileList,
    SynologyLibraryList, SynologyStreamProfile, SynologyVideoItemKind,
};

fn synology_route_selection(
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

const DISCOVERY_QUERY: &[&str] = &[
    "SYNO.API.Auth",
    "SYNO.FileStation.*",
    "SYNO.VideoStation.*",
    "SYNO.VideoStation2.*",
];
const DYNAMIC_MAX_SHUFFLE_ITEMS: usize = 200;
const PLAYBACK_CACHE_TTL: Duration = Duration::from_hours(2);
#[derive(Debug, Clone)]
pub struct SynologyBind {
    pub id: i64,
    pub server_id: String,
    pub endpoint: String,
    pub username: String,
    pub video_station_available: bool,
    pub created_at: i64,
    pub provider_instance_name: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SynologyVideoEntry {
    pub kind: SynologyVideoEntryKind,
    pub metadata: synctv_media_providers::synology::SynologyVideoMetadata,
    pub season: Option<u32>,
    pub episode: Option<u32>,
    pub tv_show_id: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SynologyVideoEntryKind {
    Movie,
    TvShow,
    Episode,
    HomeVideo,
    TvRecording,
}

#[derive(Debug, Clone)]
pub struct SynologyVideoPage {
    pub items: Vec<SynologyVideoEntry>,
    pub total: u64,
    pub page: usize,
    pub has_more: bool,
}

#[derive(Clone)]
struct AuthenticatedSynology {
    client: SynologyClient,
    file_sid: String,
    video_sid: Option<String>,
    apis: SynologyApiMap,
    instance_name: Option<String>,
}

pub struct SynologyProvider {
    http_client: reqwest::Client,
    credential_repo: Option<Arc<UserProviderCredentialRepository>>,
}

impl SynologyProvider {
    pub const NAME: &'static str = "synology";

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
            ProviderError::Internal("Synology credential repository is unavailable".to_string())
        })
    }

    fn credential_repo_or<'a>(
        &'a self,
        fallback: Option<&'a UserProviderCredentialRepository>,
    ) -> Result<&'a UserProviderCredentialRepository, ProviderError> {
        self.credential_repo.as_deref().or(fallback).ok_or_else(|| {
            ProviderError::Internal("Synology credential repository is unavailable".to_string())
        })
    }

    fn client(&self, endpoint: &str) -> Result<SynologyClient, ProviderError> {
        SynologyClient::with_http_client(endpoint, self.http_client.clone()).map_err(Into::into)
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

    #[allow(clippy::too_many_arguments)]
    pub async fn login_and_persist(
        &self,
        user_id: UserId,
        endpoint: String,
        username: String,
        password: String,
        otp_code: Option<String>,
        device_name: Option<String>,
        provider_instance_name: Option<String>,
    ) -> Result<(String, bool), ProviderError> {
        if username.trim().is_empty() || password.is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Synology username and password are required".to_string(),
            ));
        }
        let client = self.client(&endpoint)?;
        let apis = client.discover(DISCOVERY_QUERY).await?;
        let auth_api = required_api(&apis, "SYNO.API.Auth")?;
        let file_login = client
            .login(
                auth_api,
                username.trim(),
                &password,
                "FileStation",
                otp_code.as_deref(),
                device_name.as_deref(),
            )
            .await?;
        let video_station_available = apis.contains_key("SYNO.VideoStation.Library");
        let video_login = if video_station_available {
            Some(
                client
                    .login(
                        auth_api,
                        username.trim(),
                        &password,
                        "VideoStation",
                        otp_code.as_deref(),
                        device_name.as_deref(),
                    )
                    .await?,
            )
        } else {
            None
        };
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
                credential_data: ProviderCredential::Synology {
                    endpoint,
                    username: username.trim().to_string(),
                    password,
                    file_sid: file_login.sid,
                    video_sid: video_login.as_ref().map(|login| login.sid.clone()),
                    device_id: file_login
                        .did
                        .or_else(|| video_login.as_ref().and_then(|login| login.did.clone())),
                    synotoken: file_login.synotoken.or_else(|| {
                        video_login
                            .as_ref()
                            .and_then(|login| login.synotoken.clone())
                    }),
                    apis: store_apis(&apis),
                },
                expires_at: None,
                created_at: now,
                updated_at: now,
            })
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok((server_id, video_station_available))
    }

    async fn authenticated_with_repo(
        &self,
        repo: &UserProviderCredentialRepository,
        user_id: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedSynology, ProviderError> {
        let mut credential = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .ok_or(ProviderError::CredentialRequired)?;
        let instance_name = credential.provider_instance_name.clone();
        let ProviderCredential::Synology {
            endpoint,
            username,
            password,
            mut file_sid,
            mut video_sid,
            mut device_id,
            mut synotoken,
            mut apis,
        } = credential.credential_data.clone()
        else {
            return Err(ProviderError::InvalidCredentialType);
        };
        let client = self.client(&endpoint)?;
        let mut runtime_apis = load_apis(&apis);
        if runtime_apis.is_empty() {
            runtime_apis = client.discover(DISCOVERY_QUERY).await?;
            apis = store_apis(&runtime_apis);
        }
        let auth_api = required_api(&runtime_apis, "SYNO.API.Auth")?;
        let file_list_api = required_api(&runtime_apis, "SYNO.FileStation.List")?;
        let mut changed = false;
        if client
            .list_shares(file_list_api, &file_sid, 0, 1)
            .await
            .is_err()
        {
            let login = client
                .login(auth_api, &username, &password, "FileStation", None, None)
                .await?;
            file_sid = login.sid;
            device_id = login.did.or(device_id);
            synotoken = login.synotoken.or(synotoken);
            changed = true;
        }
        if let Some(library_api) = runtime_apis.get("SYNO.VideoStation.Library") {
            let valid = match video_sid.as_deref() {
                Some(sid) => client.list_video_libraries(library_api, sid).await.is_ok(),
                None => false,
            };
            if !valid {
                let login = client
                    .login(auth_api, &username, &password, "VideoStation", None, None)
                    .await?;
                video_sid = Some(login.sid);
                device_id = login.did.or(device_id);
                synotoken = login.synotoken.or(synotoken);
                changed = true;
            }
        }
        if changed {
            credential.credential_data = ProviderCredential::Synology {
                endpoint,
                username,
                password,
                file_sid: file_sid.clone(),
                video_sid: video_sid.clone(),
                device_id,
                synotoken,
                apis,
            };
            credential.updated_at = Utc::now();
            repo.upsert_by_user_provider_server(&credential)
                .await
                .map_err(|error| ProviderError::Internal(error.to_string()))?;
        }
        Ok(AuthenticatedSynology {
            client,
            file_sid,
            video_sid,
            apis: runtime_apis,
            instance_name,
        })
    }

    async fn authenticated(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<AuthenticatedSynology, ProviderError> {
        self.authenticated_with_repo(self.credential_repo()?, user_id, server_id)
            .await
    }

    pub async fn list_files(
        &self,
        user_id: UserId,
        server_id: &str,
        path: &str,
        page: usize,
        page_size: usize,
        search: Option<&str>,
    ) -> Result<(SynologyFileList, Option<String>), ProviderError> {
        let auth = self.authenticated(user_id, server_id).await?;
        let page = page.max(1);
        let limit = u32::try_from(page_size.clamp(1, 200)).unwrap_or(200);
        let offset = u64::try_from((page - 1).saturating_mul(page_size)).unwrap_or(u64::MAX);
        let list = if let Some(search) = search.map(str::trim).filter(|value| !value.is_empty()) {
            let search_api = required_api(&auth.apis, "SYNO.FileStation.Search")?;
            let root = normalize_file_path(path);
            let task = auth
                .client
                .start_search(search_api, &auth.file_sid, &root, search, true)
                .await?;
            let result = auth
                .client
                .list_search(search_api, &auth.file_sid, &task.taskid, offset, limit)
                .await;
            let _ = auth
                .client
                .stop_search(search_api, &auth.file_sid, &task.taskid)
                .await;
            result?
        } else if path.trim().is_empty() || path.trim() == "/" {
            auth.client
                .list_shares(
                    required_api(&auth.apis, "SYNO.FileStation.List")?,
                    &auth.file_sid,
                    offset,
                    limit,
                )
                .await?
        } else {
            auth.client
                .list_files(
                    required_api(&auth.apis, "SYNO.FileStation.List")?,
                    &auth.file_sid,
                    &normalize_file_path(path),
                    offset,
                    limit,
                )
                .await?
        };
        Ok((list, auth.instance_name))
    }

    pub async fn list_video_libraries(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<(SynologyLibraryList, Option<String>), ProviderError> {
        let auth = self.authenticated(user_id, server_id).await?;
        let sid = auth.video_sid.as_deref().ok_or_else(|| {
            ProviderError::InvalidConfig("Synology Video Station is unavailable".to_string())
        })?;
        let libraries = auth
            .client
            .list_video_libraries(required_api(&auth.apis, "SYNO.VideoStation.Library")?, sid)
            .await?;
        Ok((libraries, auth.instance_name))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn list_video_items(
        &self,
        user_id: UserId,
        server_id: &str,
        source: SynologyPlaylistSource,
        page: usize,
        page_size: usize,
        search: Option<&str>,
    ) -> Result<(SynologyVideoPage, Option<String>), ProviderError> {
        let auth = self.authenticated(user_id, server_id).await?;
        let sid = required_video_sid(&auth)?;
        let page = page.max(1);
        let limit = u32::try_from(page_size.clamp(1, 200)).unwrap_or(200);
        let offset = u64::try_from((page - 1).saturating_mul(page_size)).unwrap_or(u64::MAX);
        let (items, total) = match source {
            SynologyPlaylistSource::Movies { library_id } => {
                let result = auth
                    .client
                    .list_movies(
                        required_api(&auth.apis, "SYNO.VideoStation.Movie")?,
                        sid,
                        library_id,
                        offset,
                        limit,
                        search,
                    )
                    .await?;
                (
                    result
                        .movies
                        .into_iter()
                        .map(|item| SynologyVideoEntry {
                            kind: SynologyVideoEntryKind::Movie,
                            metadata: item.metadata,
                            season: None,
                            episode: None,
                            tv_show_id: None,
                        })
                        .collect(),
                    result.total,
                )
            }
            SynologyPlaylistSource::TvShows { library_id } => {
                let result = auth
                    .client
                    .list_tv_shows(
                        required_api(&auth.apis, "SYNO.VideoStation.TVShow")?,
                        sid,
                        library_id,
                        offset,
                        limit,
                        search,
                    )
                    .await?;
                (
                    result
                        .tvshows
                        .into_iter()
                        .map(|item| SynologyVideoEntry {
                            kind: SynologyVideoEntryKind::TvShow,
                            metadata: item.metadata,
                            season: None,
                            episode: None,
                            tv_show_id: None,
                        })
                        .collect(),
                    result.total,
                )
            }
            SynologyPlaylistSource::Episodes {
                library_id,
                tv_show_id,
            } => {
                let result = auth
                    .client
                    .list_episodes(
                        required_api(&auth.apis, "SYNO.VideoStation.TVShowEpisode")?,
                        sid,
                        library_id,
                        tv_show_id,
                        offset,
                        limit,
                        search,
                    )
                    .await?;
                (
                    result
                        .episodes
                        .into_iter()
                        .map(|item| SynologyVideoEntry {
                            kind: SynologyVideoEntryKind::Episode,
                            metadata: item.metadata,
                            season: Some(item.season),
                            episode: Some(item.episode),
                            tv_show_id: Some(item.tvshow_id),
                        })
                        .collect(),
                    result.total,
                )
            }
            SynologyPlaylistSource::HomeVideos { library_id } => {
                let result = auth
                    .client
                    .list_home_videos(
                        required_api(&auth.apis, "SYNO.VideoStation.HomeVideo")?,
                        sid,
                        library_id,
                        offset,
                        limit,
                        search,
                    )
                    .await?;
                (
                    result
                        .homevideos
                        .into_iter()
                        .map(|item| SynologyVideoEntry {
                            kind: SynologyVideoEntryKind::HomeVideo,
                            metadata: item.metadata,
                            season: None,
                            episode: None,
                            tv_show_id: None,
                        })
                        .collect(),
                    result.total,
                )
            }
            SynologyPlaylistSource::TvRecordings { library_id } => {
                let result = auth
                    .client
                    .list_tv_recordings(
                        required_api(&auth.apis, "SYNO.VideoStation.TVRecording")?,
                        sid,
                        library_id,
                        offset,
                        limit,
                        search,
                    )
                    .await?;
                (
                    result
                        .tv_recordings
                        .into_iter()
                        .map(|item| SynologyVideoEntry {
                            kind: SynologyVideoEntryKind::TvRecording,
                            metadata: item.metadata,
                            season: None,
                            episode: None,
                            tv_show_id: None,
                        })
                        .collect(),
                    result.total,
                )
            }
            SynologyPlaylistSource::Files { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "Synology Video Station listing requires a media library source".to_string(),
                ));
            }
        };
        Ok((
            SynologyVideoPage {
                has_more: offset.saturating_add(u64::from(limit)) < total,
                items,
                total,
                page,
            },
            auth.instance_name,
        ))
    }

    pub async fn list_binds(&self, user_id: UserId) -> Result<Vec<SynologyBind>, ProviderError> {
        self.credential_repo()?
            .get_by_provider(user_id, Self::NAME)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?
            .into_iter()
            .map(|credential| {
                let ProviderCredential::Synology {
                    endpoint,
                    username,
                    video_sid,
                    ..
                } = credential.credential_data
                else {
                    return Err(ProviderError::InvalidCredentialType);
                };
                Ok(SynologyBind {
                    id: credential.id,
                    server_id: credential.server_id,
                    endpoint,
                    username,
                    video_station_available: video_sid.is_some(),
                    created_at: credential.created_at.timestamp(),
                    provider_instance_name: credential.provider_instance_name,
                })
            })
            .collect()
    }

    pub async fn logout_and_delete(
        &self,
        user_id: UserId,
        server_id: &str,
    ) -> Result<bool, ProviderError> {
        let repo = self.credential_repo()?;
        let credential = repo
            .get_by_provider_and_server(user_id, Self::NAME, server_id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        let Some(credential) = credential else {
            return Ok(false);
        };
        if let ProviderCredential::Synology {
            endpoint,
            file_sid,
            video_sid,
            apis,
            ..
        } = &credential.credential_data
        {
            if let Ok(client) = self.client(endpoint) {
                let apis = load_apis(apis);
                if let Some(auth_api) = apis.get("SYNO.API.Auth") {
                    let _ = client.logout(auth_api, file_sid, "FileStation").await;
                    if let Some(video_sid) = video_sid {
                        let _ = client.logout(auth_api, video_sid, "VideoStation").await;
                    }
                }
            }
        }
        repo.delete(credential.id)
            .await
            .map_err(|error| ProviderError::Internal(error.to_string()))?;
        Ok(true)
    }

    pub async fn get_resource(
        &self,
        request: super::StatefulPlaybackResourceRequest<'_>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let super::StatefulPlaybackResourceRequest {
            store,
            session_repo,
            version,
            mode_name,
            media_index,
            request_context,
            range_header,
        } = request;
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let media = versioned
            .result
            .playback_infos
            .get(mode_name)
            .and_then(|info| info.medias.get(media_index))
            .ok_or(ProviderError::NotFound)?;
        let PlaybackMediaProvider::Synology(provider) = &media.provider else {
            return Err(ProviderError::InvalidConfig(
                "Synology cached playback resource is invalid".to_string(),
            ));
        };
        let (owner, server_id, resource) = match provider {
            crate::models::PlaybackSynologyMedia::Refresh {
                credential_owner_id,
                server_id,
                resource,
            }
            | crate::models::PlaybackSynologyMedia::Proxy {
                credential_owner_id,
                server_id,
                resource,
                ..
            } => (credential_owner_id, server_id, resource),
            crate::models::PlaybackSynologyMedia::Direct { .. } => {
                return Err(ProviderError::InvalidConfig(
                    "Synology direct media cannot use the provider proxy resource endpoint"
                        .to_string(),
                ));
            }
        };
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        let url = match resource {
            SynologyPlaybackResource::File { path } => auth.client.download_url(
                required_api(&auth.apis, "SYNO.FileStation.Download")?,
                &auth.file_sid,
                path,
            )?,
            SynologyPlaybackResource::VideoStation {
                file_id,
                profile,
                audio_track,
                ac3_passthrough,
            } => {
                let sid = required_video_sid(&auth)?;
                let streaming_api = required_api(&auth.apis, "SYNO.VideoStation2.Streaming")?;
                let session = auth
                    .client
                    .open_stream(
                        streaming_api,
                        sid,
                        *file_id,
                        stream_profile(*profile),
                        *audio_track,
                        *ac3_passthrough,
                    )
                    .await?;
                let url = match auth.client.stream_url(
                    required_api(&auth.apis, "SYNO.VideoStation.Streaming")?,
                    sid,
                    &session.stream_id,
                    &session.format,
                ) {
                    Ok(url) => url,
                    Err(error) => {
                        let _ = auth
                            .client
                            .close_stream(streaming_api, sid, &session.stream_id, &session.format)
                            .await;
                        return Err(error.into());
                    }
                };
                let playback_context = versioned.playback_context.as_ref().ok_or_else(|| {
                    ProviderError::InvalidConfig(
                        "Synology Video Station stream requires a playback generation context"
                            .to_string(),
                    )
                })?;
                if let Err(error) = session_repo
                    .upsert(NewProviderPlaybackSession {
                        room_id: playback_context.room_id,
                        playback_generation: playback_context.playback_generation,
                        provider_instance_name: versioned.result.provider_instance_name.clone(),
                        credential_owner_id: owner,
                        resource_key: format!("stream:{}", session.stream_id),
                        resource_version: Some(versioned.version.clone()),
                        session: ProviderPlaybackSession::Synology(
                            SynologyPlaybackSession::Stream {
                                server_id: server_id.clone(),
                                stream_id: session.stream_id.clone(),
                                format: session.format.clone(),
                                file_id: *file_id,
                            },
                        ),
                        paused: !playback_context.is_playing,
                    })
                    .await
                {
                    let _ = auth
                        .client
                        .close_stream(streaming_api, sid, &session.stream_id, &session.format)
                        .await;
                    return Err(ProviderError::Internal(error.to_string()));
                }
                url
            }
        };
        super::playback_transport::transport_action_for_target_url(
            url,
            HashMap::new(),
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
        let PlaybackSubtitleProvider::Synology(subtitle) = &subtitle.provider else {
            return Err(ProviderError::InvalidConfig(
                "Synology cached subtitle resource is invalid".to_string(),
            ));
        };
        let (owner, server_id) = match subtitle {
            crate::models::PlaybackSynologySubtitle::File {
                credential_owner_id,
                server_id,
                ..
            }
            | crate::models::PlaybackSynologySubtitle::VideoStation {
                credential_owner_id,
                server_id,
                ..
            } => (credential_owner_id, server_id),
        };
        let owner = owner
            .parse::<UserId>()
            .map_err(ProviderError::InvalidConfig)?;
        let auth = self.authenticated(owner, server_id).await?;
        let url = match subtitle {
            crate::models::PlaybackSynologySubtitle::File { path, .. } => {
                auth.client.download_url(
                    required_api(&auth.apis, "SYNO.FileStation.Download")?,
                    &auth.file_sid,
                    path,
                )?
            }
            crate::models::PlaybackSynologySubtitle::VideoStation {
                file_id,
                subtitle_id,
                preview,
                ..
            } => {
                let sid = required_video_sid(&auth)?;
                auth.client.subtitle_url(
                    required_api(&auth.apis, "SYNO.VideoStation.Subtitle")?,
                    sid,
                    *file_id,
                    subtitle_id,
                    *preview,
                )?
            }
        };
        super::playback_transport::full_response_cache_action_for_target_url(
            url,
            HashMap::new(),
            None,
        )
    }

    pub async fn get_segment(
        &self,
        store: Option<&Arc<dyn super::ProviderStore>>,
        version: &str,
        target_url: String,
        request_context: Option<&super::ExecutionControl>,
        range_header: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let versioned =
            super::playback_transport::lookup_versioned(store, version, request_context).await?;
        let has_synology_media = versioned
            .result
            .playback_infos
            .values()
            .flat_map(|info| &info.medias)
            .any(|media| matches!(media.provider, PlaybackMediaProvider::Synology(_)));
        if !has_synology_media {
            return Err(ProviderError::InvalidConfig(
                "Synology cached segment resource is invalid".to_string(),
            ));
        }
        super::playback_transport::transport_action_for_target_url(
            target_url,
            HashMap::new(),
            range_header,
        )
    }

    pub async fn poster_action(
        &self,
        user_id: UserId,
        server_id: &str,
        item_id: i64,
        media_type: &str,
        poster_mtime: Option<&str>,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        let auth = self.authenticated(user_id, server_id).await?;
        let sid = required_video_sid(&auth)?;
        let url = auth.client.poster_url(
            required_api(&auth.apis, "SYNO.VideoStation.Poster")?,
            sid,
            item_id,
            media_type,
            poster_mtime,
        )?;
        super::playback_transport::transport_action_for_target_url(url, HashMap::new(), None)
    }

    pub async fn file_thumbnail_action(
        &self,
        user_id: UserId,
        server_id: &str,
        path: &str,
        size: &str,
    ) -> Result<super::PlaybackTransportAction, ProviderError> {
        validate_file_path(path)?;
        let auth = self.authenticated(user_id, server_id).await?;
        let url = auth.client.thumbnail_url(
            required_api(&auth.apis, "SYNO.FileStation.Thumb")?,
            &auth.file_sid,
            path,
            size,
        )?;
        super::playback_transport::transport_action_for_target_url(url, HashMap::new(), None)
    }

    fn media_config(
        config: &MediaSourceConfig,
    ) -> Result<&SynologyMediaSourceConfig, ProviderError> {
        match config {
            MediaSourceConfig::Synology(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Synology media source_config".to_string(),
            )),
        }
    }

    fn playlist_config(
        config: &PlaylistSourceConfig,
    ) -> Result<&SynologyPlaylistSourceConfig, ProviderError> {
        match config {
            PlaylistSourceConfig::Synology(config) => Ok(config),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Synology playlist source_config".to_string(),
            )),
        }
    }

    fn source_server_id(source: SourceConfig<'_>) -> Result<&str, ProviderError> {
        match source {
            SourceConfig::Media(config) => Ok(&Self::media_config(config)?.server_id),
            SourceConfig::DynamicPlaylist(config) => Ok(&Self::playlist_config(config)?.server_id),
        }
    }

    fn ensure_playback_proxy_mode_supported(
        &self,
        source_config: SourceConfig<'_>,
        mode: crate::models::PlaybackProxyMode,
    ) -> Result<(), ProviderError> {
        let policy = self.playback_proxy_policy(source_config)?.ok_or_else(|| {
            ProviderError::InvalidConfig(
                "Synology playback proxy policy is unavailable".to_string(),
            )
        })?;
        if policy.supported_modes.contains(&mode) {
            Ok(())
        } else {
            Err(ProviderError::InvalidConfig(format!(
                "Synology source does not support playback proxy mode '{}'",
                mode.as_str()
            )))
        }
    }
}

#[async_trait]
impl MediaProvider for SynologyProvider {
    fn name(&self) -> &'static str {
        Self::NAME
    }

    fn playback_proxy_policy(
        &self,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<PlaybackProxyPolicy>, ProviderError> {
        let (current_mode, variant, supports_direct) = match source_config {
            SourceConfig::Media(MediaSourceConfig::Synology(config)) => (
                config.proxy_mode,
                match config.source {
                    SynologyMediaSource::File { .. } => "file",
                    SynologyMediaSource::LibraryItem { .. } => "library_item",
                },
                matches!(config.source, SynologyMediaSource::File { .. }),
            ),
            SourceConfig::DynamicPlaylist(PlaylistSourceConfig::Synology(config)) => (
                config.proxy_mode,
                match config.source {
                    SynologyPlaylistSource::Files { .. } => "file",
                    SynologyPlaylistSource::Movies { .. }
                    | SynologyPlaylistSource::TvShows { .. }
                    | SynologyPlaylistSource::Episodes { .. }
                    | SynologyPlaylistSource::HomeVideos { .. }
                    | SynologyPlaylistSource::TvRecordings { .. } => "library_item",
                },
                matches!(config.source, SynologyPlaylistSource::Files { .. }),
            ),
            _ => {
                return Err(ProviderError::InvalidConfig(
                    "Synology requires Synology source_config".to_string(),
                ));
            }
        };
        let auto_policies = vec![PlaybackProxyAutoPolicy::new(
            variant,
            crate::models::PlaybackProxyMode::Only,
            PlaybackProxyAutoReason::ProviderSession,
        )];
        Ok(Some(if supports_direct {
            PlaybackProxyPolicy::all_modes(current_mode, auto_policies)
        } else {
            PlaybackProxyPolicy::proxy_with_direct_fallback(current_mode, auto_policies)
        }))
    }

    fn set_playback_proxy_mode(
        &self,
        source_config: &mut MediaSourceConfig,
        mode: crate::models::PlaybackProxyMode,
    ) -> Result<(), ProviderError> {
        self.ensure_playback_proxy_mode_supported(SourceConfig::media(source_config), mode)?;
        let MediaSourceConfig::Synology(config) = source_config else {
            return Err(ProviderError::InvalidConfig(
                "Synology requires Synology media source_config".to_string(),
            ));
        };
        config.proxy_mode = mode;
        Ok(())
    }

    fn as_dynamic_playlist_provider(&self) -> Option<&dyn DynamicPlaylistProvider> {
        Some(self)
    }

    fn as_playback_session_lifecycle(&self) -> Option<&dyn ProviderPlaybackSessionLifecycle> {
        Some(self)
    }

    async fn generate_playback(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: &MediaSourceConfig,
    ) -> Result<PlaybackResult, ProviderError> {
        let config = Self::media_config(source_config)?;
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        let auth = self
            .authenticated_with_repo(repo, owner, &config.server_id)
            .await?;
        let mut result = match &config.source {
            SynologyMediaSource::File { path } => {
                generate_file_playback(&auth, owner, &config.server_id, path).await
            }
            SynologyMediaSource::LibraryItem {
                kind,
                item_id,
                file_id,
            } => {
                generate_video_playback(
                    &auth,
                    owner,
                    &config.server_id,
                    *kind,
                    *item_id,
                    *file_id,
                    ctx.playback_client_profile(),
                )
                .await
            }
        }?;
        if synology_route_selection(config.proxy_mode).direct {
            if let SynologyMediaSource::File { path } = &config.source {
                let direct_url = auth.client.download_url(
                    required_api(&auth.apis, "SYNO.FileStation.Download")?,
                    &auth.file_sid,
                    path,
                )?;
                if let Some(info) = result.playback_infos.get("original").cloned() {
                    let medias = info
                        .medias
                        .into_iter()
                        .map(|mut media| {
                            media.provider = PlaybackMediaProvider::Synology(
                                crate::models::PlaybackSynologyMedia::Direct {
                                    url: direct_url.clone(),
                                    headers: HashMap::new(),
                                },
                            );
                            media
                        })
                        .collect();
                    result
                        .playback_infos
                        .insert("direct".to_string(), PlaybackInfo { medias, ..info });
                }
            }
        }
        let cache_identity = serde_json::to_string(config).map_err(ProviderError::JsonError)?;
        let result = super::cached_versioned_playback_or_fill(
            Self::NAME,
            &format!(
                "playback:{owner}:{}:{cache_identity}:profile:{}",
                config.server_id,
                super::playback_profile_cache_token(ctx.playback_client_profile())
            ),
            PLAYBACK_CACHE_TTL,
            ctx,
            |result, version, expires_at| {
                mark_synology_playback_resources(
                    result,
                    version,
                    expires_at,
                    config.proxy_mode,
                    ctx.playback_client_profile(),
                );
            },
            || async { Ok(result) },
        )
        .await?;
        let result = super::require_compatible_playback_route(
            result,
            config.proxy_mode,
            ctx.playback_client_profile(),
        )?;

        if let SynologyMediaSource::LibraryItem { file_id, .. } = &config.source {
            let resource_version = result.playback_infos.values().find_map(|info| {
                info.medias.iter().find_map(|media| match &media.provider {
                    PlaybackMediaProvider::Synology(
                        crate::models::PlaybackSynologyMedia::Proxy { version, .. },
                    ) => Some(version.clone()),
                    _ => None,
                })
            });
            if let Some(resource_version) = resource_version {
                if let Some((repo, session)) = super::playback_session_registration(
                    ctx,
                    format!("watch:{resource_version}"),
                    Some(resource_version.clone()),
                    ProviderPlaybackSession::Synology(SynologyPlaybackSession::WatchSession {
                        server_id: config.server_id.clone(),
                        file_id: *file_id,
                    }),
                )? {
                    repo.upsert(session)
                        .await
                        .map_err(|error| ProviderError::Internal(error.to_string()))?;
                }
            }
        }
        Ok(result)
    }

    async fn validate_source_config(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<(), ProviderError> {
        let current_mode = self
            .playback_proxy_policy(source_config)?
            .ok_or_else(|| {
                ProviderError::InvalidConfig(
                    "Synology playback proxy policy is unavailable".to_string(),
                )
            })?
            .current_mode;
        self.ensure_playback_proxy_mode_supported(source_config, current_mode)?;
        let owner = *ctx.user_id().ok_or(ProviderError::CredentialRequired)?;
        let server_id = Self::source_server_id(source_config)?;
        let repo = self.credential_repo_or(ctx.credential_repo)?;
        self.authenticated_with_repo(repo, owner, server_id).await?;
        if let SourceConfig::Media(config) = source_config {
            let config = Self::media_config(config)?;
            match &config.source {
                SynologyMediaSource::File { path } => validate_file_path(path)?,
                SynologyMediaSource::LibraryItem {
                    item_id, file_id, ..
                } if *item_id <= 0 || *file_id <= 0 => {
                    return Err(ProviderError::InvalidConfig(
                        "Synology item_id and file_id must be positive".to_string(),
                    ));
                }
                SynologyMediaSource::LibraryItem { .. } => {}
            }
        }
        Ok(())
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
            crate::models::SourceProvider::Synology,
            *owner,
            Self::source_server_id(source_config)?,
        )])
    }

    async fn source_cover(
        &self,
        ctx: &ProviderContext<'_>,
        source_config: SourceConfig<'_>,
    ) -> Result<Option<SourceCover>, ProviderError> {
        let SourceConfig::Media(config) = source_config else {
            return Ok(None);
        };
        let config = Self::media_config(config)?;
        let SynologyMediaSource::LibraryItem { kind, item_id, .. } = config.source else {
            return Ok(None);
        };
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        Ok(Some(SourceCover::SynologyPoster {
            server_id: config.server_id.clone(),
            credential_owner_id: owner,
            item_id,
            media_type: media_type(kind).to_string(),
            poster_mtime: None,
        }))
    }
}

#[async_trait]
impl ProviderPlaybackSessionLifecycle for SynologyProvider {
    async fn progress(
        &self,
        ctx: &ProviderContext<'_>,
        record: &crate::models::ProviderPlaybackSessionRecord,
        position: f64,
        _paused: bool,
    ) -> Result<(), ProviderError> {
        let ProviderPlaybackSession::Synology(SynologyPlaybackSession::WatchSession {
            server_id,
            file_id,
        }) = &record.session
        else {
            return Ok(());
        };
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let credential_repo = self.credential_repo_or(ctx.credential_repo)?;
        let auth = self
            .authenticated_with_repo(credential_repo, owner, server_id)
            .await?;
        let sid = required_video_sid(&auth)?;
        let position_seconds = std::time::Duration::try_from_secs_f64(position)
            .map_err(|_| {
                ProviderError::InvalidConfig(
                    "Synology watch position must be finite and non-negative".to_string(),
                )
            })?
            .as_secs();
        auth.client
            .set_watch_position(
                required_api(&auth.apis, "SYNO.VideoStation.WatchStatus")?,
                sid,
                *file_id,
                position_seconds,
            )
            .await
            .map_err(ProviderError::from)
    }

    async fn cleanup(
        &self,
        ctx: &ProviderContext<'_>,
        record: &crate::models::ProviderPlaybackSessionRecord,
    ) -> Result<(), ProviderError> {
        match &record.session {
            ProviderPlaybackSession::Synology(SynologyPlaybackSession::WatchSession { .. }) => {
                self.progress(ctx, record, record.stop_position.unwrap_or(0.0), false)
                    .await
            }
            ProviderPlaybackSession::Synology(SynologyPlaybackSession::Stream {
                server_id,
                stream_id,
                format,
                ..
            }) => {
                let owner = *ctx
                    .credential_owner_or_user_id()
                    .ok_or(ProviderError::CredentialRequired)?;
                let credential_repo = self.credential_repo_or(ctx.credential_repo)?;
                let auth = self
                    .authenticated_with_repo(credential_repo, owner, server_id)
                    .await?;
                auth.client
                    .close_stream(
                        required_api(&auth.apis, "SYNO.VideoStation2.Streaming")?,
                        required_video_sid(&auth)?,
                        stream_id,
                        format,
                    )
                    .await
                    .map_err(ProviderError::from)
            }
            _ => Err(ProviderError::InvalidConfig(
                "Synology lifecycle received another provider's session".to_string(),
            )),
        }
    }
}

#[async_trait]
impl DynamicPlaylistProvider for SynologyProvider {
    async fn list_playlist(
        &self,
        ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: Option<&ProviderTarget>,
        query: DynamicListQuery,
    ) -> Result<DynamicListResult, ProviderError> {
        let config = Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
            ProviderError::InvalidConfig("Missing Synology source_config".to_string())
        })?)?;
        let owner = *ctx
            .credential_owner_or_user_id()
            .ok_or(ProviderError::CredentialRequired)?;
        let DynamicPagination::Page { page } = query.pagination else {
            return Err(ProviderError::InvalidConfig(
                "Synology uses page pagination".to_string(),
            ));
        };
        let auth = self
            .authenticated_with_repo(
                self.credential_repo_or(ctx.credential_repo)?,
                owner,
                &config.server_id,
            )
            .await?;
        list_dynamic_page(
            &auth,
            config,
            owner,
            target,
            page.max(1),
            query.page_size.max(1),
            query.search.as_deref(),
        )
        .await
    }

    async fn resolve_item(
        &self,
        _ctx: &ProviderContext<'_>,
        playlist: &crate::models::Playlist,
        target: &ProviderTarget,
    ) -> Result<Option<NextPlayItem>, ProviderError> {
        let config = Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
            ProviderError::InvalidConfig("Missing Synology source_config".to_string())
        })?)?;
        match target {
            ProviderTarget::Synology(SynologyTarget::File { relative_path }) => {
                let SynologyPlaylistSource::Files { path } = &config.source else {
                    return Err(ProviderError::InvalidConfig(
                        "Synology file target does not belong to this playlist".to_string(),
                    ));
                };
                let full_path = join_path(path, relative_path);
                Ok(Some(NextPlayItem {
                    name: full_path
                        .rsplit('/')
                        .next()
                        .unwrap_or("Synology media")
                        .to_string(),
                    item_type: ItemType::Media,
                    source_config: MediaSourceConfig::Synology(SynologyMediaSourceConfig {
                        server_id: config.server_id.clone(),
                        proxy_mode: config.proxy_mode,
                        source: SynologyMediaSource::File { path: full_path },
                    }),
                    target: target.clone(),
                }))
            }
            ProviderTarget::Synology(SynologyTarget::LibraryItem {
                kind,
                item_id,
                file_id,
                ..
            }) => Ok(Some(NextPlayItem {
                name: format!("Synology {} {item_id}", media_type(*kind)),
                item_type: ItemType::Media,
                source_config: MediaSourceConfig::Synology(SynologyMediaSourceConfig {
                    server_id: config.server_id.clone(),
                    proxy_mode: config.proxy_mode,
                    source: SynologyMediaSource::LibraryItem {
                        kind: *kind,
                        item_id: *item_id,
                        file_id: *file_id,
                    },
                }),
                target: target.clone(),
            })),
            ProviderTarget::Synology(SynologyTarget::TvShow { .. }) => Ok(None),
            _ => Err(ProviderError::InvalidConfig(
                "Expected Synology dynamic target".to_string(),
            )),
        }
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
        let parent_target = match target {
            ProviderTarget::Synology(SynologyTarget::File { relative_path }) => relative_path
                .rsplit_once('/')
                .map(|(parent, _)| parent)
                .filter(|parent| !parent.is_empty())
                .map(|parent| ProviderTarget::synology_file(parent.to_string())),
            ProviderTarget::Synology(SynologyTarget::LibraryItem {
                kind: SynologyLibraryItemKind::Episode,
                parent_id: Some(tv_show_id),
                ..
            }) => {
                let config =
                    Self::playlist_config(playlist.source_config.as_ref().ok_or_else(|| {
                        ProviderError::InvalidConfig("Missing Synology source_config".to_string())
                    })?)?;
                match config.source {
                    SynologyPlaylistSource::TvShows { library_id } => {
                        Some(ProviderTarget::synology_tv_show(library_id, *tv_show_id))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
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
            if has_enough_media_for_next(&media, target, play_mode) || !result.has_more {
                break;
            }
            page = page.saturating_add(1);
        }
        let selected = select_next_media(&media, target, play_mode);
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
        match target {
            Some(ProviderTarget::Synology(SynologyTarget::File { relative_path })) => {
                let mut current = String::new();
                Ok(relative_path
                    .split('/')
                    .filter(|part| !part.is_empty())
                    .map(|part| {
                        current = join_path(&current, part);
                        DynamicBrowsePathSegment {
                            name: part.to_string(),
                            target: ProviderTarget::synology_file(current.clone()),
                        }
                    })
                    .collect())
            }
            Some(ProviderTarget::Synology(SynologyTarget::TvShow {
                library_id,
                tv_show_id,
            })) => Ok(vec![DynamicBrowsePathSegment {
                name: format!("TV show {tv_show_id}"),
                target: ProviderTarget::synology_tv_show(*library_id, *tv_show_id),
            }]),
            _ => Ok(Vec::new()),
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn list_dynamic_page(
    auth: &AuthenticatedSynology,
    config: &SynologyPlaylistSourceConfig,
    owner: UserId,
    target: Option<&ProviderTarget>,
    page: usize,
    page_size: usize,
    search: Option<&str>,
) -> Result<DynamicListResult, ProviderError> {
    let limit = u32::try_from(page_size.clamp(1, 200)).unwrap_or(200);
    let offset = u64::try_from((page - 1).saturating_mul(page_size)).unwrap_or(u64::MAX);
    let (items, total) = match &config.source {
        SynologyPlaylistSource::Files { path } => {
            let relative = match target {
                Some(ProviderTarget::Synology(SynologyTarget::File { relative_path })) => {
                    relative_path.as_str()
                }
                None => "",
                _ => {
                    return Err(ProviderError::InvalidConfig(
                        "Expected Synology file target".to_string(),
                    ));
                }
            };
            let folder = join_path(path, relative);
            let response = auth
                .client
                .list_files(
                    required_api(&auth.apis, "SYNO.FileStation.List")?,
                    &auth.file_sid,
                    &folder,
                    offset,
                    limit,
                )
                .await?;
            let items = response
                .files
                .into_iter()
                .map(|file| map_file_item(path, file, owner, &config.server_id))
                .collect::<Result<Vec<_>, _>>()?;
            (items, response.total)
        }
        SynologyPlaylistSource::Movies { library_id } => {
            ensure_root_target(target)?;
            let sid = required_video_sid(auth)?;
            let response = auth
                .client
                .list_movies(
                    required_api(&auth.apis, "SYNO.VideoStation.Movie")?,
                    sid,
                    *library_id,
                    offset,
                    limit,
                    search,
                )
                .await?;
            (
                response
                    .movies
                    .into_iter()
                    .filter_map(|item| {
                        map_video_item(
                            SynologyLibraryItemKind::Movie,
                            item.metadata,
                            None,
                            owner,
                            &config.server_id,
                        )
                    })
                    .collect(),
                response.total,
            )
        }
        SynologyPlaylistSource::TvShows { library_id } => match target {
            None => {
                let sid = required_video_sid(auth)?;
                let response = auth
                    .client
                    .list_tv_shows(
                        required_api(&auth.apis, "SYNO.VideoStation.TVShow")?,
                        sid,
                        *library_id,
                        offset,
                        limit,
                        search,
                    )
                    .await?;
                (
                    response
                        .tvshows
                        .into_iter()
                        .map(|item| DynamicPlaylistItem {
                            name: item.metadata.title,
                            item_type: ItemType::Playlist,
                            target: ProviderTarget::synology_tv_show(*library_id, item.metadata.id),
                            size: None,
                            thumbnail: Some(synology_thumbnail(
                                owner,
                                &config.server_id,
                                item.metadata.id,
                                "tvshow",
                                item.metadata.additional.poster_mtime,
                            )),
                            description: non_empty(item.metadata.additional.summary),
                            modified_at: Some(item.metadata.create_time),
                            source_config: None,
                            metadata: None,
                        })
                        .collect(),
                    response.total,
                )
            }
            Some(ProviderTarget::Synology(SynologyTarget::TvShow {
                library_id: target_library_id,
                tv_show_id,
            })) if target_library_id == library_id => {
                list_episode_page(
                    auth,
                    *library_id,
                    *tv_show_id,
                    offset,
                    limit,
                    search,
                    owner,
                    &config.server_id,
                )
                .await?
            }
            _ => {
                return Err(ProviderError::InvalidConfig(
                    "Expected Synology TV show target".to_string(),
                ));
            }
        },
        SynologyPlaylistSource::Episodes {
            library_id,
            tv_show_id,
        } => {
            ensure_root_target(target)?;
            list_episode_page(
                auth,
                *library_id,
                *tv_show_id,
                offset,
                limit,
                search,
                owner,
                &config.server_id,
            )
            .await?
        }
        SynologyPlaylistSource::HomeVideos { library_id } => {
            ensure_root_target(target)?;
            let sid = required_video_sid(auth)?;
            let response = auth
                .client
                .list_home_videos(
                    required_api(&auth.apis, "SYNO.VideoStation.HomeVideo")?,
                    sid,
                    *library_id,
                    offset,
                    limit,
                    search,
                )
                .await?;
            (
                response
                    .homevideos
                    .into_iter()
                    .filter_map(|item| {
                        map_video_item(
                            SynologyLibraryItemKind::HomeVideo,
                            item.metadata,
                            None,
                            owner,
                            &config.server_id,
                        )
                    })
                    .collect(),
                response.total,
            )
        }
        SynologyPlaylistSource::TvRecordings { library_id } => {
            ensure_root_target(target)?;
            let sid = required_video_sid(auth)?;
            let response = auth
                .client
                .list_tv_recordings(
                    required_api(&auth.apis, "SYNO.VideoStation.TVRecording")?,
                    sid,
                    *library_id,
                    offset,
                    limit,
                    search,
                )
                .await?;
            (
                response
                    .tv_recordings
                    .into_iter()
                    .filter_map(|item| {
                        map_video_item(
                            SynologyLibraryItemKind::TvRecording,
                            item.metadata,
                            None,
                            owner,
                            &config.server_id,
                        )
                    })
                    .collect(),
                response.total,
            )
        }
    };
    Ok(DynamicListResult {
        items,
        pagination: DynamicPagination::Page { page },
        has_more: offset.saturating_add(u64::from(limit)) < total,
        supports_search: !matches!(&config.source, SynologyPlaylistSource::Files { .. }),
    })
}

#[allow(clippy::too_many_arguments)]
async fn list_episode_page(
    auth: &AuthenticatedSynology,
    library_id: i64,
    tv_show_id: i64,
    offset: u64,
    limit: u32,
    search: Option<&str>,
    owner: UserId,
    server_id: &str,
) -> Result<(Vec<DynamicPlaylistItem>, u64), ProviderError> {
    let sid = required_video_sid(auth)?;
    let response = auth
        .client
        .list_episodes(
            required_api(&auth.apis, "SYNO.VideoStation.TVShowEpisode")?,
            sid,
            library_id,
            tv_show_id,
            offset,
            limit,
            search,
        )
        .await?;
    Ok((
        response
            .episodes
            .into_iter()
            .filter_map(|item| {
                map_video_item(
                    SynologyLibraryItemKind::Episode,
                    item.metadata,
                    Some(tv_show_id),
                    owner,
                    server_id,
                )
            })
            .collect(),
        response.total,
    ))
}

fn map_file_item(
    root: &str,
    file: SynologyFile,
    owner: UserId,
    server_id: &str,
) -> Result<DynamicPlaylistItem, ProviderError> {
    let relative = file
        .path
        .strip_prefix(root.trim_end_matches('/'))
        .unwrap_or(&file.path)
        .trim_start_matches('/')
        .to_string();
    Ok(DynamicPlaylistItem {
        name: file.name,
        item_type: if file.isdir {
            ItemType::Playlist
        } else {
            ItemType::Media
        },
        target: ProviderTarget::synology_file(relative),
        size: (!file.isdir).then_some(file.additional.size),
        thumbnail: (!file.isdir).then_some(DynamicPlaylistItemThumbnail::SynologyFile {
            server_id: server_id.to_string(),
            credential_owner_id: owner,
            path: file.path,
        }),
        description: None,
        modified_at: i64::try_from(file.additional.time.mtime).ok(),
        source_config: None,
        metadata: None,
    })
}

fn map_video_item(
    kind: SynologyLibraryItemKind,
    metadata: synctv_media_providers::synology::SynologyVideoMetadata,
    parent_id: Option<i64>,
    owner: UserId,
    server_id: &str,
) -> Option<DynamicPlaylistItem> {
    let file = metadata.additional.file.first()?;
    Some(DynamicPlaylistItem {
        name: metadata.title,
        item_type: ItemType::Media,
        target: ProviderTarget::synology_library_item(kind, metadata.id, file.id, parent_id),
        size: Some(file.filesize),
        thumbnail: Some(synology_thumbnail(
            owner,
            server_id,
            metadata.id,
            media_type(kind),
            metadata.additional.poster_mtime,
        )),
        description: non_empty(metadata.additional.summary),
        modified_at: Some(metadata.create_time),
        source_config: None,
        metadata: None,
    })
}

fn synology_thumbnail(
    owner: UserId,
    server_id: &str,
    item_id: i64,
    media_type: &str,
    poster_mtime: Option<String>,
) -> DynamicPlaylistItemThumbnail {
    DynamicPlaylistItemThumbnail::SynologyPoster {
        server_id: server_id.to_string(),
        credential_owner_id: owner,
        item_id,
        media_type: media_type.to_string(),
        poster_mtime,
    }
}

fn ensure_root_target(target: Option<&ProviderTarget>) -> Result<(), ProviderError> {
    if target.is_some() {
        return Err(ProviderError::InvalidConfig(
            "Synology playlist target must be empty at this level".to_string(),
        ));
    }
    Ok(())
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn has_enough_media_for_next(
    media: &[DynamicPlaylistItem],
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
    media: &'a [DynamicPlaylistItem],
    target: &ProviderTarget,
    play_mode: PlayMode,
) -> Option<&'a DynamicPlaylistItem> {
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

fn join_path(base: &str, relative: &str) -> String {
    let base = base.trim_end_matches('/');
    let relative = relative.trim_matches('/');
    match (base.is_empty(), relative.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{relative}"),
        (false, true) => base.to_string(),
        (false, false) => format!("{base}/{relative}"),
    }
}

async fn generate_file_playback(
    auth: &AuthenticatedSynology,
    owner: UserId,
    server_id: &str,
    path: &str,
) -> Result<PlaybackResult, ProviderError> {
    validate_file_path(path)?;
    let (parent, name) = split_path(path)?;
    let (file, subtitle_files) = find_file_and_subtitles(auth, parent, name, path).await?;
    let media = PlaybackMedia {
        name: file.name.clone(),
        format: detect_direct_url_format(path).to_string(),
        expire_at: None,
        metadata: Some(file_metadata(&file)),
        p2p_swarm_id: Some(synology_swarm_id(
            auth.instance_name.as_deref(),
            "media",
            server_id,
            &format!(
                "file:{path}:size:{}:mtime:{}:ctime:{}",
                file.additional.size, file.additional.time.mtime, file.additional.time.ctime
            ),
        )),
        provider: PlaybackMediaProvider::Synology(crate::models::PlaybackSynologyMedia::Refresh {
            credential_owner_id: owner.to_string(),
            server_id: server_id.to_string(),
            resource: SynologyPlaybackResource::File {
                path: path.to_string(),
            },
        }),
    };
    let subtitles = subtitle_files
        .into_iter()
        .enumerate()
        .map(|(subtitle_index, file)| PlaybackSubtitle {
            name: file.name.clone(),
            language: file_subtitle_language(path, &file.name),
            format: file_subtitle_format(&file.name)
                .unwrap_or_default()
                .to_string(),
            p2p_swarm_id: Some(synology_swarm_id(
                auth.instance_name.as_deref(),
                "subtitle",
                server_id,
                &format!(
                    "file:{}:size:{}:mtime:{}:ctime:{}",
                    file.path,
                    file.additional.size,
                    file.additional.time.mtime,
                    file.additional.time.ctime
                ),
            )),
            provider: PlaybackSubtitleProvider::Synology(
                crate::models::PlaybackSynologySubtitle::File {
                    version: String::new(),
                    expires_at: 0,
                    mode_name: String::new(),
                    subtitle_index,
                    credential_owner_id: owner.to_string(),
                    server_id: server_id.to_string(),
                    path: file.path,
                },
            ),
        })
        .collect();
    Ok(single_result(
        "original",
        media,
        subtitles,
        auth.instance_name.clone(),
        None,
        Some(false),
        None,
    ))
}

async fn find_file_and_subtitles(
    auth: &AuthenticatedSynology,
    parent: &str,
    media_name: &str,
    media_path: &str,
) -> Result<(SynologyFile, Vec<SynologyFile>), ProviderError> {
    const PAGE_SIZE: u32 = 1_000;

    let api = required_api(&auth.apis, "SYNO.FileStation.List")?;
    let mut offset = 0_u64;
    let mut media = None;
    let mut subtitles = Vec::new();
    loop {
        let listing = auth
            .client
            .list_files(api, &auth.file_sid, parent, offset, PAGE_SIZE)
            .await?;
        let count = listing.files.len() as u64;
        for file in listing.files {
            if file.name == media_name && !file.isdir {
                media = Some(file.clone());
            }
            if subtitles.len() < 32 && related_file_subtitle(media_path, &file) {
                subtitles.push(file);
            }
        }
        offset = offset.saturating_add(count);
        if count == 0 || offset >= listing.total || (media.is_some() && subtitles.len() >= 32) {
            break;
        }
    }
    Ok((media.ok_or(ProviderError::NotFound)?, subtitles))
}

async fn generate_video_playback(
    auth: &AuthenticatedSynology,
    owner: UserId,
    server_id: &str,
    kind: SynologyLibraryItemKind,
    item_id: i64,
    file_id: i64,
    playback_client_profile: Option<&super::PlaybackClientProfile>,
) -> Result<PlaybackResult, ProviderError> {
    let sid = required_video_sid(auth)?;
    let item_kind = upstream_item_kind(kind);
    let item = auth
        .client
        .video_item(
            required_api(&auth.apis, video_item_api(kind))?,
            sid,
            item_kind,
            item_id,
        )
        .await?;
    let file = item
        .additional
        .file
        .iter()
        .find(|file| file.id == file_id)
        .ok_or(ProviderError::NotFound)?;
    let tracks = match auth.apis.get("SYNO.VideoStation.AudioTrack") {
        Some(api) => auth
            .client
            .list_audio_tracks(api, sid, file_id)
            .await
            .map(|tracks| tracks.trackinfo)
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let subtitles = match auth.apis.get("SYNO.VideoStation.Subtitle") {
        Some(api) => auth
            .client
            .list_subtitles(api, sid, file_id)
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    let duration = file.duration_seconds();
    let playback_subtitles = subtitles
        .iter()
        .enumerate()
        .map(|(index, subtitle)| PlaybackSubtitle {
            name: if subtitle.title.is_empty() {
                subtitle.lang.clone()
            } else {
                subtitle.title.clone()
            },
            language: subtitle.lang.clone(),
            format: subtitle.format.clone(),
            p2p_swarm_id: Some(synology_swarm_id(
                auth.instance_name.as_deref(),
                "subtitle",
                server_id,
                &format!(
                    "video_station:file:{file_id}:subtitle:{}:preview:{}",
                    subtitle.id, subtitle.need_preview
                ),
            )),
            provider: PlaybackSubtitleProvider::Synology(
                crate::models::PlaybackSynologySubtitle::VideoStation {
                    version: String::new(),
                    expires_at: 0,
                    mode_name: String::new(),
                    subtitle_index: index,
                    credential_owner_id: owner.to_string(),
                    server_id: server_id.to_string(),
                    file_id,
                    subtitle_id: subtitle.id.clone(),
                    preview: subtitle.need_preview,
                },
            ),
        })
        .collect::<Vec<_>>();
    let default_audio = tracks
        .iter()
        .find(|track| track.is_default)
        .or_else(|| tracks.first())
        .map(|track| track.id);
    let profiles = [
        ("raw", SynologyPlaybackProfile::Raw, "video"),
        ("hls_remux", SynologyPlaybackProfile::HlsRemux, "m3u8"),
        ("medium", SynologyPlaybackProfile::HlsMedium, "m3u8"),
        ("low", SynologyPlaybackProfile::HlsLow, "m3u8"),
    ];
    let mut playback_infos = HashMap::new();
    for (name, profile, format) in profiles {
        let mode_name = if name == "raw" { "raw" } else { "transcoded" };
        let info = playback_infos
            .entry(mode_name.to_string())
            .or_insert_with(|| PlaybackInfo {
                thumbnail: None,
                medias: Vec::new(),
                default_media_index: Some(0),
                subtitles: playback_subtitles.clone(),
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            });
        let resource = SynologyPlaybackResource::VideoStation {
            file_id,
            profile,
            audio_track: default_audio,
            ac3_passthrough: true,
        };
        let descriptor = serde_json::to_string(&resource)
            .expect("Synology playback resource is JSON serializable");
        let descriptor = format!(
            "{descriptor}:path:{}:size:{}:duration:{}:video:{}:audio:{}",
            file.path, file.filesize, file.duration, file.video_codec, file.audio_codec
        );
        info.medias.push(PlaybackMedia {
            name: match name {
                "raw" => "Original".to_string(),
                "hls_remux" => "Original HLS".to_string(),
                "medium" => "Medium".to_string(),
                "low" => "Low".to_string(),
                _ => name.to_string(),
            },
            format: format.to_string(),
            expire_at: None,
            metadata: Some(video_file_metadata(file, profile)),
            p2p_swarm_id: Some(synology_swarm_id(
                auth.instance_name.as_deref(),
                "media",
                server_id,
                &descriptor,
            )),
            provider: PlaybackMediaProvider::Synology(
                crate::models::PlaybackSynologyMedia::Refresh {
                    credential_owner_id: owner.to_string(),
                    server_id: server_id.to_string(),
                    resource,
                },
            ),
        });
    }
    let metadata = SynologyPlaybackMetadata {
        title: item.title.clone(),
        summary: item.additional.summary.clone(),
        tagline: item.tagline.clone(),
        certificate: item.certificate.clone(),
        rating: item.rating,
        actors: item.additional.actor.clone(),
        directors: item.additional.director.clone(),
        writers: item.additional.writer.clone(),
        genres: item.additional.genre.clone(),
        item_id,
        file_id,
        kind,
        path: file.sharepath.clone(),
        size: file.filesize,
        duration_seconds: duration.unwrap_or_default(),
        progress_seconds: file.position,
        width: file.resolutionx,
        height: file.resolutiony,
        video_codec: file.video_codec.clone(),
        audio_codec: file.audio_codec.clone(),
        container: file.container_type.clone(),
        video_bitrate: file.video_bitrate,
        audio_bitrate: file.audio_bitrate,
        frame_rate_numerator: file.frame_rate_num,
        frame_rate_denominator: file.frame_rate_den,
        audio_channels: file.channel,
        audio_frequency_hz: file.frequency,
        poster_url: None,
        backdrop_url: None,
        watched: item.additional.watched_ratio >= 0.9
            || duration.is_some_and(|duration| duration > 0 && file.position >= duration),
        watched_ratio: item.additional.watched_ratio,
        parental_controlled: item.additional.is_parental_controlled,
        create_time: item.create_time,
        last_watched: item.last_watched,
        audio_tracks: tracks
            .iter()
            .map(|track| SynologyAudioTrackMetadata {
                id: track.id,
                language: track.language.clone(),
                codec: track.codec.clone(),
                channels: track.channel,
                bitrate: track.bitrate,
                default: track.is_default,
            })
            .collect(),
        subtitles: subtitles
            .iter()
            .map(|subtitle| SynologySubtitleMetadata {
                id: subtitle.id.clone(),
                language: subtitle.lang.clone(),
                title: subtitle.title.clone(),
                format: subtitle.format.clone(),
                embedded: subtitle.embedded,
            })
            .collect(),
    };
    let default_mode =
        if super::playback_profile_prefers_transcode(playback_client_profile, &file.container_type)
        {
            "transcoded"
        } else {
            "raw"
        };
    Ok(PlaybackResult {
        playback_infos,
        default_mode: default_mode.to_string(),
        provider: crate::models::SourceProvider::Synology,
        provider_instance_name: auth.instance_name.clone(),
        duration_seconds: duration
            .map(|duration| std::time::Duration::from_secs(duration).as_secs_f64()),
        playback_kind: Some(crate::models::PlaybackKind::Regular),
        metadata: Some(PlaybackMetadata::Synology(metadata)),
    })
}

fn single_result(
    mode: &str,
    media: PlaybackMedia,
    subtitles: Vec<PlaybackSubtitle>,
    instance_name: Option<String>,
    duration_seconds: Option<f64>,
    is_live: Option<bool>,
    metadata: Option<PlaybackMetadata>,
) -> PlaybackResult {
    PlaybackResult {
        playback_infos: HashMap::from([(
            mode.to_string(),
            PlaybackInfo {
                thumbnail: None,
                medias: vec![media],
                default_media_index: Some(0),
                subtitles,
                default_subtitle_index: None,
                danmakus: Vec::new(),
                default_danmaku_index: None,
            },
        )]),
        default_mode: mode.to_string(),
        provider: crate::models::SourceProvider::Synology,
        provider_instance_name: instance_name,
        duration_seconds,
        playback_kind: is_live.map(|value| {
            if value {
                crate::models::PlaybackKind::Live
            } else {
                crate::models::PlaybackKind::Regular
            }
        }),
        metadata,
    }
}

fn mark_synology_playback_resources(
    result: &mut PlaybackResult,
    version: &str,
    expires_at: i64,
    proxy_mode: crate::models::PlaybackProxyMode,
    client_profile: Option<&super::PlaybackClientProfile>,
) {
    let original_default = result.default_mode.clone();
    let prefer_proxy = matches!(
        proxy_mode,
        crate::models::PlaybackProxyMode::Prefer | crate::models::PlaybackProxyMode::Only
    );
    let original_modes = result
        .playback_infos
        .iter()
        .map(|(name, info)| (name.clone(), info.clone()))
        .collect::<Vec<_>>();
    let mut generated = std::collections::HashMap::new();
    for (mode_name, original_info) in original_modes {
        if mode_name.starts_with("proxy_") {
            continue;
        }
        let selection = synology_route_selection(proxy_mode);
        let direct_available = original_info
            .medias
            .iter()
            .any(|media| !media.requires_provider_url());
        if selection.direct && direct_available {
            if let Some(direct_info) = super::build_direct_playback_info_for_client(
                &mode_name,
                &original_info,
                client_profile,
            ) {
                generated.insert(mode_name.clone(), direct_info);
            }
        }
        if !selection.proxy {
            continue;
        }
        let mut proxy_info = original_info.clone();
        let (proxy_medias, proxy_default_media_index) = super::map_playback_resources(
            &original_info.medias,
            original_info.default_media_index,
            |media_index, media| {
                if !super::proxy_playback_media_supported_by_client(
                    client_profile,
                    &mode_name,
                    media,
                ) {
                    return None;
                }
                let PlaybackMediaProvider::Synology(
                    crate::models::PlaybackSynologyMedia::Refresh {
                        credential_owner_id,
                        server_id,
                        resource,
                    },
                ) = &media.provider
                else {
                    return None;
                };
                let mut proxy = media.clone();
                proxy.provider =
                    PlaybackMediaProvider::Synology(crate::models::PlaybackSynologyMedia::Proxy {
                        version: version.to_string(),
                        expires_at,
                        mode_name: mode_name.clone(),
                        media_index,
                        credential_owner_id: credential_owner_id.clone(),
                        server_id: server_id.clone(),
                        resource: resource.clone(),
                    });
                Some(proxy)
            },
        );
        proxy_info.medias = proxy_medias;
        proxy_info.default_media_index = proxy_default_media_index;
        if proxy_info.medias.is_empty() {
            continue;
        }
        for (subtitle_index, subtitle) in proxy_info.subtitles.iter_mut().enumerate() {
            if let PlaybackSubtitleProvider::Synology(resource) = &mut subtitle.provider {
                match resource {
                    crate::models::PlaybackSynologySubtitle::File {
                        version: resource_version,
                        expires_at: resource_expires_at,
                        mode_name: resource_mode_name,
                        subtitle_index: resource_subtitle_index,
                        ..
                    }
                    | crate::models::PlaybackSynologySubtitle::VideoStation {
                        version: resource_version,
                        expires_at: resource_expires_at,
                        mode_name: resource_mode_name,
                        subtitle_index: resource_subtitle_index,
                        ..
                    } => {
                        *resource_version = version.to_string();
                        *resource_expires_at = expires_at;
                        resource_mode_name.clone_from(&mode_name);
                        *resource_subtitle_index = subtitle_index;
                    }
                }
            }
        }
        generated.insert(format!("proxy_{mode_name}"), proxy_info);
    }
    result.playback_infos = generated;
    super::select_generated_playback_default(result, &original_default, prefer_proxy);
}

fn synology_swarm_id(
    provider_instance_name: Option<&str>,
    resource_kind: &str,
    server_id: &str,
    resource: &str,
) -> String {
    super::provider_p2p_swarm_id(
        SynologyProvider::NAME,
        provider_instance_name,
        resource_kind,
        &format!("server:{server_id}:{resource}"),
    )
}

fn related_file_subtitle(media_path: &str, file: &SynologyFile) -> bool {
    !file.isdir
        && file_subtitle_format(&file.name).is_some()
        && related_file_stem(
            media_path.rsplit('/').next().unwrap_or_default(),
            &file.name,
        )
}

fn related_file_stem(media_name: &str, subtitle_name: &str) -> bool {
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

fn file_subtitle_format(name: &str) -> Option<&str> {
    let extension = name.rsplit_once('.')?.1;
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "srt" | "vtt" | "ass" | "ssa" | "sub" | "ttml"
    )
    .then_some(extension)
}

fn file_subtitle_language(media_path: &str, subtitle_name: &str) -> String {
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

fn file_metadata(file: &SynologyFile) -> PlaybackMediaMetadata {
    PlaybackMediaMetadata {
        resolution: None,
        bitrate: None,
        codec: (!file.additional.r#type.is_empty()).then(|| file.additional.r#type.clone()),
        fps: None,
    }
}

fn video_file_metadata(
    file: &synctv_media_providers::synology::SynologyVideoFile,
    profile: SynologyPlaybackProfile,
) -> PlaybackMediaMetadata {
    PlaybackMediaMetadata {
        resolution: Some(format!("{}x{}", file.resolutionx, file.resolutiony)),
        bitrate: i64::try_from(file.frame_bitrate).ok(),
        codec: video_station_profile_codec(&file.video_codec, profile),
        fps: (file.frame_rate_den > 0)
            .then(|| i32::try_from(file.frame_rate_num / file.frame_rate_den).ok())
            .flatten(),
    }
}

fn video_station_profile_codec(
    source_codec: &str,
    profile: SynologyPlaybackProfile,
) -> Option<String> {
    match profile {
        SynologyPlaybackProfile::HlsMedium | SynologyPlaybackProfile::HlsLow => {
            Some("h264".to_string())
        }
        SynologyPlaybackProfile::Raw | SynologyPlaybackProfile::HlsRemux => {
            (!source_codec.is_empty()).then(|| source_codec.to_string())
        }
    }
}

fn store_apis(apis: &SynologyApiMap) -> HashMap<String, SynologyApiBinding> {
    apis.iter()
        .map(|(name, api)| {
            (
                name.clone(),
                SynologyApiBinding {
                    path: api.path.clone(),
                    min_version: api.min_version,
                    max_version: api.max_version,
                },
            )
        })
        .collect()
}

fn load_apis(apis: &HashMap<String, SynologyApiBinding>) -> SynologyApiMap {
    apis.iter()
        .map(|(name, api)| {
            (
                name.clone(),
                SynologyApiInfo {
                    path: api.path.clone(),
                    min_version: api.min_version,
                    max_version: api.max_version,
                },
            )
        })
        .collect()
}

fn required_api<'a>(
    apis: &'a SynologyApiMap,
    name: &str,
) -> Result<&'a SynologyApiInfo, ProviderError> {
    apis.get(name).ok_or_else(|| {
        ProviderError::InvalidConfig(format!("Synology server does not provide {name}"))
    })
}

fn required_video_sid(auth: &AuthenticatedSynology) -> Result<&str, ProviderError> {
    auth.video_sid.as_deref().ok_or_else(|| {
        ProviderError::InvalidConfig("Synology Video Station is unavailable".to_string())
    })
}

fn normalize_file_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() || path == "/" {
        "/".to_string()
    } else {
        format!("/{}", path.trim_matches('/'))
    }
}

fn validate_file_path(path: &str) -> Result<(), ProviderError> {
    if path.trim().is_empty() || path.contains('\0') || path.contains("..") {
        return Err(ProviderError::InvalidConfig(
            "Synology file path is invalid".to_string(),
        ));
    }
    Ok(())
}

fn split_path(path: &str) -> Result<(&str, &str), ProviderError> {
    let path = path.trim_end_matches('/');
    let (parent, name) = path.rsplit_once('/').ok_or_else(|| {
        ProviderError::InvalidConfig("Synology file path must be absolute".to_string())
    })?;
    if name.is_empty() {
        return Err(ProviderError::InvalidConfig(
            "Synology file path has no file name".to_string(),
        ));
    }
    Ok((if parent.is_empty() { "/" } else { parent }, name))
}

const fn upstream_item_kind(kind: SynologyLibraryItemKind) -> SynologyVideoItemKind {
    match kind {
        SynologyLibraryItemKind::Movie => SynologyVideoItemKind::Movie,
        SynologyLibraryItemKind::Episode => SynologyVideoItemKind::Episode,
        SynologyLibraryItemKind::HomeVideo => SynologyVideoItemKind::HomeVideo,
        SynologyLibraryItemKind::TvRecording => SynologyVideoItemKind::TvRecording,
    }
}

const fn media_type(kind: SynologyLibraryItemKind) -> &'static str {
    match kind {
        SynologyLibraryItemKind::Movie => "movie",
        SynologyLibraryItemKind::Episode => "tvshow_episode",
        SynologyLibraryItemKind::HomeVideo => "home_video",
        SynologyLibraryItemKind::TvRecording => "tv_record",
    }
}

const fn video_item_api(kind: SynologyLibraryItemKind) -> &'static str {
    match kind {
        SynologyLibraryItemKind::Movie => "SYNO.VideoStation2.Movie",
        SynologyLibraryItemKind::Episode => "SYNO.VideoStation2.TVShowEpisode",
        SynologyLibraryItemKind::HomeVideo => "SYNO.VideoStation2.HomeVideo",
        SynologyLibraryItemKind::TvRecording => "SYNO.VideoStation2.TVRecording",
    }
}

const fn stream_profile(profile: SynologyPlaybackProfile) -> SynologyStreamProfile {
    match profile {
        SynologyPlaybackProfile::Raw => SynologyStreamProfile::Raw,
        SynologyPlaybackProfile::HlsRemux => SynologyStreamProfile::HlsRemux,
        SynologyPlaybackProfile::HlsMedium => SynologyStreamProfile::HlsMedium,
        SynologyPlaybackProfile::HlsLow => SynologyStreamProfile::HlsLow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media_item(index: usize) -> DynamicPlaylistItem {
        DynamicPlaylistItem {
            name: format!("Movie {index}"),
            item_type: ItemType::Media,
            target: ProviderTarget::synology_library_item(
                SynologyLibraryItemKind::Movie,
                i64::try_from(index).unwrap_or(i64::MAX),
                i64::try_from(index).unwrap_or(i64::MAX),
                None,
            ),
            size: None,
            thumbnail: None,
            description: None,
            modified_at: None,
            source_config: None,
            metadata: None,
        }
    }

    #[test]
    fn sequential_selection_continues_after_the_two_hundredth_item() {
        let media = (1..=250).map(media_item).collect::<Vec<_>>();
        let current = media[200].target.clone();

        assert!(!has_enough_media_for_next(
            &media[..200],
            &current,
            PlayMode::Sequential
        ));
        assert_eq!(
            select_next_media(&media, &current, PlayMode::Sequential)
                .map(|item| item.name.as_str()),
            Some("Movie 202")
        );
    }

    #[test]
    fn repeat_all_wraps_after_a_large_folder() {
        let media = (1..=250).map(media_item).collect::<Vec<_>>();
        let current = media.last().expect("last media").target.clone();

        assert_eq!(
            select_next_media(&media, &current, PlayMode::RepeatAll).map(|item| item.name.as_str()),
            Some("Movie 1")
        );
    }

    #[test]
    fn empty_file_playlist_path_resolves_to_root() {
        assert_eq!(join_path("", ""), "/");
        assert_eq!(join_path("/video", "Series"), "/video/Series");
    }

    #[test]
    fn file_station_subtitles_match_media_stem() {
        assert!(related_file_stem("Movie.mkv", "movie.zh-CN.ass"));
        assert!(related_file_stem("movie.mp4", "MOVIE_en.srt"));
        assert!(!related_file_stem("movie.mkv", "movie2.srt"));
        assert_eq!(file_subtitle_format("movie.zh-CN.ASS"), Some("ASS"));
        assert_eq!(file_subtitle_format("movie.txt"), None);
        assert_eq!(
            file_subtitle_language("/video/Movie.mkv", "Movie.zh-CN.ass"),
            "zh-CN"
        );
    }

    #[test]
    fn video_station_media_keeps_proxy_route_when_direct_is_preferred() {
        let media = PlaybackMedia {
            name: "Movie".to_string(),
            format: "hls".to_string(),
            expire_at: None,
            metadata: None,
            p2p_swarm_id: None,
            provider: PlaybackMediaProvider::Synology(
                crate::models::PlaybackSynologyMedia::Refresh {
                    credential_owner_id: "42".to_string(),
                    server_id: "synology-main".to_string(),
                    resource: SynologyPlaybackResource::VideoStation {
                        file_id: 7,
                        profile: SynologyPlaybackProfile::HlsRemux,
                        audio_track: None,
                        ac3_passthrough: true,
                    },
                },
            ),
        };
        let mut result =
            single_result("original", media, Vec::new(), None, None, Some(false), None);

        mark_synology_playback_resources(
            &mut result,
            "version",
            1_900_000_000,
            crate::models::PlaybackProxyMode::Prefer,
            None,
        );

        assert_eq!(result.default_mode, "proxy_original");
        assert_eq!(result.playback_infos.len(), 1);
        assert!(result.playback_infos.contains_key("proxy_original"));
    }

    #[test]
    fn video_station_transcode_profiles_advertise_h264() {
        assert_eq!(
            video_station_profile_codec("hevc", SynologyPlaybackProfile::Raw).as_deref(),
            Some("hevc")
        );
        assert_eq!(
            video_station_profile_codec("hevc", SynologyPlaybackProfile::HlsRemux).as_deref(),
            Some("hevc")
        );
        assert_eq!(
            video_station_profile_codec("hevc", SynologyPlaybackProfile::HlsMedium).as_deref(),
            Some("h264")
        );
        assert_eq!(
            video_station_profile_codec("hevc", SynologyPlaybackProfile::HlsLow).as_deref(),
            Some("h264")
        );
    }

    #[test]
    fn library_policy_excludes_direct_only_modes() {
        let provider = SynologyProvider::with_http_client(reqwest::Client::new());
        let mut source_config = MediaSourceConfig::Synology(SynologyMediaSourceConfig {
            server_id: "synology-main".to_string(),
            proxy_mode: crate::models::PlaybackProxyMode::Auto,
            source: SynologyMediaSource::LibraryItem {
                kind: SynologyLibraryItemKind::Movie,
                item_id: 7,
                file_id: 11,
            },
        });

        let policy = provider
            .playback_proxy_policy(SourceConfig::media(&source_config))
            .expect("Synology policy should resolve")
            .expect("Synology should expose a policy");

        assert_eq!(
            policy.supported_modes,
            vec![
                crate::models::PlaybackProxyMode::Auto,
                crate::models::PlaybackProxyMode::Prefer,
                crate::models::PlaybackProxyMode::Only,
            ]
        );
        assert_eq!(
            policy.auto_policies,
            vec![PlaybackProxyAutoPolicy::new(
                "library_item",
                crate::models::PlaybackProxyMode::Only,
                PlaybackProxyAutoReason::ProviderSession,
            )]
        );

        let error = provider
            .set_playback_proxy_mode(
                &mut source_config,
                crate::models::PlaybackProxyMode::DirectOnly,
            )
            .expect_err("Synology library items should reject direct-only mode");
        assert!(error.to_string().contains("does not support"));
    }
}
