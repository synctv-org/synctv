use super::super::*;

impl ManagementServiceImpl {
    pub(crate) async fn provider_tik_tok_bind(
        &self,
        request: Request<TikTokBindRequest>,
    ) -> Result<Response<tiktok_proto::BindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.tiktok_api
                .bind(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_tik_tok_get_binds(
        &self,
        request: Request<TikTokGetBindsRequest>,
    ) -> Result<Response<tiktok_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.tiktok_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_tik_tok_unbind(
        &self,
        request: Request<TikTokUnbindRequest>,
    ) -> Result<Response<tiktok_proto::UnbindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.tiktok_api
                .unbind(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_tik_tok_resolve(
        &self,
        request: Request<TikTokResolveRequest>,
    ) -> Result<Response<tiktok_proto::ResolveResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.tiktok_api
                .resolve(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_tik_tok_get_user(
        &self,
        request: Request<TikTokGetUserRequest>,
    ) -> Result<Response<tiktok_proto::GetUserResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.tiktok_api
                .get_user(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_tik_tok_list_user_posts(
        &self,
        request: Request<TikTokListUserPostsRequest>,
    ) -> Result<Response<tiktok_proto::ListUserPostsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.tiktok_api
                .list_user_posts(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }
}
