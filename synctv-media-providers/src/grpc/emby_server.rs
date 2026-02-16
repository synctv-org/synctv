//! Emby gRPC Server Implementation
//!
//! Thin wrapper around `EmbyService` that implements gRPC server trait.

use super::emby::{
    emby_server::Emby, DeleteActiveEncodingsReq, Empty, FsListReq, FsListResp, GetItemReq,
    GetItemsReq, GetItemsResp, Item, LoginReq, LoginResp, LogoutReq, MeReq, MeResp,
    PlaybackInfoReq, PlaybackInfoResp, SystemInfoReq, SystemInfoResp,
};
use super::validation::validate_host_with_dns;
use crate::emby::{EmbyInterface, EmbyService as EmbyServiceImpl};
use crate::emby::error::EmbyError;
use tonic::{Request, Response, Status};

/// Map Emby errors to appropriate gRPC status codes instead of leaking internals.
fn map_emby_error(context: &str, e: EmbyError) -> Status {
    match e {
        EmbyError::Auth(_) => Status::unauthenticated(format!("{context}: authentication failed")),
        EmbyError::Http { status, .. } => {
            if status.as_u16() == 401 || status.as_u16() == 403 {
                Status::permission_denied(format!("{context}: access denied"))
            } else if status.as_u16() == 404 {
                Status::not_found(format!("{context}: resource not found"))
            } else if status.is_server_error() {
                Status::unavailable(format!("{context}: upstream server error"))
            } else {
                Status::internal(format!("{context}: request failed"))
            }
        }
        EmbyError::Network(_) => Status::unavailable(format!("{context}: network error")),
        EmbyError::Parse(_) => Status::internal(format!("{context}: failed to parse response")),
        EmbyError::Api { .. } => Status::internal(format!("{context}: API error")),
        EmbyError::InvalidConfig(_) => Status::invalid_argument(format!("{context}: invalid configuration")),
        EmbyError::InvalidHeader(_) => Status::internal(format!("{context}: invalid header")),
        EmbyError::NotImplemented(_) => Status::unimplemented(format!("{context}: not implemented")),
        EmbyError::ResponseTooLarge { size } => Status::resource_exhausted(format!("{context}: response too large ({size} bytes)")),
    }
}

/// Emby gRPC server
///
/// Thin wrapper that delegates to `EmbyService` for actual implementation.
pub struct EmbyService {
    service: EmbyServiceImpl,
}

impl EmbyService {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            service: EmbyServiceImpl::new(),
        }
    }
}

impl Default for EmbyService {
    fn default() -> Self {
        Self::new()
    }
}

#[tonic::async_trait]
impl Emby for EmbyService {
    async fn login(&self, request: Request<LoginReq>) -> Result<Response<LoginResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.login(req).await
            .map_err(|e| map_emby_error("login", e))?;
        Ok(Response::new(resp))
    }

    async fn me(&self, request: Request<MeReq>) -> Result<Response<MeResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.me(req).await
            .map_err(|e| map_emby_error("me", e))?;
        Ok(Response::new(resp))
    }

    async fn get_items(
        &self,
        request: Request<GetItemsReq>,
    ) -> Result<Response<GetItemsResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.get_items(req).await
            .map_err(|e| map_emby_error("get_items", e))?;
        Ok(Response::new(resp))
    }

    async fn get_item(&self, request: Request<GetItemReq>) -> Result<Response<Item>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.get_item(req).await
            .map_err(|e| map_emby_error("get_item", e))?;
        Ok(Response::new(resp))
    }

    async fn get_system_info(
        &self,
        request: Request<SystemInfoReq>,
    ) -> Result<Response<SystemInfoResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.get_system_info(req).await
            .map_err(|e| map_emby_error("get_system_info", e))?;
        Ok(Response::new(resp))
    }

    async fn fs_list(&self, request: Request<FsListReq>) -> Result<Response<FsListResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.fs_list(req).await
            .map_err(|e| map_emby_error("fs_list", e))?;
        Ok(Response::new(resp))
    }

    async fn logout(&self, request: Request<LogoutReq>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.logout(req).await
            .map_err(|e| map_emby_error("logout", e))?;
        Ok(Response::new(resp))
    }

    async fn playback_info(
        &self,
        request: Request<PlaybackInfoReq>,
    ) -> Result<Response<PlaybackInfoResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.playback_info(req).await
            .map_err(|e| map_emby_error("playback_info", e))?;
        Ok(Response::new(resp))
    }

    async fn delete_active_encodings(
        &self,
        request: Request<DeleteActiveEncodingsReq>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;
        let resp = self.service.delete_active_encodings(req).await
            .map_err(|e| map_emby_error("delete_active_encodings", e))?;
        Ok(Response::new(resp))
    }
}
