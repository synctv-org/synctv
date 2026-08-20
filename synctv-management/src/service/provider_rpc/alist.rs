use super::super::*;

impl ManagementServiceImpl {
    pub(crate) async fn provider_alist_login(
        &self,
        request: Request<AlistLoginRequest>,
    ) -> Result<Response<alist_proto::LoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.alist_api
                .login(
                    &actor_user_id,
                    Self::alist_login_command(provider_request),
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_alist_list(
        &self,
        request: Request<AlistListRequest>,
    ) -> Result<Response<alist_proto::ListResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.alist_api
                .list(
                    &actor_user_id,
                    AlistListQuery {
                        server_id: provider_request.server_id,
                        path: provider_request.path,
                        password: provider_request.password,
                        page: provider_request.page,
                        per_page: provider_request.per_page,
                        refresh: provider_request.refresh,
                    },
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_alist_search(
        &self,
        request: Request<AlistSearchRequest>,
    ) -> Result<Response<alist_proto::SearchResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.alist_api
                .search(
                    &actor_user_id,
                    AlistSearchQuery {
                        server_id: provider_request.server_id,
                        parent: provider_request.parent,
                        keywords: provider_request.keywords,
                        scope: provider_request.scope,
                        page: provider_request.page,
                        per_page: provider_request.per_page,
                        password: provider_request.password,
                    },
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_alist_get_me(
        &self,
        request: Request<AlistGetMeRequest>,
    ) -> Result<Response<alist_proto::GetMeResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.alist_api
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

    pub(crate) async fn provider_alist_logout(
        &self,
        request: Request<AlistLogoutRequest>,
    ) -> Result<Response<alist_proto::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.alist_api
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

    pub(crate) async fn provider_alist_get_binds(
        &self,
        request: Request<AlistGetBindsRequest>,
    ) -> Result<Response<alist_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.alist_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }
}
