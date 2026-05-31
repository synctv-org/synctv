//! User operations: `get_profile`, `set_username`

use crate::impls::ApiError;
use crate::proto::client::OpaquePasswordUpdateVerificationMethod;
use crate::realtime_lifecycle::DeletedRoomAfterCommitFanout;
use std::collections::HashMap;
use synctv_core::models::{FileBlob, FileUploadSession, NewStoredFile, PageParams, RoomId, UserId};
use synctv_core::validation::UsernameValidator;

use super::convert::{stored_file_reference_to_video_cover, user_to_proto};
use super::ClientApiImpl;

const USER_ROOM_DELETION_PAGE_SIZE: u32 = 100;

fn new_file_to_avatar_proto(file: &NewStoredFile) -> crate::proto::client::UserAvatar {
    crate::proto::client::UserAvatar {
        id: file.id.clone(),
        storage_backend: file.storage_backend.clone(),
        object_key: file.object_key.clone(),
        url: file.url.clone().unwrap_or_default(),
        mime_type: file.mime_type.clone().unwrap_or_default(),
        size_bytes: file.size_bytes.unwrap_or_default(),
        width: file.width.unwrap_or_default(),
        height: file.height.unwrap_or_default(),
        metadata: serde_json::to_vec(&file.metadata).unwrap_or_default(),
    }
}

fn avatar_proto_to_new_file(
    avatar: crate::proto::client::UserAvatar,
) -> Result<NewStoredFile, ApiError> {
    let metadata = if avatar.metadata.is_empty() {
        serde_json::Value::Object(Default::default())
    } else {
        serde_json::from_slice(&avatar.metadata)
            .map_err(|error| ApiError::InvalidInput(format!("Invalid avatar metadata: {error}")))?
    };
    if !metadata.is_object() {
        return Err(ApiError::InvalidInput(
            "Avatar metadata must be a JSON object".to_string(),
        ));
    }
    Ok(NewStoredFile {
        id: avatar.id,
        storage_backend: avatar.storage_backend,
        object_key: avatar.object_key,
        url: (!avatar.url.trim().is_empty()).then_some(avatar.url),
        mime_type: (!avatar.mime_type.trim().is_empty()).then_some(avatar.mime_type),
        size_bytes: (avatar.size_bytes > 0).then_some(avatar.size_bytes),
        width: (avatar.width > 0).then_some(avatar.width),
        height: (avatar.height > 0).then_some(avatar.height),
        metadata,
    })
}

fn avatar_upload_session_to_proto(
    session: FileUploadSession,
) -> crate::proto::client::UserAvatarUploadSession {
    crate::proto::client::UserAvatarUploadSession {
        avatar: Some(new_file_to_avatar_proto(&session.file)),
        upload_required: session.upload_required,
        upload_url: session.upload_url.unwrap_or_default(),
        upload_method: session.upload_method.unwrap_or_default(),
        upload_headers: session.upload_headers.into_iter().collect(),
        expires_at: session.expires_at.map_or(0, |ts| ts.timestamp()),
        max_size_bytes: session.max_size_bytes,
        ownership_proof_required: session.ownership_proof_required,
        ownership_proof_nonce: session.ownership_proof_nonce.unwrap_or_default(),
        ownership_proof_ranges: session
            .ownership_proof_ranges
            .into_iter()
            .map(
                |range| crate::proto::client::UserAvatarOwnershipProofRange {
                    offset: range.offset,
                    length: range.length,
                },
            )
            .collect(),
        ownership_proof_metadata_key: session.ownership_proof_metadata_key.unwrap_or_default(),
    }
}

fn avatar_object_to_proto(blob: &FileBlob) -> crate::proto::client::UserAvatarObjectResponse {
    crate::proto::client::UserAvatarObjectResponse {
        object_key: blob.object_key.clone(),
        mime_type: blob.mime_type.clone(),
        checksum_sha256: blob.checksum_sha256.clone(),
        data: blob.data.clone(),
    }
}

fn auth_factors_to_proto(
    factors: &synctv_core::models::UserAuthFactors,
) -> crate::proto::client::UserAuthFactors {
    crate::proto::client::UserAuthFactors {
        password: factors.password,
        webauthn: factors.webauthn,
        email: factors.email,
        eligible_count: i32::try_from(factors.eligible_count()).unwrap_or(i32::MAX),
    }
}

fn user_preferences_to_proto(
    preferences: &synctv_core::models::UserPreferences,
) -> Result<crate::proto::client::UserPreferences, ApiError> {
    Ok(crate::proto::client::UserPreferences {
        two_factor_enabled: preferences.two_factor_enabled,
        notifications: Some(user_notification_preferences_to_proto(
            &preferences.notifications,
        )),
        settings: serde_json::to_vec(&preferences.settings).map_err(|error| {
            ApiError::Internal(format!("Failed to serialize settings: {error}"))
        })?,
    })
}

pub(crate) fn user_notification_preferences_to_proto(
    preferences: &synctv_core::models::UserNotificationPreferences,
) -> crate::proto::client::UserNotificationPreferences {
    crate::proto::client::UserNotificationPreferences {
        room_invitation_in_app: preferences.room_invitation_in_app,
        room_event_in_app: preferences.room_event_in_app,
        system_announcement_in_app: preferences.system_announcement_in_app,
        room_invitation_email: preferences.room_invitation_email,
        room_event_email: preferences.room_event_email,
        system_announcement_email: preferences.system_announcement_email,
    }
}

pub(crate) fn user_preferences_update_from_proto(
    req: crate::proto::client::UpdateUserPreferencesRequest,
) -> synctv_core::models::UserPreferencesUpdate {
    synctv_core::models::UserPreferencesUpdate {
        two_factor_enabled: req.two_factor_enabled,
        notifications: req.notifications.map(|value| {
            synctv_core::models::UserNotificationPreferences {
                room_invitation_in_app: value.room_invitation_in_app,
                room_event_in_app: value.room_event_in_app,
                system_announcement_in_app: value.system_announcement_in_app,
                room_invitation_email: value.room_invitation_email,
                room_event_email: value.room_event_email,
                system_announcement_email: value.system_announcement_email,
            }
        }),
    }
}

async fn list_owned_room_ids(
    api: &ClientApiImpl,
    user_id: &UserId,
) -> Result<Vec<RoomId>, ApiError> {
    let mut page = 1;
    let mut room_ids = Vec::new();

    loop {
        let (rooms, total) = api
            .room_service
            .list_rooms_by_creator(
                user_id,
                PageParams::new(Some(page), Some(USER_ROOM_DELETION_PAGE_SIZE)),
            )
            .await
            .map_err(ApiError::from)?;

        if rooms.is_empty() {
            break;
        }

        room_ids.extend(rooms.into_iter().map(|room| room.id));
        if i64::try_from(room_ids.len()).unwrap_or(i64::MAX) >= total {
            break;
        }

        page += 1;
    }

    Ok(room_ids)
}

fn prepare_deleted_room_outbox_fanout(
    api: &ClientApiImpl,
    room_ids: &[RoomId],
    deleted_by: &UserId,
) -> (
    HashMap<RoomId, synctv_core::repository::realtime_outbox::NewRealtimeOutboxEvent>,
    Vec<DeletedRoomAfterCommitFanout>,
) {
    let mut outbox_events = HashMap::with_capacity(room_ids.len());
    let mut fanout = Vec::with_capacity(room_ids.len());
    for room_id in room_ids {
        let prepared = api
            .room_lifecycle_fanout
            .prepare_room_deleted_outbox_fanout(room_id, deleted_by);
        if let Some(outbox_event) = prepared.cloned_outbox_event() {
            outbox_events.insert(*room_id, outbox_event);
        }
        fanout.push(DeletedRoomAfterCommitFanout {
            room_id: *room_id,
            event: prepared.into_event(),
        });
    }
    (outbox_events, fanout)
}

impl ClientApiImpl {
    async fn user_to_proto_with_avatar(
        &self,
        user: &synctv_core::models::User,
    ) -> Result<crate::proto::client::User, ApiError> {
        let mut proto = user_to_proto(user, &self.public_id_codec);
        if let Some(file) = self
            .load_stored_file_reference(user.avatar_file_reference_id)
            .await?
        {
            let url = self
                .stored_file_reference_url(
                    &file,
                    &synctv_core::service::user_avatar_upload_policy(),
                )?
                .unwrap_or_default();
            let file = stored_file_reference_to_video_cover(&file, Some(url));
            proto.avatar = Some(crate::proto::client::UserAvatar {
                id: file.id,
                storage_backend: file.storage_backend,
                object_key: file.object_key,
                url: file.url,
                mime_type: file.mime_type,
                size_bytes: file.size_bytes,
                width: file.width,
                height: file.height,
                metadata: file.metadata,
            });
        }
        Ok(proto)
    }

    pub async fn close_account(
        &self,
        user_id: &UserId,
    ) -> Result<crate::proto::client::CloseAccountResponse, ApiError> {
        let uid = *user_id;
        let owned_room_ids = list_owned_room_ids(self, &uid).await?;
        let (deleted_room_outbox_events, deleted_room_fanout) =
            prepare_deleted_room_outbox_fanout(self, &owned_room_ids, &uid);
        let summary = self
            .user_service
            .delete_user_with_summary_and_outbox(&uid, deleted_room_outbox_events)
            .await
            .map_err(ApiError::from)?;

        self.realtime_lifecycle
            .finalize_user_deletion(
                self.room_service.as_ref(),
                &summary,
                &uid,
                "user_deleted",
                deleted_room_fanout,
            )
            .await;

        Ok(crate::proto::client::CloseAccountResponse { success: true })
    }

    pub async fn update_profile(
        &self,
        user_id: &UserId,
        username: Option<String>,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let normalized_username = username.as_ref().map(|value| value.trim().to_string());
        if normalized_username.is_none() {
            return Err(ApiError::InvalidInput(
                "No valid update fields provided (username)".to_string(),
            ));
        }

        if let Some(ref username) = normalized_username {
            let request = crate::proto::client::SetUsernameRequest {
                new_username: username.clone(),
            };
            crate::impls::validate_proto_request(&request)?;
            UsernameValidator::new()
                .validate(username)
                .map_err(|e| ApiError::InvalidInput(e.to_string()))?;
        }

        let uid = *user_id;
        let updated_user = self
            .user_service
            .update_profile(&uid, normalized_username)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetProfileResponse {
            user: Some(self.user_to_proto_with_avatar(&updated_user).await?),
        })
    }

    pub async fn get_profile(
        &self,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let uid = *user_id;
        let user = self
            .user_service
            .get_user(&uid)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetProfileResponse {
            user: Some(self.user_to_proto_with_avatar(&user).await?),
        })
    }

    pub async fn get_user_preferences(
        &self,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetUserPreferencesResponse, ApiError> {
        let (preferences, auth_factors) = self
            .user_service
            .get_user_preferences(user_id)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::GetUserPreferencesResponse {
            preferences: Some(user_preferences_to_proto(&preferences)?),
            auth_factors: Some(auth_factors_to_proto(&auth_factors)),
        })
    }

    pub async fn update_user_preferences(
        &self,
        user_id: &UserId,
        req: crate::proto::client::UpdateUserPreferencesRequest,
    ) -> Result<crate::proto::client::UpdateUserPreferencesResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let update = user_preferences_update_from_proto(req);
        if update.is_empty() {
            return Err(ApiError::InvalidInput(
                "No valid user preference fields provided".to_string(),
            ));
        }
        let (preferences, auth_factors) = self
            .user_service
            .update_user_preferences(user_id, update)
            .await
            .map_err(ApiError::from)?;

        Ok(crate::proto::client::UpdateUserPreferencesResponse {
            preferences: Some(user_preferences_to_proto(&preferences)?),
            auth_factors: Some(auth_factors_to_proto(&auth_factors)),
        })
    }

    pub async fn set_username(
        &self,
        user_id: &UserId,
        req: crate::proto::client::SetUsernameRequest,
    ) -> Result<crate::proto::client::SetUsernameResponse, ApiError> {
        let response = self
            .update_profile(user_id, Some(req.new_username.trim().to_string()))
            .await?;

        Ok(crate::proto::client::SetUsernameResponse {
            user: response.user,
        })
    }

    pub async fn create_user_avatar_upload_session(
        &self,
        user_id: &UserId,
        req: crate::proto::client::CreateUserAvatarUploadSessionRequest,
    ) -> Result<crate::proto::client::CreateUserAvatarUploadSessionResponse, ApiError> {
        let metadata = if req.metadata.is_empty() {
            serde_json::Value::Object(Default::default())
        } else {
            serde_json::from_slice(&req.metadata).map_err(|error| {
                ApiError::InvalidInput(format!("Invalid avatar metadata: {error}"))
            })?
        };
        let session = self
            .user_service
            .create_avatar_upload_session(
                user_id,
                synctv_core::service::user::CreateUserAvatarUploadSession {
                    client_avatar_id: (!req.client_avatar_id.trim().is_empty())
                        .then_some(req.client_avatar_id),
                    mime_type: req.mime_type,
                    size_bytes: req.size_bytes,
                    width: (req.width > 0).then_some(req.width),
                    height: (req.height > 0).then_some(req.height),
                    checksum_sha256: (!req.checksum_sha256.trim().is_empty())
                        .then_some(req.checksum_sha256),
                    metadata,
                },
            )
            .await
            .map_err(ApiError::from)?;
        Ok(
            crate::proto::client::CreateUserAvatarUploadSessionResponse {
                session: Some(avatar_upload_session_to_proto(session)),
            },
        )
    }

    pub async fn upload_user_avatar_object(
        &self,
        req: crate::proto::client::UploadUserAvatarObjectRequest,
    ) -> Result<crate::proto::client::UploadUserAvatarObjectResponse, ApiError> {
        let blob = self
            .user_service
            .store_avatar_upload_object(
                &req.encoded_object_key,
                &req.token,
                (!req.content_type.trim().is_empty()).then_some(req.content_type.as_str()),
                req.data,
            )
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::UploadUserAvatarObjectResponse {
            object: Some(avatar_object_to_proto(&blob)),
        })
    }

    pub async fn get_user_avatar_object(
        &self,
        req: crate::proto::client::GetUserAvatarObjectRequest,
    ) -> Result<crate::proto::client::UserAvatarObjectResponse, ApiError> {
        let blob = self
            .user_service
            .get_avatar_object(&req.encoded_object_key, &req.token)
            .await
            .map_err(ApiError::from)?;
        Ok(avatar_object_to_proto(&blob))
    }

    pub async fn update_user_avatar(
        &self,
        user_id: &UserId,
        req: crate::proto::client::UpdateUserAvatarRequest,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let avatar = req
            .avatar
            .ok_or_else(|| ApiError::InvalidInput("avatar is required".to_string()))?;
        let updated = self
            .user_service
            .update_avatar(user_id, avatar_proto_to_new_file(avatar)?)
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::GetProfileResponse {
            user: Some(self.user_to_proto_with_avatar(&updated).await?),
        })
    }

    pub async fn clear_user_avatar(
        &self,
        user_id: &UserId,
    ) -> Result<crate::proto::client::GetProfileResponse, ApiError> {
        let updated = self
            .user_service
            .clear_avatar(user_id)
            .await
            .map_err(ApiError::from)?;
        Ok(crate::proto::client::GetProfileResponse {
            user: Some(self.user_to_proto_with_avatar(&updated).await?),
        })
    }

    pub async fn start_email_bind(
        &self,
        user_id: &UserId,
        req: crate::proto::client::StartEmailBindRequest,
    ) -> Result<crate::proto::client::StartEmailBindResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let email_api = self.email_api.as_ref().ok_or_else(|| {
            ApiError::ServiceUnavailable(
                synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE.to_string(),
            )
        })?;
        let email = crate::impls::validation::validate_email(&req.email)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        email_api
            .check_email_delivery_rate_limits(
                &email,
                user_id,
                synctv_core::models::EmailTokenType::EmailBind,
                None,
            )
            .await?;
        let token = self
            .user_service
            .start_email_bind(user_id, &email)
            .await
            .map_err(ApiError::from)?;

        if let Err(error) = email_api
            .email_service
            .send_email_bind_token_email_with_control(&email, &token, None)
            .await
        {
            self.user_service
                .delete_pending_email_bind(user_id, &email, &token)
                .await
                .map_err(ApiError::from)?;
            return Err(ApiError::from(error));
        }

        Ok(crate::proto::client::StartEmailBindResponse {
            masked_email: synctv_core::service::mask_email(&email),
        })
    }

    pub async fn confirm_email_bind(
        &self,
        user_id: &UserId,
        req: crate::proto::client::ConfirmEmailBindRequest,
    ) -> Result<crate::proto::client::ConfirmEmailBindResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let email = crate::impls::validation::validate_email(&req.email)
            .map_err(|error| ApiError::InvalidInput(error.to_string()))?;
        let updated_user = self
            .user_service
            .confirm_email_bind(user_id, &email, &req.token)
            .await
            .map_err(|error| match error {
                synctv_core::Error::InvalidInput(_) => {
                    ApiError::InvalidInput("Invalid or expired email bind token".to_string())
                }
                other => ApiError::from(other),
            })?;

        Ok(crate::proto::client::ConfirmEmailBindResponse {
            user: Some(user_to_proto(&updated_user, &self.public_id_codec)),
        })
    }

    pub async fn unbind_email(
        &self,
        user_id: &UserId,
        req: crate::proto::client::UnbindEmailRequest,
    ) -> Result<crate::proto::client::UnbindEmailResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let updated_user = self.user_service.unbind_email(user_id).await?;

        Ok(crate::proto::client::UnbindEmailResponse {
            user: Some(user_to_proto(&updated_user, &self.public_id_codec)),
        })
    }

    pub async fn start_opaque_password_update(
        &self,
        user_id: &UserId,
        req: crate::proto::client::StartOpaquePasswordUpdateRequest,
    ) -> Result<crate::proto::client::StartOpaquePasswordUpdateResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let method = OpaquePasswordUpdateVerificationMethod::try_from(req.verification_method)
            .map_err(|_| ApiError::InvalidInput("Invalid verification_method".to_string()))?;
        let challenge = match method {
            OpaquePasswordUpdateVerificationMethod::CurrentOpaquePassword => self
                .user_service
                .start_opaque_password_update(
                    user_id,
                    req.credential_request,
                    req.registration_request,
                )
                .await
                .map_err(ApiError::from)?,
            OpaquePasswordUpdateVerificationMethod::EmailToken => {
                let email_api = self.email_api.as_ref().ok_or_else(|| {
                    ApiError::ServiceUnavailable(
                        synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE.to_string(),
                    )
                })?;
                if req.email_token.is_empty() {
                    return Err(ApiError::InvalidInput(
                        "email_token is required for email authentication".to_string(),
                    ));
                }
                email_api
                    .email_token_service
                    .validate_token_for_user(
                        &req.email_token,
                        synctv_core::models::EmailTokenType::PasswordReset,
                        user_id,
                    )
                    .await
                    .map_err(ApiError::from)?;
                self.user_service
                    .start_opaque_password_update_after_external_verification(
                        user_id,
                        req.registration_request,
                    )
                    .await
                    .map_err(ApiError::from)?
            }
            OpaquePasswordUpdateVerificationMethod::Passkey => {
                let passkey_challenge = self
                    .passkey_service()?
                    .start_user_verification(user_id)
                    .await
                    .map_err(ApiError::from)?;
                let challenge = self
                    .user_service
                    .start_opaque_password_update_pending_passkey_verification(
                        user_id,
                        req.registration_request,
                    )
                    .await
                    .map_err(ApiError::from)?;
                return Ok(crate::proto::client::StartOpaquePasswordUpdateResponse {
                    session_id: challenge.session_id,
                    credential_response: Vec::new(),
                    registration_response: challenge.registration_response,
                    passkey_session_id: passkey_challenge.session_id,
                    passkey_options: passkey_challenge.options_json,
                });
            }
            OpaquePasswordUpdateVerificationMethod::Unspecified => {
                return Err(ApiError::InvalidInput(
                    "Unsupported verification_method for this endpoint".to_string(),
                ));
            }
        };

        Ok(crate::proto::client::StartOpaquePasswordUpdateResponse {
            session_id: challenge.session_id,
            credential_response: challenge.credential_response,
            registration_response: challenge.registration_response,
            passkey_session_id: String::new(),
            passkey_options: Vec::new(),
        })
    }

    pub async fn finish_opaque_password_update(
        &self,
        user_id: &UserId,
        req: crate::proto::client::FinishOpaquePasswordUpdateRequest,
    ) -> Result<crate::proto::client::FinishOpaquePasswordUpdateResponse, ApiError> {
        crate::impls::validate_proto_request(&req)?;
        let user = if !req.passkey_session_id.is_empty() || !req.passkey_credential.is_empty() {
            self.passkey_service()?
                .finish_user_verification(&req.passkey_session_id, &req.passkey_credential, user_id)
                .await
                .map_err(ApiError::from)?;
            self.user_service
                .finish_opaque_password_update_after_passkey_verification(
                    user_id,
                    &req.session_id,
                    req.registration_upload,
                )
                .await
                .map_err(ApiError::from)?
        } else if req.credential_finalization.is_empty() {
            self.user_service
                .finish_opaque_password_update_after_external_verification(
                    user_id,
                    &req.session_id,
                    req.registration_upload,
                )
                .await
                .map_err(ApiError::from)?
        } else {
            self.user_service
                .finish_opaque_password_update(
                    user_id,
                    &req.session_id,
                    req.credential_finalization,
                    req.registration_upload,
                )
                .await
                .map_err(ApiError::from)?
        };

        Ok(crate::proto::client::FinishOpaquePasswordUpdateResponse {
            user: Some(user_to_proto(&user, &self.public_id_codec)),
        })
    }
}
