use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_fnos_login(
        &self,
        request: Request<crate::proto::FnosLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response =
            map_classified_result(self.fnos_api.login(&actor_user_id, provider_request).await)?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_list(
        &self,
        request: Request<crate::proto::FnosListRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response =
            map_classified_result(self.fnos_api.list(&actor_user_id, provider_request).await)?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_list_media_libraries(
        &self,
        request: Request<crate::proto::FnosListMediaLibrariesRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::ListMediaLibrariesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.fnos_api
                .list_media_libraries(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_list_media_items(
        &self,
        request: Request<crate::proto::FnosListMediaItemsRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::ListMediaItemsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.fnos_api
                .list_media_items(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_set_favorite(
        &self,
        request: Request<crate::proto::FnosSetFavoriteRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::SetFavoriteResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.fnos_api
                .set_favorite(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_set_watched(
        &self,
        request: Request<crate::proto::FnosSetWatchedRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::SetWatchedResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.fnos_api
                .set_watched(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_get_server_info(
        &self,
        request: Request<crate::proto::FnosGetServerInfoRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::GetServerInfoResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.fnos_api
                .get_server_info(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_logout(
        &self,
        request: Request<crate::proto::FnosLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response =
            map_classified_result(self.fnos_api.logout(&actor_user_id, provider_request).await)?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_fnos_get_binds(
        &self,
        request: Request<crate::proto::FnosGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::fnos::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.fnos_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
