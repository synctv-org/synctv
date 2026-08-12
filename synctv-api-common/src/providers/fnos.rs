use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::{FnosLoginResult, FnosProvider, ProviderError};
use synctv_proto::providers::fnos::{
    login_response, Authenticated, BindInfo, FileItem, GetBindsResponse, GetServerInfoRequest,
    GetServerInfoResponse, ListMediaItemsRequest, ListMediaItemsResponse,
    ListMediaLibrariesRequest, ListMediaLibrariesResponse, ListRequest, ListResponse, LoginRequest,
    LoginResponse, LogoutRequest, LogoutResponse, MediaCollection, MediaItem, MediaLibrary,
    SetFavoriteRequest, SetFavoriteResponse, SetWatchedRequest, SetWatchedResponse,
    TwoFactorRequired,
};
use synctv_proto::source_config::{
    fnos_media_source_config, fnos_playlist_source_config, media_source_config,
    playlist_source_config, FnosFavoritesPlaylistSourceConfig, FnosFileSourceConfig,
    FnosFilesPlaylistSourceConfig, FnosHistoryPlaylistSourceConfig, FnosLibraryItemSourceConfig,
    FnosMediaLibraryPlaylistSourceConfig, FnosMediaSourceConfig, FnosPlaylistSourceConfig,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    discovered_media, discovered_playlist, provider_instance_name_for_response,
    publish_provider_credential_changed, resolve_bound_instance_name,
};

#[derive(Clone)]
pub struct FnosApiImpl {
    provider: Arc<FnosProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl FnosApiImpl {
    #[must_use]
    pub fn new(provider: Arc<FnosProvider>, event_service: Arc<dyn RealtimeEventService>) -> Self {
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
        let result = self
            .provider
            .login_and_persist(
                user_id,
                req.endpoint,
                req.webdav_endpoint,
                req.media_endpoint,
                req.username,
                req.password,
                req.twofa_code,
                req.trust_device,
                instance_name.map(ToString::to_string),
            )
            .await?;
        let result = match result {
            FnosLoginResult::Authenticated {
                server_id,
                server,
                media_available,
            } => {
                publish_provider_credential_changed(
                    &self.event_service,
                    user_id,
                    synctv_core::models::SourceProvider::Fnos,
                    &server_id,
                );
                login_response::Result::Authenticated(Authenticated {
                    server_id,
                    host_name: server.host_name,
                    version: server.version,
                    media_available,
                })
            }
            FnosLoginResult::TwoFactorRequired { setup_required } => {
                login_response::Result::TwoFactorRequired(TwoFactorRequired { setup_required })
            }
        };
        Ok(LoginResponse {
            result: Some(result),
        })
    }

    pub async fn image_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        image_path: &str,
        width: u32,
    ) -> Result<synctv_core::provider::PlaybackTransportAction, ProviderError> {
        self.provider
            .image_action(credential_owner_id, server_id, image_path, width)
            .await
    }

    pub async fn list(
        &self,
        user_id: UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let (listing, stored_instance_name) = self
            .provider
            .list(user_id, &req.server_id, &req.path)
            .await?;
        let instance_name =
            resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;

        let search = req
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_ascii_lowercase);
        let mut files = listing
            .files
            .into_iter()
            .filter(|file| {
                search
                    .as_ref()
                    .is_none_or(|search| file.name.to_ascii_lowercase().contains(search))
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| {
            (!left.is_dir, left.name.to_ascii_lowercase())
                .cmp(&(!right.is_dir, right.name.to_ascii_lowercase()))
        });

        let total = u64::try_from(files.len()).unwrap_or(u64::MAX);
        let page = usize::try_from(req.page.max(1)).unwrap_or(usize::MAX);
        let page_size = usize::try_from(req.page_size.max(1)).unwrap_or(usize::MAX);
        let start = page.saturating_sub(1).saturating_mul(page_size);
        let end = start.saturating_add(page_size).min(files.len());
        let content = files
            .get(start..end)
            .unwrap_or_default()
            .iter()
            .cloned()
            .map(|file| file_item(file, &req.server_id, instance_name.as_deref()))
            .collect();
        Ok(ListResponse {
            content,
            total,
            page: req.page.max(1),
            has_more: end < files.len(),
            source: Some(fnos_file_source(
                &req.server_id,
                &req.path,
                true,
                instance_name.as_deref(),
            )),
        })
    }

    pub async fn get_server_info(
        &self,
        user_id: UserId,
        req: GetServerInfoRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<GetServerInfoResponse, ProviderError> {
        let (server, stored_instance_name) =
            self.provider.server_info(user_id, &req.server_id).await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(GetServerInfoResponse {
            host_name: server.host_name,
            version: server.version,
        })
    }

    pub async fn list_media_libraries(
        &self,
        user_id: UserId,
        req: ListMediaLibrariesRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListMediaLibrariesResponse, ProviderError> {
        let server_id = req.server_id.clone();
        let (libraries, stored_instance_name) = self
            .provider
            .media_libraries(user_id, &req.server_id)
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(ListMediaLibrariesResponse {
            libraries: libraries
                .into_iter()
                .map(|library| {
                    let poster = library.poster.map(|path| {
                        crate::fnos_thumbnail_urls::fnos_own_thumbnail_url(&server_id, &path, 400)
                    });
                    let posters = library
                        .posters
                        .into_iter()
                        .map(|path| {
                            crate::fnos_thumbnail_urls::fnos_own_thumbnail_url(
                                &server_id, &path, 400,
                            )
                        })
                        .collect();
                    MediaLibrary {
                        guid: library.guid,
                        title: library.title,
                        poster,
                        posters,
                        category: library.category,
                        view_type: library.view_type,
                        poster_type: library.poster_type,
                    }
                })
                .collect(),
        })
    }

    pub async fn list_media_items(
        &self,
        user_id: UserId,
        req: ListMediaItemsRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListMediaItemsResponse, ProviderError> {
        let server_id = req.server_id.clone();
        let page = req.page.max(1);
        let page_size = req.page_size.clamp(1, 200);
        let search = req
            .search
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let collection = MediaCollection::try_from(req.collection).map_err(|_| {
            ProviderError::InvalidConfig("FNOS media collection is invalid".to_string())
        })?;
        let request = synctv_media_providers::fnos::FnosMediaListRequest {
            ancestor_guid: req.ancestor_guid.clone(),
            exclude_grouped_video: 1,
            sort_type: match collection {
                MediaCollection::Favorites | MediaCollection::History => "DESC",
                MediaCollection::Library | MediaCollection::Unspecified => "ASC",
            }
            .to_string(),
            sort_column: match collection {
                MediaCollection::Favorites | MediaCollection::History => "create_time",
                MediaCollection::Library | MediaCollection::Unspecified => "title",
            }
            .to_string(),
            page_size,
            page,
            tags: synctv_media_providers::fnos::FnosMediaTags {
                media_types: req.media_types.clone(),
            },
        };
        let (items, total, stored_instance_name) = if collection == MediaCollection::History {
            let (mut items, instance_name) =
                self.provider.media_history(user_id, &req.server_id).await?;
            retain_media_items(&mut items, &req, search, false);
            paginate_media_items(items, page, page_size, instance_name)
        } else if collection == MediaCollection::Favorites
            && req.ancestor_guid.is_none()
            && search.is_none()
        {
            let (response, instance_name) = self
                .provider
                .favorite_media_items(user_id, &req.server_id, &request)
                .await?;
            (response.list, response.total, instance_name)
        } else if let Some(search) = search {
            let (mut items, instance_name) = self
                .provider
                .search_media(user_id, &req.server_id, search)
                .await?;
            retain_media_items(
                &mut items,
                &req,
                Some(search),
                collection == MediaCollection::Favorites,
            );
            paginate_media_items(items, page, page_size, instance_name)
        } else {
            let (response, instance_name) = self
                .provider
                .media_items(user_id, &req.server_id, &request)
                .await?;
            (response.list, response.total, instance_name)
        };
        let instance_name =
            resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        let page_source = fnos_media_playlist_source(
            &server_id,
            collection,
            req.ancestor_guid.as_deref(),
            &req.media_types,
            instance_name.as_deref(),
        );
        Ok(ListMediaItemsResponse {
            items: items
                .into_iter()
                .map(|item| {
                    let title = item.display_title();
                    let is_folder = item.is_folder();
                    let is_playable = item.is_playable();
                    let source = if is_folder {
                        Some(fnos_media_playlist_source(
                            &server_id,
                            MediaCollection::Library,
                            Some(&item.guid),
                            &req.media_types,
                            instance_name.as_deref(),
                        ))
                    } else if is_playable {
                        Some(discovered_media(
                            media_source_config::Provider::Fnos(FnosMediaSourceConfig {
                                server_id: server_id.clone(),
                                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto
                                    as i32,
                                source: Some(fnos_media_source_config::Source::LibraryItem(
                                    FnosLibraryItemSourceConfig {
                                        item_guid: item.guid.clone(),
                                        media_guid: item.media_guid.clone(),
                                    },
                                )),
                            }),
                            instance_name.as_deref(),
                        ))
                    } else {
                        None
                    };
                    MediaItem {
                        guid: item.guid,
                        title,
                        item_type: item.item_type,
                        poster: item.poster.map(|path| {
                            crate::fnos_thumbnail_urls::fnos_own_thumbnail_url(
                                &server_id, &path, 400,
                            )
                        }),
                        media_guid: item.media_guid,
                        parent_guid: item.parent_guid,
                        overview: item.overview,
                        duration_seconds: item.duration,
                        progress_seconds: item.ts,
                        watched: item.watched != 0,
                        season_number: item.season_number,
                        episode_number: item.episode_number,
                        is_folder,
                        is_playable,
                        favorite: item.is_favorite != 0,
                        source,
                    }
                })
                .collect(),
            total,
            page,
            has_more: u64::from(page).saturating_mul(u64::from(page_size)) < total,
            source: Some(page_source),
        })
    }

    pub async fn set_favorite(
        &self,
        user_id: UserId,
        req: SetFavoriteRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<SetFavoriteResponse, ProviderError> {
        let (success, stored_instance_name) = self
            .provider
            .set_media_favorite(user_id, &req.server_id, &req.item_guid, req.favorite)
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(SetFavoriteResponse { success })
    }

    pub async fn set_watched(
        &self,
        user_id: UserId,
        req: SetWatchedRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<SetWatchedResponse, ProviderError> {
        let (success, stored_instance_name) = self
            .provider
            .set_media_watched(user_id, &req.server_id, &req.item_guid, req.watched)
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(SetWatchedResponse { success })
    }

    pub async fn logout(
        &self,
        user_id: UserId,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, ProviderError> {
        let success = self
            .provider
            .delete_credential(user_id, &req.server_id)
            .await?;
        if success {
            publish_provider_credential_changed(
                &self.event_service,
                user_id,
                synctv_core::models::SourceProvider::Fnos,
                &req.server_id,
            );
        }
        Ok(LogoutResponse { success })
    }

    pub async fn get_binds(
        &self,
        user_id: UserId,
        instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, ProviderError> {
        let binds = self
            .provider
            .list_binds(user_id, instance_name)
            .await?
            .into_iter()
            .map(|bind| BindInfo {
                id: bind.id.to_string(),
                server_id: bind.server_id,
                endpoint: bind.endpoint,
                webdav_endpoint: bind.webdav_endpoint,
                media_endpoint: bind.media_endpoint,
                username: bind.username,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
                media_available: bind.media_available,
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }
}

fn file_item(
    file: synctv_media_providers::fnos::FnosFile,
    server_id: &str,
    provider_instance_name: Option<&str>,
) -> FileItem {
    let source = fnos_file_source(server_id, &file.path, file.is_dir, provider_instance_name);
    FileItem {
        name: file.name,
        path: file.path,
        size: file.size,
        modified_at: file.modified_at,
        created_at: file.created_at,
        is_dir: file.is_dir,
        storage_id: file.storage_id,
        source: Some(source),
    }
}

fn fnos_file_source(
    server_id: &str,
    path: &str,
    playlist: bool,
    provider_instance_name: Option<&str>,
) -> synctv_proto::providers::common::DiscoveredSource {
    if playlist {
        discovered_playlist(
            playlist_source_config::Provider::Fnos(FnosPlaylistSourceConfig {
                server_id: server_id.to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(fnos_playlist_source_config::Source::Files(
                    FnosFilesPlaylistSourceConfig {
                        path: path.to_string(),
                    },
                )),
            }),
            provider_instance_name,
        )
    } else {
        discovered_media(
            media_source_config::Provider::Fnos(FnosMediaSourceConfig {
                server_id: server_id.to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(fnos_media_source_config::Source::File(
                    FnosFileSourceConfig {
                        path: path.to_string(),
                    },
                )),
            }),
            provider_instance_name,
        )
    }
}

fn fnos_media_playlist_source(
    server_id: &str,
    collection: MediaCollection,
    ancestor_guid: Option<&str>,
    media_types: &[String],
    provider_instance_name: Option<&str>,
) -> synctv_proto::providers::common::DiscoveredSource {
    let source = match collection {
        MediaCollection::Favorites => {
            fnos_playlist_source_config::Source::Favorites(FnosFavoritesPlaylistSourceConfig {
                media_types: media_types.to_vec(),
            })
        }
        MediaCollection::History => {
            fnos_playlist_source_config::Source::History(FnosHistoryPlaylistSourceConfig {})
        }
        MediaCollection::Library | MediaCollection::Unspecified => {
            fnos_playlist_source_config::Source::MediaLibrary(
                FnosMediaLibraryPlaylistSourceConfig {
                    ancestor_guid: ancestor_guid.map(str::to_string),
                    media_types: media_types.to_vec(),
                },
            )
        }
    };
    discovered_playlist(
        playlist_source_config::Provider::Fnos(FnosPlaylistSourceConfig {
            server_id: server_id.to_string(),
            proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
            source: Some(source),
        }),
        provider_instance_name,
    )
}

fn media_item_matches_search(
    item: &synctv_media_providers::fnos::FnosMediaItem,
    lowercase_search: &str,
) -> bool {
    item.display_title()
        .to_ascii_lowercase()
        .contains(lowercase_search)
}

fn retain_media_items(
    items: &mut Vec<synctv_media_providers::fnos::FnosMediaItem>,
    req: &ListMediaItemsRequest,
    search: Option<&str>,
    require_favorite: bool,
) {
    let lowercase_search = search.map(str::to_ascii_lowercase);
    items.retain(|item| {
        let in_source = req.ancestor_guid.as_deref().is_none_or(|ancestor| {
            item.parent_guid.as_deref() == Some(ancestor)
                || item.ancestor_guid.as_deref() == Some(ancestor)
        });
        let has_type = req.media_types.is_empty()
            || req
                .media_types
                .iter()
                .any(|item_type| item.item_type.eq_ignore_ascii_case(item_type));
        let has_search = lowercase_search
            .as_deref()
            .is_none_or(|search| media_item_matches_search(item, search));
        let in_collection = !require_favorite || item.is_favorite != 0;
        in_source && has_type && has_search && in_collection
    });
}

fn paginate_media_items(
    items: Vec<synctv_media_providers::fnos::FnosMediaItem>,
    page: u32,
    page_size: u32,
    instance_name: Option<String>,
) -> (
    Vec<synctv_media_providers::fnos::FnosMediaItem>,
    u64,
    Option<String>,
) {
    let total = u64::try_from(items.len()).unwrap_or(u64::MAX);
    let start =
        usize::try_from(page.saturating_sub(1).saturating_mul(page_size)).unwrap_or(usize::MAX);
    let items = items
        .into_iter()
        .skip(start)
        .take(usize::try_from(page_size).unwrap_or(usize::MAX))
        .collect();
    (items, total, instance_name)
}
