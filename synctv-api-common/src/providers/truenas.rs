use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::{ProviderError, TrueNasProvider};
use synctv_proto::providers::truenas::{
    BindInfo, FileItem, GetBindsResponse, ListRequest, ListResponse, LoginRequest, LoginResponse,
    LogoutRequest, LogoutResponse,
};
use synctv_proto::source_config::{
    media_source_config, playlist_source_config, TrueNasFolderPlaylistSourceConfig,
    TrueNasMediaSourceConfig, TrueNasPlaylistSourceConfig,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    discovered_media, discovered_playlist, publish_provider_credential_changed,
    resolve_bound_instance_name,
};

#[derive(Clone)]
pub struct TrueNasApiImpl {
    provider: Arc<TrueNasProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl TrueNasApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<TrueNasProvider>,
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
                req.api_key,
                instance_name.map(str::to_string),
            )
            .await?;
        self.publish(user_id, &server_id);
        Ok(LoginResponse {
            server_id,
            hostname: info.hostname,
            version: info.version,
            system_product: info.system_product.unwrap_or_default(),
        })
    }

    pub async fn list(
        &self,
        user_id: UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let page = usize::try_from(req.page.max(1)).map_err(|_| {
            ProviderError::InvalidConfig("TrueNAS page exceeds usize::MAX".to_string())
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
        Ok(ListResponse {
            content: response
                .content
                .into_iter()
                .map(|item| {
                    let is_dir = item.is_directory();
                    let source = truenas_source(
                        &req.server_id,
                        &item.path,
                        is_dir,
                        instance_name.as_deref(),
                    );
                    FileItem {
                        name: item.name,
                        path: item.path,
                        realpath: item.realpath,
                        is_dir,
                        size: item.size,
                        allocation_size: item.allocation_size,
                        mode: item.mode,
                        uid: item.uid,
                        gid: item.gid,
                        mount_id: item.mount_id,
                        acl: item.acl,
                        is_mountpoint: item.is_mountpoint,
                        is_ctldir: item.is_ctldir,
                        attributes: item.attributes,
                        xattrs: item.xattrs,
                        zfs_attributes: item.zfs_attrs.unwrap_or_default(),
                        source: Some(source),
                    }
                })
                .collect(),
            total: response.total,
            page: response.page as u64,
            has_more: response.has_more,
            source: Some(truenas_source(
                &req.server_id,
                &req.path,
                true,
                instance_name.as_deref(),
            )),
        })
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
                hostname: bind.hostname,
                version: bind.version,
                system_product: bind.system_product,
                created_at: bind.created_at,
                provider_instance_name: bind.provider_instance_name.unwrap_or_default(),
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }

    fn publish(&self, user_id: UserId, server_id: &str) {
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            synctv_core::models::SourceProvider::TrueNas,
            server_id,
        );
    }
}

fn truenas_source(
    server_id: &str,
    path: &str,
    playlist: bool,
    provider_instance_name: Option<&str>,
) -> synctv_proto::providers::common::DiscoveredSource {
    if playlist {
        discovered_playlist(
            playlist_source_config::Provider::Truenas(TrueNasPlaylistSourceConfig {
                server_id: server_id.to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                source: Some(
                    synctv_proto::source_config::true_nas_playlist_source_config::Source::Folder(
                        TrueNasFolderPlaylistSourceConfig {
                            path: path.to_string(),
                        },
                    ),
                ),
            }),
            provider_instance_name,
        )
    } else {
        discovered_media(
            media_source_config::Provider::Truenas(TrueNasMediaSourceConfig {
                server_id: server_id.to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                path: path.to_string(),
            }),
            provider_instance_name,
        )
    }
}
