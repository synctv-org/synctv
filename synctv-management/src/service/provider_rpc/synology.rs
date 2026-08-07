use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_synology_login(
        &self,
        request: Request<crate::proto::SynologyLoginRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .login(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_list_files(
        &self,
        request: Request<crate::proto::SynologyListFilesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListFilesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .list_files(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_list_libraries(
        &self,
        request: Request<crate::proto::SynologyListLibrariesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListLibrariesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .list_libraries(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_list_movies(
        &self,
        request: Request<crate::proto::SynologyListMoviesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .list_movies(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_list_tv_shows(
        &self,
        request: Request<crate::proto::SynologyListTvShowsRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .list_tv_shows(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_list_episodes(
        &self,
        request: Request<crate::proto::SynologyListEpisodesRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .list_episodes(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_list_home_videos(
        &self,
        request: Request<crate::proto::SynologyListHomeVideosRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .list_home_videos(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_list_tv_recordings(
        &self,
        request: Request<crate::proto::SynologyListTvRecordingsRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::ListVideoItemsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .list_tv_recordings(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_logout(
        &self,
        request: Request<crate::proto::SynologyLogoutRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .logout(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_synology_get_binds(
        &self,
        request: Request<crate::proto::SynologyGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::synology::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.synology_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
