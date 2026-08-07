use super::super::*;

impl ManagementServiceImpl {
    pub(crate) async fn provider_twitch_bind(
        &self,
        request: Request<TwitchBindRequest>,
    ) -> Result<Response<twitch_proto::BindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.twitch_api
                .bind(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_get_binds(
        &self,
        request: Request<TwitchGetBindsRequest>,
    ) -> Result<Response<twitch_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.twitch_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_unbind(
        &self,
        request: Request<TwitchUnbindRequest>,
    ) -> Result<Response<twitch_proto::UnbindResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.twitch_api
                .unbind(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_resolve(
        &self,
        request: Request<TwitchResolveRequest>,
    ) -> Result<Response<twitch_proto::ResolveResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.twitch_api
                .resolve(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_list_channel_items(
        &self,
        request: Request<TwitchListChannelItemsRequest>,
    ) -> Result<Response<twitch_proto::ListChannelItemsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, mut provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        provider_request.instance_name.clear();
        let response = map_classified_result(
            self.twitch_api
                .list_channel_items(&actor_user_id, provider_request, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_list_followed_live(
        &self,
        request: Request<crate::proto::TwitchListFollowedLiveRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListFollowedLiveResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.twitch_api
                .list_followed_live(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_list_category_streams(
        &self,
        request: Request<crate::proto::TwitchListCategoryStreamsRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListCategoryStreamsResponse>, Status>
    {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.twitch_api
                .list_category_streams(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_list_top_categories(
        &self,
        request: Request<crate::proto::TwitchListTopCategoriesRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListTopCategoriesResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.twitch_api
                .list_top_categories(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_search_live_channels(
        &self,
        request: Request<crate::proto::TwitchSearchLiveChannelsRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::SearchLiveChannelsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.twitch_api
                .search_live_channels(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_twitch_list_schedule(
        &self,
        request: Request<crate::proto::TwitchListScheduleRequest>,
    ) -> Result<Response<synctv_proto::providers::twitch::ListScheduleResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.twitch_api
                .list_schedule(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
}
