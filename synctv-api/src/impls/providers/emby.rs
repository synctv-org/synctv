//! Emby API Implementation
//!
//! Unified implementation for all Emby API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::emby::{
    GetMeRequest, GetMeResponse, ListRequest, ListResponse, LoginRequest, LoginResponse,
    LogoutRequest, LogoutResponse, MediaItem,
};
use std::sync::Arc;
use synctv_core::provider::EmbyProvider;

/// Emby API implementation
///
/// Contains all business logic for Emby operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct EmbyApiImpl {
    provider: Arc<EmbyProvider>,
}

impl EmbyApiImpl {
    #[must_use]
    pub const fn new(provider: Arc<EmbyProvider>) -> Self {
        Self { provider }
    }

    /// Login to Emby
    pub async fn login(
        &self,
        req: LoginRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let user_info = self
            .provider
            .login(req.host, req.api_key, instance_name)
            .await?;

        // Extract admin status from user policy
        let is_admin = user_info
            .policy
            .as_ref()
            .is_some_and(|p| p.is_administrator);

        Ok(LoginResponse {
            user_id: user_info.id,
            username: user_info.name,
            is_admin,
        })
    }

    /// List Emby library items
    pub async fn list(
        &self,
        req: ListRequest,
        instance_name: Option<&str>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        let list_req = synctv_media_providers::grpc::emby::FsListReq {
            host: req.host,
            token: req.token,
            path: req.path,
            start_index: req.start_index,
            limit: req.limit,
            search_term: req.search_term,
            user_id: req.user_id,
        };

        let resp = self.provider.fs_list(list_req, instance_name).await?;

        // Convert Item to MediaItem
        let items: Vec<MediaItem> = resp
            .items
            .into_iter()
            .map(|item| MediaItem {
                id: item.id,
                name: item.name,
                r#type: item.r#type,
                parent_id: item.parent_id,
                series_name: item.series_name,
                series_id: item.series_id,
                season_name: item.season_name,
            })
            .collect();

        Ok(ListResponse {
            items,
            total: resp.total,
        })
    }

    /// Get Emby user info
    pub async fn get_me(
        &self,
        req: GetMeRequest,
        instance_name: Option<&str>,
    ) -> Result<GetMeResponse, synctv_core::provider::ProviderError> {
        let me_req = synctv_media_providers::grpc::emby::MeReq {
            host: req.host,
            token: req.token,
            user_id: String::new(), // Empty = get current user
        };

        let resp = self.provider.me(me_req, instance_name).await?;

        Ok(GetMeResponse {
            id: resp.id,
            name: resp.name,
        })
    }

    /// Logout
    pub async fn logout(
        &self,
        _req: LogoutRequest,
    ) -> Result<LogoutResponse, synctv_core::provider::ProviderError> {
        Ok(LogoutResponse {
            message: "Logout successful".to_string(),
        })
    }
}
