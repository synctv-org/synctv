use super::super::*;

impl ManagementServiceImpl {
    pub(crate) async fn provider_emby_login(
        &self,
        request: Request<EmbyLoginRequest>,
    ) -> Result<Response<emby_proto::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.emby_api
                .login(
                    &actor_user_id,
                    Self::emby_login_command(provider_request),
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_emby_list(
        &self,
        request: Request<EmbyListRequest>,
    ) -> Result<Response<emby_proto::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.emby_api
                .list(
                    &actor_user_id,
                    EmbyListQuery {
                        server_id: provider_request.server_id,
                        path: provider_request.path,
                        start_index: provider_request.start_index,
                        limit: provider_request.limit,
                        search_term: provider_request.search_term,
                    },
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_emby_get_me(
        &self,
        request: Request<EmbyGetMeRequest>,
    ) -> Result<Response<emby_proto::GetMeResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.emby_api
                .get_me(
                    &actor_user_id,
                    ProviderCredentialServerQuery {
                        server_id: provider_request.server_id,
                    },
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_emby_logout(
        &self,
        request: Request<EmbyLogoutRequest>,
    ) -> Result<Response<emby_proto::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.emby_api
                .logout(
                    &actor_user_id,
                    ProviderCredentialServerQuery {
                        server_id: provider_request.server_id,
                    },
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_emby_get_binds(
        &self,
        request: Request<EmbyGetBindsRequest>,
    ) -> Result<Response<emby_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_api_result(
            self.emby_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }
}
