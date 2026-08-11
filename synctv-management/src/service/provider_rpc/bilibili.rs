use super::super::*;

impl ManagementServiceImpl {
    pub(crate) async fn provider_bilibili_list_playlist(
        &self,
        request: Request<crate::proto::BilibiliListPlaylistRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListPlaylistResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .list_playlist(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_list_live_areas(
        &self,
        request: Request<crate::proto::BilibiliListLiveAreasRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListLiveAreasResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (_actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response =
            map_classified_result(self.bilibili_api.list_live_areas(provider_request).await)?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_list_favorite_folders(
        &self,
        request: Request<crate::proto::BilibiliListFavoriteFoldersRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListFavoriteFoldersResponse>, Status>
    {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .list_favorite_folders(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_list_followed_pgc(
        &self,
        request: Request<crate::proto::BilibiliListFollowedPgcRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListFollowedPgcResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .list_followed_pgc(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_list_history(
        &self,
        request: Request<crate::proto::BilibiliListHistoryRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListHistoryResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .list_history(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_list_pgc_timeline(
        &self,
        request: Request<crate::proto::BilibiliListPgcTimelineRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListPgcTimelineResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .list_pgc_timeline(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_list_pgc_seasons(
        &self,
        request: Request<crate::proto::BilibiliListPgcSeasonsRequest>,
    ) -> Result<Response<synctv_proto::providers::bilibili::ListPgcSeasonsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let request = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(request.actor, request.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .list_pgc_seasons(&actor_user_id, provider_request)
                .await,
        )?;
        Ok(Response::new(response))
    }
    pub(crate) async fn provider_bilibili_parse(
        &self,
        request: Request<BilibiliParseRequest>,
    ) -> Result<Response<bilibili_proto::ParseResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.bilibili_api
                .parse(
                    &actor_user_id,
                    BilibiliParseQuery {
                        url: provider_request.url,
                        shared: provider_request.shared,
                    },
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_login_qr(
        &self,
        request: Request<BilibiliLoginQrRequest>,
    ) -> Result<Response<bilibili_proto::QrCodeResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (_actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.bilibili_api
                .login_qr(BilibiliLoginQrCommand, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_check_qr(
        &self,
        request: Request<BilibiliCheckQrRequest>,
    ) -> Result<Response<bilibili_proto::QrStatusResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.bilibili_api
                .check_qr(
                    &actor_user_id,
                    BilibiliCheckQrQuery {
                        key: provider_request.key,
                    },
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_start_sms_login(
        &self,
        request: Request<BilibiliStartSmsLoginRequest>,
    ) -> Result<Response<bilibili_proto::StartSmsLoginResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (_actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.bilibili_api
                .start_sms_login(BilibiliStartSmsLoginCommand, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_send_sms(
        &self,
        request: Request<BilibiliSendSmsRequest>,
    ) -> Result<Response<bilibili_proto::SendSmsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (_actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .send_sms(BilibiliSendSmsCommand {
                    session_token: provider_request.session_token,
                    phone: provider_request.phone,
                    validate: provider_request.validate,
                })
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_login_sms(
        &self,
        request: Request<BilibiliLoginSmsRequest>,
    ) -> Result<Response<bilibili_proto::LoginSmsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .login_sms(
                    &actor_user_id,
                    BilibiliLoginSmsCommand {
                        session_token: provider_request.session_token,
                        code: provider_request.code,
                    },
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_get_user_info(
        &self,
        request: Request<BilibiliGetUserInfoRequest>,
    ) -> Result<Response<bilibili_proto::UserInfoResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_classified_result(
            self.bilibili_api
                .get_user_info(
                    &actor_user_id,
                    BilibiliUserInfoQuery,
                    instance_name.as_deref(),
                )
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_logout(
        &self,
        request: Request<BilibiliLogoutRequest>,
    ) -> Result<Response<bilibili_proto::LogoutResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, _provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let response = map_classified_result(
            self.bilibili_api
                .logout(&actor_user_id, BilibiliLogoutCommand)
                .await,
        )?;
        Ok(Response::new(response))
    }

    pub(crate) async fn provider_bilibili_get_binds(
        &self,
        request: Request<BilibiliGetBindsRequest>,
    ) -> Result<Response<bilibili_proto::GetBindsResponse>, Status> {
        self.check_admin_get_validated(&request)?;
        let req = request.into_inner();
        let (actor_user_id, provider_request) = self
            .resolve_client_actor_and_request(req.actor, req.request)
            .await?;
        let instance_name = Self::optional_instance_name(&provider_request.instance_name);
        let response = map_api_result(
            self.bilibili_api
                .get_binds(&actor_user_id, instance_name.as_deref())
                .await,
        )?;
        Ok(Response::new(response))
    }
}
