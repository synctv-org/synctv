use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_truenas_login(
        &self,
        request: Request<crate::proto::TruenasLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.truenas_api
                .login(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_truenas_list(
        &self,
        request: Request<crate::proto::TruenasListRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.truenas_api
                .list(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_truenas_logout(
        &self,
        request: Request<crate::proto::TruenasLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.truenas_api
                .logout(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_truenas_get_binds(
        &self,
        request: Request<crate::proto::TruenasGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::truenas::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.truenas_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
