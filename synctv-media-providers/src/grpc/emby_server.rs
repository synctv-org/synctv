//! Emby gRPC Server Implementation
//!
//! Thin wrapper around `EmbyService` that implements gRPC server trait.

use super::emby::{
    emby_server::Emby, DeleteActiveEncodingsReq, Empty, FsListReq, FsListResp, GetItemReq,
    GetItemsReq, GetItemsResp, Item, LoginReq, LoginResp, LogoutReq, MeReq, MeResp,
    PlaybackInfoReq, PlaybackInfoResp, ReportPlaybackProgressReq, ReportPlaybackStartReq,
    ReportPlaybackStopReq, SystemInfoReq, SystemInfoResp,
};
use super::error_mapper::map_provider_error;
use crate::emby::{EmbyInterface, EmbyService as EmbyServiceImpl};
use crate::validation::{
    validate_provider_auth, validate_provider_grpc_host, validate_provider_user_auth,
    validate_required,
};
use tonic::{Request, Response, Status};

/// Emby gRPC server
///
/// Thin wrapper that delegates to `EmbyService` for actual implementation.
pub struct EmbyService {
    service: EmbyServiceImpl,
}

#[allow(clippy::result_large_err)]
fn validate_login_credential(
    credential: Option<&super::emby::login_req::Credential>,
) -> Result<(), Status> {
    match credential {
        Some(super::emby::login_req::Credential::Password(_)) => Ok(()),
        Some(super::emby::login_req::Credential::ApiKey(api_key)) => {
            validate_required("api_key", api_key)
        }
        None => Err(Status::invalid_argument(
            "exactly one of password or api_key must be provided",
        )),
    }
}

impl EmbyService {
    pub fn new() -> Result<Self, reqwest::Error> {
        Ok(Self {
            service: EmbyServiceImpl::new()?,
        })
    }
}

#[tonic::async_trait]
impl Emby for EmbyService {
    async fn login(&self, request: Request<LoginReq>) -> Result<Response<LoginResp>, Status> {
        let req = request.into_inner();
        validate_provider_grpc_host(&req.host)?;
        validate_required("username", &req.username)?;
        validate_login_credential(req.credential.as_ref())?;
        let resp = self
            .service
            .login(req)
            .await
            .map_err(|e| map_provider_error("login", &e))?;
        Ok(Response::new(resp))
    }

    async fn me(&self, request: Request<MeReq>) -> Result<Response<MeResp>, Status> {
        let req = request.into_inner();
        validate_provider_auth(&req.host, &req.token)?;
        let resp = self
            .service
            .me(req)
            .await
            .map_err(|e| map_provider_error("me", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_items(
        &self,
        request: Request<GetItemsReq>,
    ) -> Result<Response<GetItemsResp>, Status> {
        let req = request.into_inner();
        validate_provider_user_auth(&req.host, &req.token, &req.user_id)?;
        let resp = self
            .service
            .get_items(req)
            .await
            .map_err(|e| map_provider_error("get_items", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_item(&self, request: Request<GetItemReq>) -> Result<Response<Item>, Status> {
        let req = request.into_inner();
        validate_provider_user_auth(&req.host, &req.token, &req.user_id)?;
        validate_required("item_id", &req.item_id)?;
        let resp = self
            .service
            .get_item(req)
            .await
            .map_err(|e| map_provider_error("get_item", &e))?;
        Ok(Response::new(resp))
    }

    async fn get_system_info(
        &self,
        request: Request<SystemInfoReq>,
    ) -> Result<Response<SystemInfoResp>, Status> {
        let req = request.into_inner();
        validate_provider_auth(&req.host, &req.token)?;
        let resp = self
            .service
            .get_system_info(req)
            .await
            .map_err(|e| map_provider_error("get_system_info", &e))?;
        Ok(Response::new(resp))
    }

    async fn fs_list(&self, request: Request<FsListReq>) -> Result<Response<FsListResp>, Status> {
        let req = request.into_inner();
        validate_provider_user_auth(&req.host, &req.token, &req.user_id)?;
        let resp = self
            .service
            .fs_list(req)
            .await
            .map_err(|e| map_provider_error("fs_list", &e))?;
        Ok(Response::new(resp))
    }

    async fn logout(&self, request: Request<LogoutReq>) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        validate_provider_auth(&req.host, &req.token)?;
        let resp = self
            .service
            .logout(req)
            .await
            .map_err(|e| map_provider_error("logout", &e))?;
        Ok(Response::new(resp))
    }

    async fn playback_info(
        &self,
        request: Request<PlaybackInfoReq>,
    ) -> Result<Response<PlaybackInfoResp>, Status> {
        let req = request.into_inner();
        validate_provider_user_auth(&req.host, &req.token, &req.user_id)?;
        validate_required("item_id", &req.item_id)?;
        let resp = self
            .service
            .playback_info(req)
            .await
            .map_err(|e| map_provider_error("playback_info", &e))?;
        Ok(Response::new(resp))
    }

    async fn delete_active_encodings(
        &self,
        request: Request<DeleteActiveEncodingsReq>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        validate_provider_auth(&req.host, &req.token)?;
        let resp = self
            .service
            .delete_active_encodings(req)
            .await
            .map_err(|e| map_provider_error("delete_active_encodings", &e))?;
        Ok(Response::new(resp))
    }

    async fn report_playback_start(
        &self,
        request: Request<ReportPlaybackStartReq>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        validate_provider_auth(&req.host, &req.token)?;
        validate_required("item_id", &req.item_id)?;
        let resp = self
            .service
            .report_playback_start(req)
            .await
            .map_err(|e| map_provider_error("report_playback_start", &e))?;
        Ok(Response::new(resp))
    }

    async fn report_playback_stop(
        &self,
        request: Request<ReportPlaybackStopReq>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        validate_provider_auth(&req.host, &req.token)?;
        validate_required("item_id", &req.item_id)?;
        let resp = self
            .service
            .report_playback_stop(req)
            .await
            .map_err(|e| map_provider_error("report_playback_stop", &e))?;
        Ok(Response::new(resp))
    }

    async fn report_playback_progress(
        &self,
        request: Request<ReportPlaybackProgressReq>,
    ) -> Result<Response<Empty>, Status> {
        let req = request.into_inner();
        validate_provider_auth(&req.host, &req.token)?;
        validate_required("item_id", &req.item_id)?;
        let resp = self
            .service
            .report_playback_progress(req)
            .await
            .map_err(|e| map_provider_error("report_playback_progress", &e))?;
        Ok(Response::new(resp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_credential_allows_explicit_empty_password() {
        let credential = super::super::emby::login_req::Credential::Password(String::new());

        assert!(validate_login_credential(Some(&credential)).is_ok());
    }

    #[test]
    fn login_credential_still_rejects_missing_credentials_and_empty_api_keys() {
        assert!(validate_login_credential(None).is_err());

        let credential = super::super::emby::login_req::Credential::ApiKey(String::new());
        assert!(validate_login_credential(Some(&credential)).is_err());
    }
}
