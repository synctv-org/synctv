use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::{
    PlaybackTransportAction, ProviderError, SeafileListRequest, SeafileProvider,
};
use synctv_proto::providers::seafile::{
    BindInfo, FileItem, GetBindsResponse, ListRepositoriesRequest, ListRequest, ListResponse,
    ListStarredRequest, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse,
    UnlockLibraryRequest, UnlockLibraryResponse,
};
use synctv_proto::source_config::{
    media_source_config, playlist_source_config, seafile_playlist_source_config,
    SeafileFolderPlaylistSourceConfig, SeafileMediaSourceConfig, SeafilePlaylistSourceConfig,
    SeafileStarredPlaylistSourceConfig,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    discovered_media, discovered_playlist, publish_provider_credential_changed,
    resolve_bound_instance_name,
};

#[derive(Clone)]
pub struct SeafileApiImpl {
    provider: Arc<SeafileProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl SeafileApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<SeafileProvider>,
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
        let (server_id, account, server) = self
            .provider
            .login_and_persist(
                user_id,
                req.endpoint,
                req.username,
                req.password,
                instance_name.map(str::to_string),
            )
            .await?;
        self.publish(user_id, &server_id);
        Ok(LoginResponse {
            server_id,
            email: account.email,
            display_name: account.name,
            version: server.version,
            features: server.features,
            quota_total: account.total,
            quota_usage: account.usage,
        })
    }

    pub async fn unlock_library(
        &self,
        user_id: UserId,
        req: UnlockLibraryRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<UnlockLibraryResponse, ProviderError> {
        let stored = self
            .provider
            .unlock_library(user_id, &req.server_id, &req.repository_id, req.password)
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored.as_deref())?;
        self.publish(user_id, &req.server_id);
        Ok(UnlockLibraryResponse { success: true })
    }

    pub async fn list_repositories(
        &self,
        user_id: UserId,
        req: ListRepositoriesRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let page = page(req.page)?;
        let (response, stored) = self
            .provider
            .list_repositories(
                user_id,
                &req.server_id,
                page,
                req.page_size.clamp(1, 200) as usize,
            )
            .await?;
        let instance_name =
            resolve_bound_instance_name(requested_instance_name, stored.as_deref())?;
        Ok(list_response(
            response,
            &req.server_id,
            None,
            instance_name.as_deref(),
        ))
    }

    pub async fn list(
        &self,
        user_id: UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let page = page(req.page)?;
        let (response, stored) = self
            .provider
            .list(SeafileListRequest {
                owner: user_id,
                server_id: &req.server_id,
                repository_id: &req.repository_id,
                path: &req.path,
                page,
                page_size: req.page_size.clamp(1, 200) as usize,
                search: req.search.as_deref(),
            })
            .await?;
        let instance_name =
            resolve_bound_instance_name(requested_instance_name, stored.as_deref())?;
        Ok(list_response(
            response,
            &req.server_id,
            Some(SeafilePlaylistSourceConfig {
                server_id: req.server_id.clone(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(seafile_playlist_source_config::Source::Folder(
                    SeafileFolderPlaylistSourceConfig {
                        repository_id: req.repository_id,
                        path: req.path,
                    },
                )),
            }),
            instance_name.as_deref(),
        ))
    }

    pub async fn list_starred(
        &self,
        user_id: UserId,
        req: ListStarredRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let page = page(req.page)?;
        let (response, stored) = self
            .provider
            .list_starred(
                user_id,
                &req.server_id,
                page,
                req.page_size.clamp(1, 200) as usize,
            )
            .await?;
        let instance_name =
            resolve_bound_instance_name(requested_instance_name, stored.as_deref())?;
        Ok(list_response(
            response,
            &req.server_id,
            Some(SeafilePlaylistSourceConfig {
                server_id: req.server_id.clone(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(seafile_playlist_source_config::Source::Starred(
                    SeafileStarredPlaylistSourceConfig {},
                )),
            }),
            instance_name.as_deref(),
        ))
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
            self.publish(user_id, &req.server_id);
        }
        Ok(LogoutResponse { success })
    }

    pub async fn binds(
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
                username: bind.username,
                version: bind.version,
                features: bind.features,
                created_at: bind.created_at,
                provider_instance_name: bind.provider_instance_name.unwrap_or_default(),
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }

    pub async fn thumbnail_action(
        &self,
        owner: UserId,
        server_id: &str,
        repository_id: &str,
        path: &str,
        size: u32,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.provider
            .thumbnail_action(owner, server_id, repository_id, path, size)
            .await
    }

    fn publish(&self, user_id: UserId, server_id: &str) {
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            synctv_core::models::SourceProvider::Seafile,
            server_id,
        );
    }
}

fn page(value: u64) -> Result<usize, ProviderError> {
    usize::try_from(value.max(1))
        .map_err(|_| ProviderError::InvalidConfig("Seafile page exceeds usize::MAX".to_string()))
}

fn list_response(
    response: synctv_core::provider::SeafileListResponse,
    server_id: &str,
    playlist_source: Option<SeafilePlaylistSourceConfig>,
    provider_instance_name: Option<&str>,
) -> ListResponse {
    ListResponse {
        content: response
            .content
            .into_iter()
            .map(|item| {
                let source = if item.is_directory {
                    discovered_playlist(
                        playlist_source_config::Provider::Seafile(SeafilePlaylistSourceConfig {
                            server_id: server_id.to_string(),
                            proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                            source: Some(seafile_playlist_source_config::Source::Folder(
                                SeafileFolderPlaylistSourceConfig {
                                    repository_id: item.repository_id.clone(),
                                    path: item.path.clone(),
                                },
                            )),
                        }),
                        provider_instance_name,
                    )
                } else {
                    discovered_media(
                        media_source_config::Provider::Seafile(SeafileMediaSourceConfig {
                            server_id: server_id.to_string(),
                            proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                            repository_id: item.repository_id.clone(),
                            path: item.path.clone(),
                            object_id: item.object_id.clone(),
                            has_thumbnail: item.has_thumbnail,
                        }),
                        provider_instance_name,
                    )
                };
                FileItem {
                    repository_id: item.repository_id,
                    repository_name: item.repository_name,
                    path: item.path,
                    name: item.name,
                    object_id: item.object_id,
                    is_dir: item.is_directory,
                    size: item.size,
                    modified_at: item.modified_at,
                    permission: item.permission,
                    modifier_name: item.modifier_name,
                    starred: item.starred,
                    has_thumbnail: item.has_thumbnail,
                    repository_encrypted: item.repository_encrypted,
                    password_required: item.password_required,
                    source: Some(source),
                }
            })
            .collect(),
        total: response.total,
        page: response.page as u64,
        has_more: response.has_more,
        source: playlist_source.map(|source| {
            discovered_playlist(
                playlist_source_config::Provider::Seafile(source),
                provider_instance_name,
            )
        }),
    }
}
