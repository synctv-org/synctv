use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::{ProviderError, QnapProvider};
use synctv_proto::providers::qnap::{
    BindInfo, FileItem, GetBindsResponse, GetCapabilitiesRequest, GetCapabilitiesResponse,
    ListRequest, ListResponse, LoginRequest, LoginResponse, LogoutRequest, LogoutResponse,
};
use synctv_proto::source_config::{
    media_source_config, playlist_source_config, QnapMediaSourceConfig, QnapPlaylistSourceConfig,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    discovered_media, discovered_playlist, provider_instance_name_for_response,
    publish_provider_credential_changed, resolve_bound_instance_name,
};

#[derive(Clone)]
pub struct QnapApiImpl {
    provider: Arc<QnapProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl QnapApiImpl {
    #[must_use]
    pub fn new(provider: Arc<QnapProvider>, event_service: Arc<dyn RealtimeEventService>) -> Self {
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
        let (server_id, login) = self
            .provider
            .login_and_persist(
                user_id,
                req.endpoint,
                req.username,
                req.password,
                instance_name.map(str::to_string),
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            synctv_core::models::SourceProvider::Qnap,
            &server_id,
        );
        Ok(LoginResponse {
            server_id,
            server_name: login.servername,
            version: login.version,
            support_rtt: login.support_rtt != 0,
        })
    }

    pub async fn list(
        &self,
        user_id: UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let page = usize::try_from(req.page.max(1)).map_err(|_| {
            ProviderError::InvalidConfig("QNAP page exceeds usize::MAX".to_string())
        })?;
        let page_size = usize::try_from(req.page_size.clamp(1, 200)).map_err(|_| {
            ProviderError::InvalidConfig("QNAP page_size exceeds usize::MAX".to_string())
        })?;
        let (response, stored_instance_name) = self
            .provider
            .list(
                user_id,
                &req.server_id,
                &req.path,
                page,
                page_size,
                req.search.as_deref(),
            )
            .await?;
        let instance_name =
            resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(ListResponse {
            content: response
                .content
                .into_iter()
                .map(|item| {
                    let source = qnap_source(
                        &req.server_id,
                        &item.path,
                        item.is_dir,
                        instance_name.as_deref(),
                    );
                    FileItem {
                        name: item.name,
                        path: item.path,
                        is_dir: item.is_dir,
                        size: item.size,
                        modified_at: item.modified_at,
                        file_type: item.file_type,
                        pre_transcoded_heights: item.pre_transcoded_heights,
                        source: Some(source),
                    }
                })
                .collect(),
            total: response.total,
            page: u64::try_from(response.page).unwrap_or(u64::MAX),
            has_more: response.has_more,
            realtime_transcode: response.realtime_transcode,
            source: Some(qnap_source(
                &req.server_id,
                &req.path,
                true,
                instance_name.as_deref(),
            )),
        })
    }

    pub async fn capabilities(
        &self,
        user_id: UserId,
        req: GetCapabilitiesRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<GetCapabilitiesResponse, ProviderError> {
        let (capabilities, stored_instance_name) =
            self.provider.capabilities(user_id, &req.server_id).await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(GetCapabilitiesResponse {
            support_rtt: capabilities.support_rtt,
            hardware_transcode: capabilities.hardware_transcode,
            qtranscode: capabilities.qtranscode,
            multimedia_codec: capabilities.multimedia_codec,
            hd_station_support: capabilities.hd_station_support,
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
            publish_provider_credential_changed(
                &self.event_service,
                user_id,
                synctv_core::models::SourceProvider::Qnap,
                &req.server_id,
            );
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
                server_name: bind.server_name,
                version: bind.version,
                support_rtt: bind.support_rtt,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }

    pub async fn thumbnail_action(
        &self,
        credential_owner_id: UserId,
        server_id: &str,
        path: &str,
        size: u32,
    ) -> Result<synctv_core::provider::PlaybackTransportAction, ProviderError> {
        self.provider
            .thumbnail_action(credential_owner_id, server_id, path, size)
            .await
    }
}

fn qnap_source(
    server_id: &str,
    path: &str,
    playlist: bool,
    provider_instance_name: Option<&str>,
) -> synctv_proto::providers::common::DiscoveredSource {
    if playlist {
        discovered_playlist(
            playlist_source_config::Provider::Qnap(QnapPlaylistSourceConfig {
                server_id: server_id.to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                path: path.to_string(),
            }),
            provider_instance_name,
        )
    } else {
        discovered_media(
            media_source_config::Provider::Qnap(QnapMediaSourceConfig {
                server_id: server_id.to_string(),
                proxy_mode: synctv_proto::source_config::PlaybackProxyMode::Auto as i32,
                path: path.to_string(),
            }),
            provider_instance_name,
        )
    }
}
