//! Alist gRPC Server Implementation
//!
//! Thin wrapper around `AlistService` that implements gRPC server trait.

use super::alist::{
    alist_server::Alist, FsGetReq, FsGetResp, FsListReq, FsListResp, FsOtherReq, FsOtherResp,
    FsSearchReq, FsSearchResp, LoginReq, LoginResp, MeReq, MeResp,
};
use super::validation::validate_host_with_dns;
use crate::alist::{AlistInterface, AlistService as AlistServiceImpl};
use crate::alist::error::AlistError;
use tonic::{Request, Response, Status};

/// Map Alist errors to appropriate gRPC status codes instead of leaking internals.
fn map_alist_error(context: &str, e: AlistError) -> Status {
    match e {
        AlistError::Auth(_) => Status::unauthenticated(format!("{context}: authentication failed")),
        AlistError::Http { status, .. } => {
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
        AlistError::Network(_) => Status::unavailable(format!("{context}: network error")),
        AlistError::Parse(_) => Status::internal(format!("{context}: failed to parse response")),
        AlistError::Api { code, .. } => {
            if code == 401 || code == 403 {
                Status::permission_denied(format!("{context}: access denied"))
            } else if code == 404 {
                Status::not_found(format!("{context}: resource not found"))
            } else {
                Status::internal(format!("{context}: API error (code {code})"))
            }
        }
        AlistError::InvalidConfig(_) => Status::invalid_argument(format!("{context}: invalid configuration")),
        AlistError::InvalidHeader(_) => Status::internal(format!("{context}: invalid header")),
        AlistError::NotImplemented(_) => Status::unimplemented(format!("{context}: not implemented")),
        AlistError::ResponseTooLarge { size } => Status::resource_exhausted(format!("{context}: response too large ({size} bytes)")),
    }
}

/// Alist gRPC server
///
/// Thin wrapper that delegates to `AlistService` for actual implementation.
pub struct AlistService {
    service: AlistServiceImpl,
}

impl AlistService {
    #[must_use] 
    pub const fn new() -> Self {
        Self {
            service: AlistServiceImpl::new(),
        }
    }
}

impl Default for AlistService {
    fn default() -> Self {
        Self::new()
    }
}

#[tonic::async_trait]
impl Alist for AlistService {
    async fn login(&self, request: Request<LoginReq>) -> Result<Response<LoginResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;

        let token = self
            .service
            .login(req)
            .await
            .map_err(|e| map_alist_error("login", e))?;

        Ok(Response::new(LoginResp { token }))
    }

    async fn me(&self, request: Request<MeReq>) -> Result<Response<MeResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;

        let resp = self
            .service
            .me(req)
            .await
            .map_err(|e| map_alist_error("me", e))?;

        Ok(Response::new(resp))
    }

    async fn fs_get(&self, request: Request<FsGetReq>) -> Result<Response<FsGetResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;

        let resp = self
            .service
            .fs_get(req)
            .await
            .map_err(|e| map_alist_error("fs_get", e))?;

        Ok(Response::new(resp))
    }

    async fn fs_list(&self, request: Request<FsListReq>) -> Result<Response<FsListResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;

        let resp = self
            .service
            .fs_list(req)
            .await
            .map_err(|e| map_alist_error("fs_list", e))?;

        Ok(Response::new(resp))
    }

    async fn fs_other(
        &self,
        request: Request<FsOtherReq>,
    ) -> Result<Response<FsOtherResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;

        let resp = self
            .service
            .fs_other(req)
            .await
            .map_err(|e| map_alist_error("fs_other", e))?;

        Ok(Response::new(resp))
    }

    async fn fs_search(
        &self,
        request: Request<FsSearchReq>,
    ) -> Result<Response<FsSearchResp>, Status> {
        let req = request.into_inner();
        validate_host_with_dns(&req.host).await?;

        let resp = self
            .service
            .fs_search(req)
            .await
            .map_err(|e| map_alist_error("fs_search", e))?;

        Ok(Response::new(resp))
    }
}
