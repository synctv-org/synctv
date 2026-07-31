use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_youtube_bind(
        &self,
        request: Request<crate::proto::YoutubeBindRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::BindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.youtube_api
                .bind(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_youtube_get_binds(
        &self,
        request: Request<crate::proto::YoutubeGetBindsRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.youtube_api
                .get_binds(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_youtube_unbind(
        &self,
        request: Request<crate::proto::YoutubeUnbindRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::UnbindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.youtube_api
                .unbind(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_youtube_resolve(
        &self,
        request: Request<crate::proto::YoutubeResolveRequest>,
    ) -> Result<Response<synctv_proto::providers::youtube::ResolveResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.youtube_api
                .resolve(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
