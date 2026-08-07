use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_seafile_login(
        &self,
        request: Request<crate::proto::SeafileLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.seafile_api
                .login(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_seafile_unlock_library(
        &self,
        request: Request<crate::proto::SeafileUnlockLibraryRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::UnlockLibraryResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.seafile_api
                .unlock_library(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_seafile_list_repositories(
        &self,
        request: Request<crate::proto::SeafileListRepositoriesRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.seafile_api
                .list_repositories(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_seafile_list(
        &self,
        request: Request<crate::proto::SeafileListRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.seafile_api
                .list(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_seafile_list_starred(
        &self,
        request: Request<crate::proto::SeafileListStarredRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.seafile_api
                .list_starred(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_seafile_logout(
        &self,
        request: Request<crate::proto::SeafileLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.seafile_api
                .logout(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_seafile_get_binds(
        &self,
        request: Request<crate::proto::SeafileGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::seafile::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.seafile_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
