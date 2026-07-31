use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_nextcloud_login(
        &self,
        request: Request<crate::proto::NextcloudLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.nextcloud_api
                .login(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_nextcloud_start_login_flow(
        &self,
        request: Request<crate::proto::NextcloudStartLoginFlowRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::StartLoginFlowResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.nextcloud_api
                .start_login_flow(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_nextcloud_poll_login_flow(
        &self,
        request: Request<crate::proto::NextcloudPollLoginFlowRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.nextcloud_api
                .poll_login_flow(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_nextcloud_list(
        &self,
        request: Request<crate::proto::NextcloudListRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.nextcloud_api
                .list(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_nextcloud_list_favorites(
        &self,
        request: Request<crate::proto::NextcloudListFavoritesRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.nextcloud_api
                .list_favorites(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_nextcloud_logout(
        &self,
        request: Request<crate::proto::NextcloudLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.nextcloud_api
                .logout(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_nextcloud_get_binds(
        &self,
        request: Request<crate::proto::NextcloudGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::nextcloud::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.nextcloud_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
