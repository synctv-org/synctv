use std::sync::Arc;

use synctv_core::models::{SynologyPlaylistSource, UserId};
use synctv_core::provider::{
    ProviderError, SynologyProvider, SynologyVideoEntry, SynologyVideoEntryKind,
};
use synctv_proto::providers::synology::{
    BindInfo, FileItem, GetBindsResponse, ListEpisodesRequest, ListFilesRequest, ListFilesResponse,
    ListHomeVideosRequest, ListLibrariesRequest, ListLibrariesResponse, ListMoviesRequest,
    ListTvRecordingsRequest, ListTvShowsRequest, ListVideoItemsResponse, LoginRequest,
    LoginResponse, LogoutRequest, LogoutResponse, SynologyVideoEntryKind as ProtoKind, VideoFile,
    VideoItem, VideoLibrary,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    provider_instance_name_for_response, publish_provider_credential_changed,
    resolve_bound_instance_name,
};

#[derive(Clone)]
pub struct SynologyApiImpl {
    provider: Arc<SynologyProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl SynologyApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<SynologyProvider>,
        event_service: Arc<dyn RealtimeEventService>,
    ) -> Self {
        Self {
            provider,
            event_service,
        }
    }

    pub async fn login(
        &self,
        user_id: UserId,
        req: LoginRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginResponse, ProviderError> {
        let (server_id, video_station_available) = self
            .provider
            .login_and_persist(
                user_id,
                req.endpoint,
                req.username,
                req.password,
                req.otp_code,
                req.device_name,
                instance_name.map(str::to_string),
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            SynologyProvider::NAME,
            &server_id,
        );
        Ok(LoginResponse {
            server_id,
            video_station_available,
        })
    }

    pub async fn list_files(
        &self,
        user_id: UserId,
        req: ListFilesRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListFilesResponse, ProviderError> {
        let page = page(req.page)?;
        let page_size = page_size(req.page_size)?;
        let (listing, stored_instance_name) = self
            .provider
            .list_files(
                user_id,
                &req.server_id,
                &req.path,
                page,
                page_size,
                req.search.as_deref(),
            )
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(ListFilesResponse {
            items: listing
                .files
                .into_iter()
                .map(|item| FileItem {
                    name: item.name,
                    path: item.path,
                    is_dir: item.isdir,
                    size: item.additional.size,
                    modified_at: item.additional.time.mtime,
                    created_at: item.additional.time.crtime,
                    file_type: item.additional.r#type,
                })
                .collect(),
            total: listing.total,
            page: req.page,
            has_more: listing
                .offset
                .saturating_add(u64::try_from(page_size).unwrap_or(u64::MAX))
                < listing.total,
        })
    }

    pub async fn list_libraries(
        &self,
        user_id: UserId,
        req: ListLibrariesRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListLibrariesResponse, ProviderError> {
        let (listing, stored_instance_name) = self
            .provider
            .list_video_libraries(user_id, &req.server_id)
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(ListLibrariesResponse {
            libraries: listing
                .libraries
                .into_iter()
                .map(|library| VideoLibrary {
                    id: library.id,
                    title: library.title,
                    library_type: library.library_type,
                    is_public: library.is_public,
                    visible: library.visible,
                })
                .collect(),
        })
    }

    pub async fn list_movies(
        &self,
        user_id: UserId,
        req: ListMoviesRequest,
        instance_name: Option<&str>,
    ) -> Result<ListVideoItemsResponse, ProviderError> {
        self.list_video_items(
            user_id,
            &req.server_id,
            SynologyPlaylistSource::Movies {
                library_id: req.library_id,
            },
            req.page,
            req.page_size,
            req.search.as_deref(),
            instance_name,
        )
        .await
    }

    pub async fn list_tv_shows(
        &self,
        user_id: UserId,
        req: ListTvShowsRequest,
        instance_name: Option<&str>,
    ) -> Result<ListVideoItemsResponse, ProviderError> {
        self.list_video_items(
            user_id,
            &req.server_id,
            SynologyPlaylistSource::TvShows {
                library_id: req.library_id,
            },
            req.page,
            req.page_size,
            req.search.as_deref(),
            instance_name,
        )
        .await
    }

    pub async fn list_episodes(
        &self,
        user_id: UserId,
        req: ListEpisodesRequest,
        instance_name: Option<&str>,
    ) -> Result<ListVideoItemsResponse, ProviderError> {
        self.list_video_items(
            user_id,
            &req.server_id,
            SynologyPlaylistSource::Episodes {
                library_id: req.library_id,
                tv_show_id: req.tv_show_id,
            },
            req.page,
            req.page_size,
            req.search.as_deref(),
            instance_name,
        )
        .await
    }

    pub async fn list_home_videos(
        &self,
        user_id: UserId,
        req: ListHomeVideosRequest,
        instance_name: Option<&str>,
    ) -> Result<ListVideoItemsResponse, ProviderError> {
        self.list_video_items(
            user_id,
            &req.server_id,
            SynologyPlaylistSource::HomeVideos {
                library_id: req.library_id,
            },
            req.page,
            req.page_size,
            req.search.as_deref(),
            instance_name,
        )
        .await
    }

    pub async fn list_tv_recordings(
        &self,
        user_id: UserId,
        req: ListTvRecordingsRequest,
        instance_name: Option<&str>,
    ) -> Result<ListVideoItemsResponse, ProviderError> {
        self.list_video_items(
            user_id,
            &req.server_id,
            SynologyPlaylistSource::TvRecordings {
                library_id: req.library_id,
            },
            req.page,
            req.page_size,
            req.search.as_deref(),
            instance_name,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn list_video_items(
        &self,
        user_id: UserId,
        server_id: &str,
        source: SynologyPlaylistSource,
        requested_page: u64,
        requested_page_size: u32,
        search: Option<&str>,
        requested_instance_name: Option<&str>,
    ) -> Result<ListVideoItemsResponse, ProviderError> {
        let (listing, stored_instance_name) = self
            .provider
            .list_video_items(
                user_id,
                server_id,
                source,
                page(requested_page)?,
                page_size(requested_page_size)?,
                search,
            )
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(ListVideoItemsResponse {
            items: listing.items.into_iter().map(video_item).collect(),
            total: listing.total,
            page: u64::try_from(listing.page).unwrap_or(u64::MAX),
            has_more: listing.has_more,
        })
    }

    pub async fn logout(
        &self,
        user_id: UserId,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, ProviderError> {
        let success = self
            .provider
            .logout_and_delete(user_id, &req.server_id)
            .await?;
        if success {
            publish_provider_credential_changed(
                &self.event_service,
                user_id,
                SynologyProvider::NAME,
                &req.server_id,
            );
        }
        Ok(LogoutResponse { success })
    }

    pub async fn binds(
        &self,
        user_id: UserId,
        requested_instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, ProviderError> {
        let binds = self
            .provider
            .list_binds(user_id)
            .await?
            .into_iter()
            .filter(|bind| {
                requested_instance_name
                    .is_none_or(|name| bind.provider_instance_name.as_deref() == Some(name))
            })
            .map(|bind| BindInfo {
                id: bind.id.to_string(),
                server_id: bind.server_id,
                endpoint: bind.endpoint,
                username: bind.username,
                video_station_available: bind.video_station_available,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }

    pub async fn file_thumbnail_action(
        &self,
        user_id: UserId,
        server_id: &str,
        path: &str,
        size: &str,
    ) -> Result<synctv_core::provider::PlaybackTransportAction, ProviderError> {
        self.provider
            .file_thumbnail_action(user_id, server_id, path, size)
            .await
    }

    pub async fn poster_action(
        &self,
        user_id: UserId,
        server_id: &str,
        item_id: i64,
        media_type: &str,
        poster_mtime: Option<&str>,
    ) -> Result<synctv_core::provider::PlaybackTransportAction, ProviderError> {
        self.provider
            .poster_action(user_id, server_id, item_id, media_type, poster_mtime)
            .await
    }
}

fn page(value: u64) -> Result<usize, ProviderError> {
    usize::try_from(value.max(1))
        .map_err(|_| ProviderError::InvalidConfig("Synology page exceeds usize::MAX".to_string()))
}

fn page_size(value: u32) -> Result<usize, ProviderError> {
    usize::try_from(value.clamp(1, 200)).map_err(|_| {
        ProviderError::InvalidConfig("Synology page_size exceeds usize::MAX".to_string())
    })
}

fn video_item(entry: SynologyVideoEntry) -> VideoItem {
    let metadata = entry.metadata;
    VideoItem {
        id: metadata.id,
        library_id: metadata.library_id,
        kind: match entry.kind {
            SynologyVideoEntryKind::Movie => ProtoKind::Movie,
            SynologyVideoEntryKind::TvShow => ProtoKind::TvShow,
            SynologyVideoEntryKind::Episode => ProtoKind::Episode,
            SynologyVideoEntryKind::HomeVideo => ProtoKind::HomeVideo,
            SynologyVideoEntryKind::TvRecording => ProtoKind::TvRecording,
        }
        .into(),
        title: metadata.title,
        sort_title: metadata.sort_title,
        tagline: metadata.tagline,
        summary: metadata.additional.summary,
        certificate: metadata.certificate,
        rating: metadata.rating,
        actors: metadata.additional.actor,
        directors: metadata.additional.director,
        writers: metadata.additional.writer,
        genres: metadata.additional.genre,
        original_available: metadata.original_available,
        create_time: metadata.create_time,
        last_watched: metadata.last_watched,
        watched_ratio: metadata.additional.watched_ratio,
        parental_controlled: metadata.additional.is_parental_controlled,
        season: entry.season,
        episode: entry.episode,
        tv_show_id: entry.tv_show_id,
        poster_mtime: metadata.additional.poster_mtime,
        backdrop_mtime: metadata.additional.backdrop_mtime,
        files: metadata
            .additional
            .file
            .into_iter()
            .map(|file| VideoFile {
                id: file.id,
                duration_seconds: file.duration_seconds().unwrap_or_default(),
                path: file.path,
                size: file.filesize,
                progress_seconds: file.position,
                width: file.resolutionx.max(file.display_x),
                height: file.resolutiony.max(file.display_y),
                video_codec: file.video_codec,
                audio_codec: file.audio_codec,
                container: file.container_type,
                video_bitrate: file.video_bitrate,
                audio_bitrate: file.audio_bitrate,
                frame_rate_numerator: file.frame_rate_num,
                frame_rate_denominator: file.frame_rate_den,
                audio_channels: file.channel,
                audio_frequency_hz: file.frequency,
                conversion_produced: file.conversion_produced,
            })
            .collect(),
    }
}
