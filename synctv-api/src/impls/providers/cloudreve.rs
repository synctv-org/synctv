use std::sync::Arc;

use synctv_core::models::UserId;
use synctv_core::provider::DynamicPagination;
use synctv_core::provider::{CloudreveProvider, ProviderError};
use synctv_proto::providers::cloudreve::{
    BindInfo, FileItem, GetBindsResponse, GetMeRequest, GetMeResponse, ListRequest, ListResponse,
    LoginRequest, LoginResponse, LogoutRequest, LogoutResponse, SearchRequest, SearchResponse,
};
use synctv_realtime::fanout::RealtimeEventService;

use super::{
    provider_instance_name_for_response, publish_provider_credential_changed,
    resolve_bound_instance_name,
};

#[derive(Clone)]
pub struct CloudreveApiImpl {
    provider: Arc<CloudreveProvider>,
    event_service: Arc<dyn RealtimeEventService>,
}

impl CloudreveApiImpl {
    #[must_use]
    pub fn new(
        provider: Arc<CloudreveProvider>,
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
        let (server_id, _) = self
            .provider
            .login_and_persist(
                user_id,
                req.host,
                req.email,
                req.password,
                instance_name.map(ToString::to_string),
            )
            .await?;
        publish_provider_credential_changed(
            &self.event_service,
            user_id,
            CloudreveProvider::NAME,
            &server_id,
        );
        Ok(LoginResponse { server_id })
    }

    pub async fn list(
        &self,
        user_id: UserId,
        req: ListRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<ListResponse, ProviderError> {
        let parent = req.path.clone();
        let pagination = match req.pagination {
            Some(synctv_proto::providers::cloudreve::list_request::Pagination::Page(page)) => {
                DynamicPagination::Page {
                    page: usize::try_from(page.page.max(1)).map_err(|_| {
                        ProviderError::InvalidConfig(
                            "Cloudreve page exceeds usize::MAX".to_string(),
                        )
                    })?,
                }
            }
            Some(synctv_proto::providers::cloudreve::list_request::Pagination::Cursor(cursor)) => {
                DynamicPagination::Cursor {
                    cursor: Some(cursor.cursor).filter(|value| !value.is_empty()),
                }
            }
            None => DynamicPagination::Cursor { cursor: None },
        };
        let (response, stored_instance_name) = self
            .provider
            .list(
                user_id,
                &req.server_id,
                &req.path,
                pagination,
                req.per_page.max(1),
            )
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        let pagination = Some(match response.pagination {
            DynamicPagination::Page { page } => {
                synctv_proto::providers::cloudreve::list_response::Pagination::Page(
                    synctv_proto::providers::cloudreve::PagePagination {
                        page: u32::try_from(page).unwrap_or(u32::MAX),
                        total: response.total,
                    },
                )
            }
            DynamicPagination::Cursor { cursor } => {
                synctv_proto::providers::cloudreve::list_response::Pagination::Cursor(
                    synctv_proto::providers::cloudreve::CursorPagination {
                        cursor: cursor.unwrap_or_default(),
                    },
                )
            }
        });
        Ok(ListResponse {
            content: response
                .content
                .into_iter()
                .map(|item| file_item(item, Some(&parent)))
                .collect(),
            pagination,
        })
    }

    pub async fn search(
        &self,
        user_id: UserId,
        req: SearchRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<SearchResponse, ProviderError> {
        let (response, stored_instance_name) = self
            .provider
            .search(user_id, &req.server_id, &req.keywords, req.offset)
            .await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(SearchResponse {
            content: response
                .content
                .into_iter()
                .map(|item| file_item(item, None))
                .collect(),
            total: response.total,
        })
    }

    pub async fn get_me(
        &self,
        user_id: UserId,
        req: GetMeRequest,
        requested_instance_name: Option<&str>,
    ) -> Result<GetMeResponse, ProviderError> {
        let (user, stored_instance_name) = self.provider.me(user_id, &req.server_id).await?;
        resolve_bound_instance_name(requested_instance_name, stored_instance_name.as_deref())?;
        Ok(GetMeResponse {
            id: user.id,
            email: user.email,
            nickname: user.nickname,
        })
    }

    pub async fn logout(
        &self,
        user_id: UserId,
        req: LogoutRequest,
    ) -> Result<LogoutResponse, ProviderError> {
        if req.server_id.trim().is_empty() {
            return Err(ProviderError::InvalidConfig(
                "Cloudreve logout requires server_id".to_string(),
            ));
        }
        if self
            .provider
            .delete_credential(user_id, &req.server_id)
            .await?
        {
            publish_provider_credential_changed(
                &self.event_service,
                user_id,
                CloudreveProvider::NAME,
                &req.server_id,
            );
        }
        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
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
                host: bind.host,
                email: bind.email,
                created_at: bind.created_at,
                provider_instance_name: provider_instance_name_for_response(
                    bind.provider_instance_name,
                ),
            })
            .collect();
        Ok(GetBindsResponse { binds })
    }
}

fn file_item(
    item: synctv_media_providers::cloudreve::CloudreveFile,
    parent: Option<&str>,
) -> FileItem {
    let path = if item.path.trim().is_empty() {
        let parent = parent
            .filter(|value| !value.trim().is_empty() && value.trim() != "/")
            .unwrap_or("cloudreve://my/")
            .trim_end_matches('/');
        format!("{parent}/{}", item.name)
    } else {
        item.path.clone()
    };
    let is_dir = item.is_dir();
    let thumbnail = item.thumbnail();
    FileItem {
        id: item.id,
        name: item.name,
        path,
        size: u64::try_from(item.size.max(0)).unwrap_or_default(),
        is_dir,
        modified: item.updated_at.map_or(0, |value| value.timestamp()),
        thumbnail,
    }
}
