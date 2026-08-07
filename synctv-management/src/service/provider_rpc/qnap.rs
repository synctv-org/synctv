use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_qnap_login(
        &self,
        request: Request<crate::proto::QnapLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response =
            map_classified_result(self.qnap_api.login(&actor_user_id, provider_request).await)?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_qnap_list(
        &self,
        request: Request<crate::proto::QnapListRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response =
            map_classified_result(self.qnap_api.list(&actor_user_id, provider_request).await)?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_qnap_get_capabilities(
        &self,
        request: Request<crate::proto::QnapGetCapabilitiesRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::GetCapabilitiesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.qnap_api
                .get_capabilities(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_qnap_logout(
        &self,
        request: Request<crate::proto::QnapLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response =
            map_classified_result(self.qnap_api.logout(&actor_user_id, provider_request).await)?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_qnap_get_binds(
        &self,
        request: Request<crate::proto::QnapGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::qnap::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.qnap_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
