use futures::StreamExt;
use tonic::{Request, Response, Status};

use super::{map_api_error, ClientServiceImpl};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::{
    user_service_server::UserService, CloseAccountRequest, CloseAccountResponse,
    CompleteUserAvatarUploadSessionRequest, CompleteUserAvatarUploadSessionResponse,
    ConfirmEmailBindRequest, CreateRoomRequest, CreateUserAvatarUploadSessionRequest,
    CreateUserAvatarUploadSessionResponse, DeletePasskeyRequest, DeletePasskeyResponse,
    DeleteTotpRequest, DeleteTotpResponse, DiscoverRoomsRequest, DiscoverRoomsResponse,
    FavoriteRoomRequest, FavoriteRoomResponse, FinishOpaquePasswordUpdateRequest,
    FinishPasskeyBindRequest, FinishRoomPasswordLoginRequest,
    FinishSensitiveOperationVerificationRequest, FinishTotpSetupRequest, GetProfileRequest,
    GetRoomDiscoveryRequest, GetRoomRequest, GetRoomResponse, GetUserAvatarObjectRequest,
    GetUserPreferencesRequest, GetUserPreferencesResponse, JoinRoomRequest, JoinRoomResponse,
    ListFavoriteRoomsRequest, ListFavoriteRoomsResponse, ListMyRoomsRequest, ListMyRoomsResponse,
    ListPasskeysRequest, ListPasskeysResponse, LogoutRequest, LogoutResponse, PasskeyCredential,
    RegenerateTotpRecoveryCodesRequest, RequestSensitiveOperationEmailCodeRequest,
    RequestSensitiveOperationEmailCodeResponse, Room, RoomDiscoveryItem,
    SensitiveOperationVerificationOutcome, SetTwoFactorEnabledRequest, SetUsernameRequest,
    StartEmailBindRequest, StartEmailBindResponse, StartOpaquePasswordUpdateRequest,
    StartOpaquePasswordUpdateResponse, StartPasskeyBindRequest, StartPasskeyBindResponse,
    StartRoomPasswordLoginRequest, StartRoomPasswordLoginResponse,
    StartSensitiveOperationPasskeyRequest, StartSensitiveOperationPasskeyResponse,
    StartSensitiveOperationVerificationRequest, StartTotpSetupRequest, StartTotpSetupResponse,
    TotpRecoveryCodesResponse, UnbindEmailRequest, UnfavoriteRoomRequest, UnfavoriteRoomResponse,
    UpdateUserAvatarRequest, UpdateUserPreferencesRequest, UpdateUserPreferencesResponse,
    UploadUserAvatarObjectRequest, UploadUserAvatarObjectResponse, User, UserAvatarObjectResponse,
};

type UserAvatarObjectStream = super::GrpcStatusStream<UserAvatarObjectResponse>;

#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
#[allow(clippy::result_large_err)]
impl UserService for ClientServiceImpl {
    type GetUserAvatarObjectStream = UserAvatarObjectStream;

    async fn logout(
        &self,
        request: Request<LogoutRequest>,
    ) -> Result<Response<LogoutResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let authorization = metadata.authorization.clone();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |_| async move {
                    let auth_value = authorization.ok_or_else(|| {
                        synctv_api_common::impls::ApiError::Authentication(
                            synctv_common::messages::AUTHENTICATION_REQUIRED.to_string(),
                        )
                    })?;
                    let token =
                        synctv_core::service::JwtValidator::extract_bearer_token(&auth_value)
                            .map_err(|_| {
                                synctv_api_common::impls::ApiError::Authentication(
                                    synctv_common::messages::INVALID_OR_EXPIRED_TOKEN.to_string(),
                                )
                            })?;
                    client_api.logout(&token).await?;
                    Ok::<(), synctv_api_common::impls::ApiError>(())
                },
            )
            .await
            .map_err(map_api_error)?;

        Ok(Response::new(LogoutResponse {
            success: true,
            message: String::new(),
        }))
    }

    async fn get_profile(
        &self,
        request: Request<GetProfileRequest>,
    ) -> Result<Response<User>, Status> {
        let metadata = self.request_metadata(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api.get_profile(&authenticated.user_id()).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_username(
        &self,
        request: Request<SetUsernameRequest>,
    ) -> Result<Response<User>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.set_username(&authenticated.user_id(), req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_user_avatar_upload_session(
        &self,
        request: Request<CreateUserAvatarUploadSessionRequest>,
    ) -> Result<Response<CreateUserAvatarUploadSessionResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .create_user_avatar_upload_session(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn upload_user_avatar_object(
        &self,
        request: Request<UploadUserAvatarObjectRequest>,
    ) -> Result<Response<UploadUserAvatarObjectResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
                let client_api = self.client_api.clone();
                async move { client_api.upload_user_avatar_object(req).await }
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn complete_user_avatar_upload_session(
        &self,
        request: Request<CompleteUserAvatarUploadSessionRequest>,
    ) -> Result<Response<CompleteUserAvatarUploadSessionResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let response = self
            .client_api
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
                let client_api = self.client_api.clone();
                async move { client_api.complete_user_avatar_upload_session(req).await }
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_user_avatar_object(
        &self,
        request: Request<GetUserAvatarObjectRequest>,
    ) -> Result<Response<Self::GetUserAvatarObjectStream>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let download = self
            .client_api
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, move || {
                let client_api = self.client_api.clone();
                async move { client_api.get_user_avatar_object(req).await }
            })
            .await
            .map_err(map_api_error)?;
        let stream = synctv_api_common::impls::client::file_download::avatar_chunk_stream(download)
            .map(|result| result.map_err(map_api_error));
        Ok(Response::new(Box::pin(stream)))
    }

    async fn update_user_avatar(
        &self,
        request: Request<UpdateUserAvatarRequest>,
    ) -> Result<Response<User>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .update_user_avatar(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn clear_user_avatar(
        &self,
        request: Request<synctv_proto::client::ClearUserAvatarRequest>,
    ) -> Result<Response<User>, Status> {
        let metadata = self.request_metadata(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.clear_user_avatar(&authenticated.user_id()).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_email_bind(
        &self,
        request: Request<StartEmailBindRequest>,
    ) -> Result<Response<StartEmailBindResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .start_email_bind(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn confirm_email_bind(
        &self,
        request: Request<ConfirmEmailBindRequest>,
    ) -> Result<Response<User>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .confirm_email_bind(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn unbind_email(
        &self,
        request: Request<UnbindEmailRequest>,
    ) -> Result<Response<User>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.unbind_email(&authenticated.user_id(), req).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_sensitive_operation_verification(
        &self,
        request: Request<StartSensitiveOperationVerificationRequest>,
    ) -> Result<Response<SensitiveOperationVerificationOutcome>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .start_sensitive_operation_verification(
                            &authenticated.user_id(),
                            authenticated.claims.auth_context(),
                            req,
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_sensitive_operation_passkey(
        &self,
        request: Request<StartSensitiveOperationPasskeyRequest>,
    ) -> Result<Response<StartSensitiveOperationPasskeyResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .start_sensitive_operation_passkey(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn request_sensitive_operation_email_code(
        &self,
        request: Request<RequestSensitiveOperationEmailCodeRequest>,
    ) -> Result<Response<RequestSensitiveOperationEmailCodeResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .request_sensitive_operation_email_code(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_sensitive_operation_verification(
        &self,
        request: Request<FinishSensitiveOperationVerificationRequest>,
    ) -> Result<Response<SensitiveOperationVerificationOutcome>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |request_control, authenticated| async move {
                    client_api
                        .finish_sensitive_operation_verification(
                            &authenticated.user_id(),
                            req,
                            client_ip,
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_opaque_password_update(
        &self,
        request: Request<StartOpaquePasswordUpdateRequest>,
    ) -> Result<Response<StartOpaquePasswordUpdateResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .start_opaque_password_update(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_opaque_password_update(
        &self,
        request: Request<FinishOpaquePasswordUpdateRequest>,
    ) -> Result<Response<User>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .finish_opaque_password_update(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_passkey_bind(
        &self,
        request: Request<StartPasskeyBindRequest>,
    ) -> Result<Response<StartPasskeyBindResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .start_passkey_bind(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_passkey_bind(
        &self,
        request: Request<FinishPasskeyBindRequest>,
    ) -> Result<Response<PasskeyCredential>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .finish_passkey_bind_request(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_passkeys(
        &self,
        request: Request<ListPasskeysRequest>,
    ) -> Result<Response<ListPasskeysResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api.list_passkeys(&authenticated.user_id()).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_passkey(
        &self,
        request: Request<DeletePasskeyRequest>,
    ) -> Result<Response<DeletePasskeyResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .delete_passkey(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_totp_setup(
        &self,
        request: Request<StartTotpSetupRequest>,
    ) -> Result<Response<StartTotpSetupResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |auth| async move { client_api.start_totp_setup(&auth.user_id(), req).await },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_totp_setup(
        &self,
        request: Request<FinishTotpSetupRequest>,
    ) -> Result<Response<TotpRecoveryCodesResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |auth| async move { client_api.finish_totp_setup(&auth.user_id(), req).await },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn regenerate_totp_recovery_codes(
        &self,
        request: Request<RegenerateTotpRecoveryCodesRequest>,
    ) -> Result<Response<TotpRecoveryCodesResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |auth| async move {
                    client_api
                        .regenerate_totp_recovery_codes(&auth.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn delete_totp(
        &self,
        request: Request<DeleteTotpRequest>,
    ) -> Result<Response<DeleteTotpResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |auth| async move { client_api.delete_totp(&auth.user_id(), req).await },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_user_preferences(
        &self,
        request: Request<GetUserPreferencesRequest>,
    ) -> Result<Response<GetUserPreferencesResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .get_user_preferences(&authenticated.user_id())
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn update_user_preferences(
        &self,
        request: Request<UpdateUserPreferencesRequest>,
    ) -> Result<Response<UpdateUserPreferencesResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .update_user_preferences(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn set_two_factor_enabled(
        &self,
        request: Request<SetTwoFactorEnabledRequest>,
    ) -> Result<Response<GetUserPreferencesResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .set_two_factor_enabled(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn close_account(
        &self,
        request: Request<CloseAccountRequest>,
    ) -> Result<Response<CloseAccountResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api.close_account(&authenticated.user_id()).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn create_room(
        &self,
        request: Request<CreateRoomRequest>,
    ) -> Result<Response<Room>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    Box::pin(client_api.create_room(&authenticated.user_id(), req)).await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room(
        &self,
        request: Request<GetRoomRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let room_id = req.room_id.clone();
        let response = self
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move { client_api.get_room_for_actor(&actor).await },
            )
            .await?;
        Ok(Response::new(response))
    }

    async fn join_room(
        &self,
        request: Request<JoinRoomRequest>,
    ) -> Result<Response<JoinRoomResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip.map(|ip| ip.to_string());
        let req = request.into_inner();
        let room_id = req.room_id.clone();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |request_control, authenticated| async move {
                    Box::pin(client_api.join_room_with_control(
                        &authenticated.user_id(),
                        &room_id,
                        req,
                        client_ip.as_deref(),
                        Some(&request_control),
                    ))
                    .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn start_room_password_login(
        &self,
        request: Request<StartRoomPasswordLoginRequest>,
    ) -> Result<Response<StartRoomPasswordLoginResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip.map(|ip| ip.to_string());
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |request_control, authenticated| async move {
                    client_api
                        .start_room_password_login_with_control(
                            &authenticated.user_id(),
                            req,
                            client_ip.as_deref(),
                            Some(&request_control),
                        )
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn finish_room_password_login(
        &self,
        request: Request<FinishRoomPasswordLoginRequest>,
    ) -> Result<Response<JoinRoomResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let client_ip = metadata.client_ip.map(|ip| ip.to_string());
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint_with_control(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |_request_control, authenticated| async move {
                    Box::pin(client_api.finish_room_password_login_with_control(
                        &authenticated.user_id(),
                        None,
                        req,
                        client_ip.as_deref(),
                    ))
                    .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn get_room_discovery(
        &self,
        request: Request<GetRoomDiscoveryRequest>,
    ) -> Result<Response<RoomDiscoveryItem>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .get_room_discovery(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn discover_rooms(
        &self,
        request: Request<DiscoverRoomsRequest>,
    ) -> Result<Response<DiscoverRoomsResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .discover_rooms(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_my_rooms(
        &self,
        request: Request<ListMyRoomsRequest>,
    ) -> Result<Response<ListMyRoomsResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .list_my_rooms(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn favorite_room(
        &self,
        request: Request<FavoriteRoomRequest>,
    ) -> Result<Response<FavoriteRoomResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .favorite_room(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn unfavorite_room(
        &self,
        request: Request<UnfavoriteRoomRequest>,
    ) -> Result<Response<UnfavoriteRoomResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Write,
                move |authenticated| async move {
                    client_api
                        .unfavorite_room(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_favorite_rooms(
        &self,
        request: Request<ListFavoriteRoomsRequest>,
    ) -> Result<Response<ListFavoriteRoomsResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_user_endpoint(
                &metadata,
                EndpointRateLimitCategory::Read,
                move |authenticated| async move {
                    client_api
                        .list_favorite_rooms(&authenticated.user_id(), req)
                        .await
                },
            )
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}
