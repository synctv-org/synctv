use super::super::{map_classified_result, ManagementServiceImpl};
use tonic::{Request, Response, Status};

impl ManagementServiceImpl {
    pub(crate) async fn provider_douyu_resolve(
        &self,
        request: Request<crate::proto::DouyuResolveRequest>,
    ) -> Result<Response<synctv_proto::providers::douyu::ResolveResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.douyu_api
                .resolve(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
