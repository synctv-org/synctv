//! Alist gRPC Server Implementation
//!
//! Thin wrapper around `AlistService` that implements gRPC server trait.

use super::alist::{
    alist_server::Alist, FsGetReq, FsGetResp, FsListReq, FsListResp, FsOtherReq, FsOtherResp,
    FsSearchReq, FsSearchResp, LoginReq, LoginResp, MeReq, MeResp,
};
use super::error_mapper::map_provider_error;
use super::validation::{validate_provider_grpc_host, validate_required};
use crate::alist::error::AlistError;
use crate::alist::{AlistInterface, AlistService as AlistServiceImpl};
use tonic::{Request, Response, Status};

/// Map Alist errors to appropriate gRPC status codes using the shared mapper.
fn map_alist_error(context: &str, e: &AlistError) -> Status {
    map_provider_error(context, e)
}

/// Alist gRPC server
///
/// Thin wrapper that delegates to `AlistService` for actual implementation.
pub struct AlistService {
    service: AlistServiceImpl,
}

impl AlistService {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            service: AlistServiceImpl::new()?,
        })
    }
}

#[tonic::async_trait]
impl Alist for AlistService {
    async fn login(&self, request: Request<LoginReq>) -> Result<Response<LoginResp>, Status> {
        let req = request.into_inner();
        validate_provider_grpc_host(&req.host)?;
        validate_required("username", &req.username)?;
        match req.credential.as_ref() {
            Some(super::alist::login_req::Credential::Password(password)) => {
                validate_required("password", password)?;
            }
            Some(super::alist::login_req::Credential::HashedPassword(hashed_password)) => {
                validate_required("hashed_password", hashed_password)?;
            }
            None => {
                return Err(Status::invalid_argument(
                    "exactly one of password or hashed_password must be provided",
                ));
            }
        }

        let token = self
            .service
            .login(req)
            .await
            .map_err(|e| map_alist_error("login", &e))?;

        Ok(Response::new(LoginResp { token }))
    }

    async fn me(&self, request: Request<MeReq>) -> Result<Response<MeResp>, Status> {
        let req = request.into_inner();
        validate_provider_grpc_host(&req.host)?;
        validate_required("token", &req.token)?;

        let resp = self
            .service
            .me(req)
            .await
            .map_err(|e| map_alist_error("me", &e))?;

        Ok(Response::new(resp))
    }

    async fn fs_get(&self, request: Request<FsGetReq>) -> Result<Response<FsGetResp>, Status> {
        let req = request.into_inner();
        validate_provider_grpc_host(&req.host)?;
        validate_required("token", &req.token)?;

        let resp = self
            .service
            .fs_get(req)
            .await
            .map_err(|e| map_alist_error("fs_get", &e))?;

        Ok(Response::new(resp))
    }

    async fn fs_list(&self, request: Request<FsListReq>) -> Result<Response<FsListResp>, Status> {
        let req = request.into_inner();
        validate_provider_grpc_host(&req.host)?;
        validate_required("token", &req.token)?;

        let resp = self
            .service
            .fs_list(req)
            .await
            .map_err(|e| map_alist_error("fs_list", &e))?;

        Ok(Response::new(resp))
    }

    async fn fs_other(
        &self,
        request: Request<FsOtherReq>,
    ) -> Result<Response<FsOtherResp>, Status> {
        let req = request.into_inner();
        validate_provider_grpc_host(&req.host)?;
        validate_required("token", &req.token)?;
        validate_required("method", &req.method)?;

        let resp = self
            .service
            .fs_other(req)
            .await
            .map_err(|e| map_alist_error("fs_other", &e))?;

        Ok(Response::new(resp))
    }

    async fn fs_search(
        &self,
        request: Request<FsSearchReq>,
    ) -> Result<Response<FsSearchResp>, Status> {
        let req = request.into_inner();
        validate_provider_grpc_host(&req.host)?;
        validate_required("token", &req.token)?;

        let resp = self
            .service
            .fs_search(req)
            .await
            .map_err(|e| map_alist_error("fs_search", &e))?;

        Ok(Response::new(resp))
    }
}
