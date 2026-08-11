//! Emby API Implementation
//!
//! Unified implementation for all Emby API operations.
//! Used by both HTTP and gRPC handlers.

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use std::sync::Arc;
use synctv_core::models::{EmbyPlaylistSource, UserId};
use synctv_core::provider::{
    EmbyListRequest, EmbyMeRequest, EmbyProvider, ExecutionControl, ProviderAccessService,
};
use synctv_proto::providers::emby::{
    BindInfo, GetBindsResponse, GetMeRequest, GetMeResponse, ListMode, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, MediaItem,
};
use synctv_proto::source_config::{
    emby_playlist_source_config, media_source_config, playlist_source_config,
    EmbyCollectionsPlaylistSource, EmbyContinueWatchingPlaylistSource,
    EmbyFavoriteItemsPlaylistSource, EmbyFavoritePeoplePlaylistSource, EmbyFolderPlaylistSource,
    EmbyGenreItemsPlaylistSource, EmbyGenresPlaylistSource, EmbyMediaSourceConfig,
    EmbyNextUpPlaylistSource, EmbyPersonItemsPlaylistSource, EmbyPlaylistSourceConfig,
    EmbyPlaylistsPlaylistSource, EmbyRecentlyAddedPlaylistSource,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::ProviderApiRuntime;
use super::{
    discovered_media, discovered_playlist, provider_instance_name_for_response,
    publish_provider_credential_changed, resolve_bound_instance_name,
};

fn emby_thumbnail_url(server_id: &str, credential_owner_id: &UserId, item_id: &str) -> String {
    format!(
        "/api/providers/emby/thumbnail/{item_id}?serverId={server_id}&credentialOwnerId={credential_owner_id}&maxHeight=300",
        item_id = utf8_percent_encode(item_id, NON_ALPHANUMERIC),
        server_id = utf8_percent_encode(server_id, NON_ALPHANUMERIC),
        credential_owner_id = utf8_percent_encode(&credential_owner_id.to_string(), NON_ALPHANUMERIC),
    )
}

/// Emby API implementation
///
/// Contains all business logic for Emby operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct EmbyApiImpl {
    provider: Arc<EmbyProvider>,
    access_service: Arc<dyn ProviderAccessService>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl EmbyApiImpl {
    #[must_use]
    pub fn new_with_runtime(provider: Arc<EmbyProvider>, runtime: ProviderApiRuntime) -> Self {
        Self {
            provider,
            access_service: runtime.access_service,
            event_service: runtime.event_service,
        }
    }

    /// Resolve Emby credentials from DB using server_id, returning (host, api_key, emby_user_id).
    async fn resolve_credentials(
        &self,
        caller_user_id: &UserId,
        server_id: &str,
        request_context: Option<&ExecutionControl>,
    ) -> Result<(String, String, String, Option<String>), synctv_core::provider::ProviderError>
    {
        let access = self
            .access_service
            .emby_access(*caller_user_id, server_id, None, request_context)
            .await?;
        Ok((
            access.host,
            access.api_key,
            access.emby_user_id,
            access.provider_instance_name,
        ))
    }

    pub async fn login_with_context(
        &self,
        caller_user_id: &UserId,
        req: LoginRequest,
        instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let host = req.host.clone();
        let (password_input, api_key_input) = match req.credential {
            Some(synctv_proto::providers::emby::login_request::Credential::Password(password)) => {
                (Some(password), None)
            }
            Some(synctv_proto::providers::emby::login_request::Credential::ApiKey(api_key)) => {
                (None, Some(api_key))
            }
            None => (None, None),
        };
        let persisted = self
            .provider
            .login_and_persist_with_context(
                synctv_core::provider::EmbyLoginAndPersistRequest {
                    user_id: *caller_user_id,
                    host,
                    username: req.username,
                    password: password_input,
                    api_key: api_key_input,
                    provider_instance_name: instance_name.map(ToString::to_string),
                },
                request_context,
            )
            .await?;
        let login_resp = persisted.login;

        // Extract admin status from user policy
        let is_admin = login_resp
            .policy
            .as_ref()
            .is_some_and(|p| p.is_administrator);

        self.access_service
            .invalidate(
                *caller_user_id,
                synctv_core::provider::EmbyProvider::NAME,
                &persisted.server_id,
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            *caller_user_id,
            synctv_core::provider::EmbyProvider::NAME,
            &persisted.server_id,
        );

        Ok(LoginResponse {
            user_id: login_resp.user_id,
            username: login_resp.username,
            is_admin,
            server_id: persisted.server_id,
        })
    }

    pub async fn list_with_context(
        &self,
        caller_user_id: &UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        let server_id = req.server_id.clone();
        let source = emby_list_source(req.mode(), &req.target_id, &req.item_types)?;
        let requested_item_types = req.item_types.clone();
        let (host, token, user_id, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id, request_context)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let list_req = EmbyListRequest {
            host,
            token,
            source: source.clone(),
            start_index: req.start_index,
            limit: req.limit,
            search_term: req.search_term,
            user_id,
        };

        let resp = self
            .provider
            .fs_list_with_context(
                list_req,
                effective_instance_name.as_deref(),
                request_context,
            )
            .await?;

        let items: Vec<MediaItem> = resp
            .items
            .into_iter()
            .filter_map(|item| {
                let is_container = item.is_folder;
                let thumbnail = if item.has_thumbnail {
                    emby_thumbnail_url(&req.server_id, caller_user_id, &item.id)
                } else {
                    String::new()
                };
                let discovered_source = emby_listing_item_source(
                    &server_id,
                    &source,
                    &item.id,
                    &item.item_type,
                    is_container,
                    &requested_item_types,
                    effective_instance_name.as_deref(),
                )?;
                Some(MediaItem {
                    thumbnail,
                    source: Some(discovered_source),
                    id: item.id,
                    name: item.name,
                    r#type: item.item_type,
                    parent_id: item.parent_id,
                    series_name: item.series_name,
                    series_id: item.series_id,
                    season_name: item.season_name,
                    description: item.description,
                    is_container,
                })
            })
            .collect();

        Ok(ListResponse {
            items,
            total: resp.total,
            source: Some(emby_playlist_source(
                &server_id,
                &source,
                effective_instance_name.as_deref(),
            )),
        })
    }

    pub async fn get_me_with_context(
        &self,
        caller_user_id: &UserId,
        req: GetMeRequest,
        requested_instance_name: Option<&str>,
        request_context: Option<&ExecutionControl>,
    ) -> Result<GetMeResponse, synctv_core::provider::ProviderError> {
        let (host, token, user_id, credential_instance_name) = self
            .resolve_credentials(caller_user_id, &req.server_id, request_context)
            .await?;
        let effective_instance_name = resolve_bound_instance_name(
            requested_instance_name,
            credential_instance_name.as_deref(),
        )?;

        let me_req = EmbyMeRequest {
            host,
            token,
            user_id,
        };

        let resp = self
            .provider
            .me_with_context(me_req, effective_instance_name.as_deref(), request_context)
            .await?;

        Ok(GetMeResponse {
            id: resp.id,
            name: resp.name,
        })
    }

    /// Logout and delete stored credential
    pub async fn logout(
        &self,
        caller_user_id: &UserId,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        if req.server_id.trim().is_empty() {
            return Err(synctv_core::provider::ProviderError::InvalidConfig(
                "Emby logout requires an explicit server_id".to_string(),
            ));
        }

        if self
            .provider
            .delete_credential(*caller_user_id, &req.server_id)
            .await?
        {
            self.access_service
                .invalidate(
                    *caller_user_id,
                    synctv_core::provider::EmbyProvider::NAME,
                    &req.server_id,
                )
                .await?;
            publish_provider_credential_changed(
                &self.event_service,
                *caller_user_id,
                synctv_core::provider::EmbyProvider::NAME,
                &req.server_id,
            );
        }

        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }

    pub async fn get_binds(
        &self,
        caller_user_id: &UserId,
        instance_name: Option<&str>,
    ) -> Result<GetBindsResponse, crate::impls::ApiError> {
        let binds = self
            .provider
            .list_binds(*caller_user_id, instance_name)
            .await?
            .into_iter()
            .map(|bind| BindInfo {
                id: bind.id.to_string(),
                server_id: bind.server_id,
                host: bind.host,
                user_id: bind.emby_user_id,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();

        Ok(GetBindsResponse { binds })
    }
}

fn emby_list_source(
    mode: ListMode,
    target_id: &str,
    item_types: &[String],
) -> Result<EmbyPlaylistSource, synctv_core::provider::ProviderError> {
    let target_id = target_id.trim();
    let item_types = item_types
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let require_target = |kind: &str| {
        (!target_id.is_empty())
            .then(|| target_id.to_string())
            .ok_or_else(|| {
                synctv_core::provider::ProviderError::InvalidConfig(format!(
                    "Emby {kind} target_id must not be empty"
                ))
            })
    };
    Ok(match mode {
        ListMode::Folder => EmbyPlaylistSource::Folder {
            item_id: target_id.to_string(),
        },
        ListMode::FavoriteItems => EmbyPlaylistSource::FavoriteItems { item_types },
        ListMode::FavoritePeople => EmbyPlaylistSource::FavoritePeople,
        ListMode::PersonItems => EmbyPlaylistSource::PersonItems {
            person_id: require_target("person")?,
            item_types,
        },
        ListMode::ContinueWatching => EmbyPlaylistSource::ContinueWatching,
        ListMode::NextUp => EmbyPlaylistSource::NextUp,
        ListMode::RecentlyAdded => EmbyPlaylistSource::RecentlyAdded { item_types },
        ListMode::Playlists => EmbyPlaylistSource::Playlists,
        ListMode::Collections => EmbyPlaylistSource::Collections,
        ListMode::Genres => EmbyPlaylistSource::Genres { item_types },
        ListMode::GenreItems => EmbyPlaylistSource::GenreItems {
            genre_id: require_target("genre")?,
            item_types,
        },
    })
}

fn emby_listing_item_source(
    server_id: &str,
    list_source: &EmbyPlaylistSource,
    item_id: &str,
    item_type: &str,
    is_container: bool,
    requested_item_types: &[String],
    provider_instance_name: Option<&str>,
) -> Option<synctv_proto::providers::common::DiscoveredSource> {
    let nested_source = match list_source {
        EmbyPlaylistSource::FavoritePeople => Some(EmbyPlaylistSource::PersonItems {
            person_id: item_id.to_string(),
            item_types: requested_item_types.to_vec(),
        }),
        EmbyPlaylistSource::Genres { item_types } => Some(EmbyPlaylistSource::GenreItems {
            genre_id: item_id.to_string(),
            item_types: item_types.clone(),
        }),
        EmbyPlaylistSource::Playlists | EmbyPlaylistSource::Collections => {
            Some(EmbyPlaylistSource::Folder {
                item_id: item_id.to_string(),
            })
        }
        _ if is_container => Some(EmbyPlaylistSource::Folder {
            item_id: item_id.to_string(),
        }),
        _ => None,
    };
    if let Some(source) = nested_source {
        return Some(emby_playlist_source(
            server_id,
            &source,
            provider_instance_name,
        ));
    }
    emby_item_source(server_id, item_id, item_type, false, provider_instance_name)
}

fn emby_item_source(
    server_id: &str,
    item_id: &str,
    item_type: &str,
    is_container: bool,
    provider_instance_name: Option<&str>,
) -> Option<synctv_proto::providers::common::DiscoveredSource> {
    if is_container {
        return Some(emby_folder_source(
            server_id,
            item_id,
            provider_instance_name,
        ));
    }
    matches!(
        item_type,
        "Movie" | "Episode" | "Video" | "Audio" | "MusicAlbum"
    )
    .then(|| {
        discovered_media(
            media_source_config::Provider::Emby(EmbyMediaSourceConfig {
                server_id: server_id.to_string(),
                item_id: item_id.to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
            }),
            provider_instance_name,
        )
    })
}

fn emby_folder_source(
    server_id: &str,
    item_id: &str,
    provider_instance_name: Option<&str>,
) -> synctv_proto::providers::common::DiscoveredSource {
    emby_playlist_source(
        server_id,
        &EmbyPlaylistSource::Folder {
            item_id: item_id.to_string(),
        },
        provider_instance_name,
    )
}

fn emby_playlist_source(
    server_id: &str,
    source: &EmbyPlaylistSource,
    provider_instance_name: Option<&str>,
) -> synctv_proto::providers::common::DiscoveredSource {
    let source = match source {
        EmbyPlaylistSource::Folder { item_id } => {
            emby_playlist_source_config::Source::Folder(EmbyFolderPlaylistSource {
                item_id: item_id.clone(),
            })
        }
        EmbyPlaylistSource::FavoriteItems { item_types } => {
            emby_playlist_source_config::Source::FavoriteItems(EmbyFavoriteItemsPlaylistSource {
                item_types: item_types.clone(),
            })
        }
        EmbyPlaylistSource::FavoritePeople => {
            emby_playlist_source_config::Source::FavoritePeople(EmbyFavoritePeoplePlaylistSource {})
        }
        EmbyPlaylistSource::PersonItems {
            person_id,
            item_types,
        } => emby_playlist_source_config::Source::PersonItems(EmbyPersonItemsPlaylistSource {
            person_id: person_id.clone(),
            item_types: item_types.clone(),
        }),
        EmbyPlaylistSource::ContinueWatching => {
            emby_playlist_source_config::Source::ContinueWatching(
                EmbyContinueWatchingPlaylistSource {},
            )
        }
        EmbyPlaylistSource::NextUp => {
            emby_playlist_source_config::Source::NextUp(EmbyNextUpPlaylistSource {})
        }
        EmbyPlaylistSource::RecentlyAdded { item_types } => {
            emby_playlist_source_config::Source::RecentlyAdded(EmbyRecentlyAddedPlaylistSource {
                item_types: item_types.clone(),
            })
        }
        EmbyPlaylistSource::Playlists => {
            emby_playlist_source_config::Source::Playlists(EmbyPlaylistsPlaylistSource {})
        }
        EmbyPlaylistSource::Collections => {
            emby_playlist_source_config::Source::Collections(EmbyCollectionsPlaylistSource {})
        }
        EmbyPlaylistSource::Genres { item_types } => {
            emby_playlist_source_config::Source::Genres(EmbyGenresPlaylistSource {
                item_types: item_types.clone(),
            })
        }
        EmbyPlaylistSource::GenreItems {
            genre_id,
            item_types,
        } => emby_playlist_source_config::Source::GenreItems(EmbyGenreItemsPlaylistSource {
            genre_id: genre_id.clone(),
            item_types: item_types.clone(),
        }),
    };
    discovered_playlist(
        playlist_source_config::Provider::Emby(EmbyPlaylistSourceConfig {
            server_id: server_id.to_string(),
            source: Some(source),
            proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
        }),
        provider_instance_name,
    )
}

#[cfg(test)]
mod tests {
    use super::{EmbyApiImpl, ProviderApiRuntime};
    use std::sync::Arc;
    use synctv_core::provider::{EmbyProvider, ProviderError};
    use synctv_core::repository::{ProviderInstanceRepository, UserProviderCredentialRepository};
    use synctv_core_testing::create_test_pool;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn invalid_config<T>(result: Result<T, ProviderError>) -> TestResult<String> {
        match result {
            Ok(_) => Err(test_error("expected provider invalid config")),
            Err(ProviderError::InvalidConfig(message)) => Ok(message),
            Err(other) => Err(test_error(format!("expected InvalidConfig, got {other:?}"))),
        }
    }

    fn test_api(pool: sqlx::PgPool) -> TestResult<EmbyApiImpl> {
        let instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool.clone())),
        ));
        let provider = Arc::new(EmbyProvider::with_client_manager(
            instance_manager,
            Arc::new(synctv_core::provider::ProviderClientManager::new()?),
        ));
        let credential_repo = Arc::new(UserProviderCredentialRepository::new(pool.clone()));
        let alist_instance_manager = Arc::new(synctv_core::service::RemoteProviderManager::new(
            Arc::new(ProviderInstanceRepository::new(pool)),
        ));
        let runtime = ProviderApiRuntime {
            access_service: Arc::new(synctv_core::provider::CachedProviderAccessService::new(
                credential_repo.clone(),
                Arc::new(synctv_core::provider::AlistProvider::with_client_manager(
                    alist_instance_manager,
                    Arc::new(synctv_core::provider::ProviderClientManager::new()?),
                )),
            )),
            event_service: Arc::new(synctv_realtime::fanout::LocalNoopRealtimeEventService::new()),
        };
        Ok(EmbyApiImpl::new_with_runtime(
            Arc::new(provider.with_credential_repo(credential_repo)),
            runtime,
        ))
    }

    #[test]
    fn resolve_login_credential_rejects_missing_credential() -> TestResult {
        let message = invalid_config(EmbyProvider::resolve_login_request(
            "https://emby.example.com".to_string(),
            "alice".to_string(),
            None,
            None,
        ))?;

        assert!(message.contains("exactly one credential"));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn logout_rejects_empty_server_id() -> TestResult {
        let (_postgres, pool) = create_test_pool().await;
        let api = test_api(pool)?;

        let message = invalid_config(
            api.logout(
                &synctv_core::models::UserId::new(),
                synctv_proto::providers::emby::LogoutRequest {
                    server_id: String::new(),
                },
            )
            .await,
        )?;

        assert!(message.contains("explicit server_id"));
        Ok(())
    }
}
