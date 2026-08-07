use super::super::*;

impl ManagementServiceImpl {
    pub(crate) async fn provider_douyin_bind(
        &self,
        request: Request<DouyinBindRequest>,
    ) -> Result<Response<douyin_proto::BindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.douyin_api
                .bind(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_douyin_get_binds(
        &self,
        request: Request<DouyinGetBindsRequest>,
    ) -> Result<Response<douyin_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.douyin_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_douyin_unbind(
        &self,
        request: Request<DouyinUnbindRequest>,
    ) -> Result<Response<douyin_proto::UnbindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.douyin_api
                .unbind(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_douyin_resolve(
        &self,
        request: Request<DouyinResolveRequest>,
    ) -> Result<Response<douyin_proto::ResolveResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.douyin_api
                .resolve(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_douyin_list_user_posts(
        &self,
        request: Request<DouyinListUserPostsRequest>,
    ) -> Result<Response<douyin_proto::ListUserPostsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.douyin_api
                .list_user_posts(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }
}
