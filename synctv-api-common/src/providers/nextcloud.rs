use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::{NextcloudProvider, PlaybackTransportAction, ProviderError};
use synctv_proto::providers::nextcloud::{
    BindInfo, FileItem, GetBindsResponse, ListFavoritesRequest, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, PollLoginFlowRequest,
    StartLoginFlowRequest, StartLoginFlowResponse,
};
use synctv_proto::source_config::{
    media_source_config, nextcloud_playlist_source_config, playlist_source_config,
    NextcloudFavoritesPlaylistSourceConfig, NextcloudFolderPlaylistSourceConfig,
    NextcloudMediaSourceConfig, NextcloudPlaylistSourceConfig,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    discovered_media, discovered_playlist, publish_provider_credential_changed,
    resolve_bound_instance_name,
};

#[derive(Clone)]
pub struct NextcloudApiImpl {
    provider: Arc<NextcloudProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl NextcloudApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<NextcloudProvider>,
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
        let (server_id, info) = self
            .provider
            .login_and_persist(
                user_id,
                req.endpoint,
                req.username,
                req.app_password,
                instance_name.map(str::to_string),
            )
            .await?;
        self.publish(user_id, &server_id);
        Ok(login_response(server_id, info))
    }

    pub async fn start_login_flow(
        &self,
        req: StartLoginFlowRequest,
    ) -> Result<StartLoginFlowResponse, ProviderError> {
        let flow = self.provider.start_login_flow(&req.endpoint).await?;
        Ok(StartLoginFlowResponse {
            login_url: flow.login,
            poll_endpoint: flow.poll.endpoint,
            poll_token: flow.poll.token,
        })
    }

    pub async fn poll_login_flow(
        &self,
        user_id: UserId,
        req: PollLoginFlowRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginResponse, ProviderError> {
        let (server_id, info) = self
            .provider
            .poll_login_and_persist(
                user_id,
                req.endpoint,
                &req.poll_endpoint,
                &req.poll_token,
                instance_name.map(str::to_string),
            )
            .await?;
        self.publish(user_id, &server_id);
        Ok(login_response(server_id, info))
    }

    pub async fn list(
        &self,
        user_id: UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let page = usize::try_from(req.page.max(1)).map_err(|_| {
            ProviderError::InvalidConfig("Nextcloud page exceeds usize::MAX".to_string())
        })?;
        let (response, stored) = self
            .provider
            .list(
                user_id,
                &req.server_id,
                &req.path,
                page,
                req.page_size.clamp(1, 200) as usize,
                req.search.as_deref(),
            )
            .await?;
        let instance_name =
            resolve_bound_instance_name(requested_instance_name, stored.as_deref())?;
        Ok(list_response(
            response,
            &req.server_id,
            NextcloudPlaylistSourceConfig {
                server_id: req.server_id.clone(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(nextcloud_playlist_source_config::Source::Folder(
                    NextcloudFolderPlaylistSourceConfig { path: req.path },
                )),
            },
            instance_name.as_deref(),
        ))
    }

    pub async fn list_favorites(
        &self,
        user_id: UserId,
        req: ListFavoritesRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let page = usize::try_from(req.page.max(1)).map_err(|_| {
            ProviderError::InvalidConfig("Nextcloud page exceeds usize::MAX".to_string())
        })?;
        let (response, stored) = self
            .provider
            .list_favorites(
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
            NextcloudPlaylistSourceConfig {
                server_id: req.server_id.clone(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(nextcloud_playlist_source_config::Source::Favorites(
                    NextcloudFavoritesPlaylistSourceConfig {},
                )),
            },
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
                user_id: bind.user_id,
                version: bind.version,
                edition: bind.edition,
                created_at: bind.created_at,
                provider_instance_name: bind.provider_instance_name.unwrap_or_default(),
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }

    pub async fn preview_action(
        &self,
        owner: UserId,
        server_id: &str,
        file_id: u64,
        width: u32,
        height: u32,
        crop: bool,
    ) -> Result<PlaybackTransportAction, ProviderError> {
        self.provider
            .thumbnail_action(owner, server_id, file_id, width, height, crop)
            .await
    }

    fn publish(&self, user_id: UserId, server_id: &str) {
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            NextcloudProvider::NAME,
            server_id,
        );
    }
}

fn login_response(
    server_id: String,
    info: synctv_media_providers::nextcloud::NextcloudServerInfo,
) -> LoginResponse {
    LoginResponse {
        server_id,
        user_id: info.user.id,
        display_name: info.user.displayname,
        version: info.capabilities.version,
        edition: info.capabilities.edition,
    }
}

fn list_response(
    response: synctv_core::provider::NextcloudListResponse,
    server_id: &str,
    playlist_source: NextcloudPlaylistSourceConfig,
    provider_instance_name: Option<&str>,
) -> ListResponse {
    ListResponse {
        content: response
            .content
            .into_iter()
            .map(|item| {
                let source = if item.is_directory {
                    discovered_playlist(
                        playlist_source_config::Provider::Nextcloud(
                            NextcloudPlaylistSourceConfig {
                                server_id: server_id.to_string(),
                                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto
                                    as i32,
                                source: Some(nextcloud_playlist_source_config::Source::Folder(
                                    NextcloudFolderPlaylistSourceConfig {
                                        path: item.path.clone(),
                                    },
                                )),
                            },
                        ),
                        provider_instance_name,
                    )
                } else {
                    discovered_media(
                        media_source_config::Provider::Nextcloud(NextcloudMediaSourceConfig {
                            server_id: server_id.to_string(),
                            proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                            path: item.path.clone(),
                            file_id: item.file_id,
                        }),
                        provider_instance_name,
                    )
                };
                FileItem {
                    name: item.name,
                    path: item.path,
                    file_id: item.file_id,
                    is_dir: item.is_directory,
                    size: item.size,
                    modified_at: item.modified_at,
                    content_type: item.content_type,
                    etag: item.etag,
                    permissions: item.permissions,
                    owner_id: item.owner_id,
                    owner_display_name: item.owner_display_name,
                    favorite: item.favorite,
                    has_preview: item.has_preview,
                    blurhash: item.blurhash,
                    width: item.width,
                    height: item.height,
                    duration_millis: item.duration_millis,
                    source: Some(source),
                }
            })
            .collect(),
        total: response.total,
        page: response.page as u64,
        has_more: response.has_more,
        source: Some(discovered_playlist(
            playlist_source_config::Provider::Nextcloud(playlist_source),
            provider_instance_name,
        )),
    }
}
