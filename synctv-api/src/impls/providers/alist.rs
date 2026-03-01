//! Alist API Implementation
//!
//! Unified implementation for all Alist API operations.
//! Used by both HTTP and gRPC handlers.

use crate::proto::providers::alist::{
    FileItem, GetMeRequest, GetMeResponse, ListRequest, ListResponse, LoginRequest, LoginResponse,
    LogoutRequest, LogoutResponse,
};
use std::sync::Arc;
use synctv_core::provider::AlistProvider;

/// Alist API implementation
///
/// Contains all business logic for Alist operations.
/// Methods accept grpc-generated request types and return grpc-generated response types.
#[derive(Clone)]
pub struct AlistApiImpl {
    provider: Arc<AlistProvider>,
}

impl AlistApiImpl {
    #[must_use]
    pub const fn new(provider: Arc<AlistProvider>) -> Self {
        Self { provider }
    }

    /// Login to Alist
    pub async fn login(
        &self,
        req: LoginRequest,
        instance_name: Option<&str>,
    ) -> Result<LoginResponse, synctv_core::provider::ProviderError> {
        let (password, hashed) = if req.hashed_password.is_empty() {
            (req.password, false)
        } else {
            (req.hashed_password, true)
        };

        let login_req = synctv_media_providers::grpc::alist::LoginReq {
            host: req.host,
            username: req.username,
            password,
            hashed,
        };

        let token = self.provider.login(login_req, instance_name).await?;

        Ok(LoginResponse { token })
    }

    /// List Alist directory
    pub async fn list(
        &self,
        req: ListRequest,
        instance_name: Option<&str>,
    ) -> Result<ListResponse, synctv_core::provider::ProviderError> {
        let list_req = synctv_media_providers::grpc::alist::FsListReq {
            host: req.host,
            token: req.token,
            path: req.path,
            password: req.password,
            page: req.page,
            per_page: req.per_page,
            refresh: req.refresh,
        };

        let resp = self.provider.fs_list(list_req, instance_name).await?;

        let content: Vec<FileItem> = resp
            .content
            .into_iter()
            .map(|item| FileItem {
                name: item.name,
                size: item.size,
                is_dir: item.is_dir,
                modified: item.modified,
                sign: item.sign,
                thumb: item.thumb,
                r#type: item.r#type,
            })
            .collect();

        Ok(ListResponse {
            content,
            total: resp.total,
        })
    }

    /// Get Alist user info
    pub async fn get_me(
        &self,
        req: GetMeRequest,
        instance_name: Option<&str>,
    ) -> Result<GetMeResponse, synctv_core::provider::ProviderError> {
        let me_req = synctv_media_providers::grpc::alist::MeReq {
            host: req.host,
            token: req.token,
        };

        let resp = self.provider.me(me_req, instance_name).await?;

        Ok(GetMeResponse {
            username: resp.username,
            base_path: resp.base_path,
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
