use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_cloudreve_login(
        &self,
        request: Request<crate::proto::CloudreveLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.cloudreve_api
                .login(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_cloudreve_list(
        &self,
        request: Request<crate::proto::CloudreveListRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.cloudreve_api
                .list(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_cloudreve_search(
        &self,
        request: Request<crate::proto::CloudreveSearchRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::SearchResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.cloudreve_api
                .search(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_cloudreve_get_me(
        &self,
        request: Request<crate::proto::CloudreveGetMeRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::GetMeResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.cloudreve_api
                .get_me(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_cloudreve_logout(
        &self,
        request: Request<crate::proto::CloudreveLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.cloudreve_api
                .logout(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_cloudreve_get_binds(
        &self,
        request: Request<crate::proto::CloudreveGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::cloudreve::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.cloudreve_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
