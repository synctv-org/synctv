//! User management HTTP handlers
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{middleware::RequestMetadata, validation::ValidatedQuery, AppResult, AppState};
use crate::http::passkey_json::{
    passkey_credential_to_json_bytes, passkey_options_to_value, validate_passkey_session_id,
};
use crate::impls::EndpointRateLimitCategory;
use crate::proto::client::GetProfileResponse;
use crate::proto::client::{
    DeletePasskeyResponse, ListMyRoomsResponse, ListPasskeysResponse, PasskeyCredential,
    PasskeyCredentialResponse,
};
use crate::proto::client::{
    FinishOpaquePasswordUpdateRequest, FinishOpaquePasswordUpdateResponse,
    OpaquePasswordUpdateVerificationMethod, StartOpaquePasswordUpdateRequest,
    StartOpaquePasswordUpdateResponse,
};
pub use crate::proto::client::{UpdateUserRequest, UpdateUserResponse};

/// Get current user info
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user",
        tag = "User",
        responses(
            (status = 200, description = "Current user profile", body = GetProfileResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_me(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<GetProfileResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |auth| async move { client_api.get_profile(&auth.user_id).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Update user (unified endpoint for username and password via PATCH)
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/user",
        tag = "User",
        request_body = UpdateUserRequest,
        responses(
            (status = 200, description = "User profile updated", body = UpdateUserResponse),
            (status = 400, description = "Invalid update request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserRequest>,
) -> AppResult<Json<UpdateUserResponse>> {
    let UpdateUserRequest {
        username,
        password,
        old_password,
    } = req;

    let mut updated_fields = Vec::new();
    if username.is_some() {
        updated_fields.push("username");
    }
    if password.is_some() {
        updated_fields.push("password");
    }
    if updated_fields.is_empty() {
        return Err(super::AppError::bad_request(
            "No valid update fields provided (username or password)",
        ));
    }

    let response_username = username.clone();
    let update_username = username.clone();
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .update_profile(
                        &auth.user_id,
                        update_username.clone(),
                        old_password,
                        password,
                    )
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    let username = if let Some(user) = response.user {
        Some(user.username)
    } else {
        response_username
    };
    Ok(Json(UpdateUserResponse {
        message: format!("{} updated successfully", updated_fields.join(" and ")),
        username,
    }))
}

pub async fn start_opaque_password_update(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartOpaquePasswordUpdateRequest>,
) -> AppResult<Json<StartOpaquePasswordUpdateResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let state_for_request = state.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                let method =
                    OpaquePasswordUpdateVerificationMethod::try_from(req.verification_method)
                        .map_err(|_| {
                            crate::impls::ApiError::InvalidInput(
                                "Invalid verification_method".to_string(),
                            )
                        })?;
                match method {
                    OpaquePasswordUpdateVerificationMethod::EmailToken => {
                        let email_api = state_for_request.email_api.as_ref().ok_or_else(|| {
                            crate::impls::ApiError::ServiceUnavailable(
                                synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE.to_string(),
                            )
                        })?;
                        if req.email_token.is_empty() {
                            return Err(crate::impls::ApiError::InvalidInput(
                                "email_token is required for email verification".to_string(),
                            ));
                        }
                        email_api
                            .email_token_service
                            .validate_token_for_user(
                                &req.email_token,
                                synctv_core::service::EmailTokenType::PasswordReset,
                                &auth.user_id,
                            )
                            .await
                            .map_err(crate::impls::ApiError::from)?;
                        let challenge = state_for_request
                            .user_service
                            .start_opaque_password_update_after_external_verification(
                                &auth.user_id,
                                req.registration_request,
                            )
                            .await
                            .map_err(crate::impls::ApiError::from)?;
                        Ok(StartOpaquePasswordUpdateResponse {
                            session_id: challenge.session_id,
                            credential_response: Vec::new(),
                            registration_response: challenge.registration_response,
                            passkey_session_id: String::new(),
                            passkey_options: Vec::new(),
                        })
                    }
                    OpaquePasswordUpdateVerificationMethod::Passkey => {
                        let passkey_service = require_passkey_service(&state_for_request)?;
                        let passkey_challenge = passkey_service
                            .start_user_verification(&auth.user_id)
                            .await
                            .map_err(crate::impls::ApiError::from)?;
                        let challenge = state_for_request
                            .user_service
                            .start_opaque_password_update_after_external_verification(
                                &auth.user_id,
                                req.registration_request,
                            )
                            .await
                            .map_err(crate::impls::ApiError::from)?;
                        Ok(StartOpaquePasswordUpdateResponse {
                            session_id: challenge.session_id,
                            credential_response: Vec::new(),
                            registration_response: challenge.registration_response,
                            passkey_session_id: passkey_challenge.session_id,
                            passkey_options: passkey_challenge.options_json,
                        })
                    }
                    _ => {
                        client_api
                            .start_opaque_password_update(&auth.user_id, req)
                            .await
                    }
                }
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn finish_opaque_password_update(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<FinishOpaquePasswordUpdateRequest>,
) -> AppResult<Json<FinishOpaquePasswordUpdateResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let state_for_request = state.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                if !req.passkey_session_id.is_empty() || !req.passkey_credential.is_empty() {
                    let passkey_service = require_passkey_service(&state_for_request)?;
                    passkey_service
                        .finish_user_verification(
                            &req.passkey_session_id,
                            &req.passkey_credential,
                            &auth.user_id,
                        )
                        .await
                        .map_err(crate::impls::ApiError::from)?;
                    let user = state_for_request
                        .user_service
                        .finish_opaque_password_update_after_external_verification(
                            &auth.user_id,
                            &req.session_id,
                            req.registration_upload,
                        )
                        .await
                        .map_err(crate::impls::ApiError::from)?;
                    Ok(FinishOpaquePasswordUpdateResponse {
                        user: Some(crate::impls::client::user_to_proto(
                            &user,
                            &state_for_request.public_id_codec,
                        )),
                    })
                } else {
                    client_api
                        .finish_opaque_password_update(&auth.user_id, req)
                        .await
                }
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

fn require_passkey_service(
    state: &AppState,
) -> Result<std::sync::Arc<synctv_core::service::PasskeyService>, crate::impls::ApiError> {
    state.passkey_service.clone().ok_or_else(|| {
        crate::impls::ApiError::ServiceUnavailable(
            "Passkey/WebAuthn service is not configured".to_string(),
        )
    })
}

fn passkey_credential_to_proto(
    credential: &synctv_core::repository::WebAuthnCredential,
) -> PasskeyCredential {
    PasskeyCredential {
        credential_id: synctv_core::service::PasskeyService::encode_credential_id(
            &credential.credential_id,
        ),
        name: credential.name.clone().unwrap_or_default(),
        sign_count: credential.sign_count,
        created_at: credential.created_at.timestamp(),
        updated_at: credential.updated_at.timestamp(),
        last_used_at: credential.last_used_at.map_or(0, |value| value.timestamp()),
    }
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyRegistrationHttpRequest {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyRegistrationHttpResponse {
    session_id: String,
    options: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FinishPasskeyRegistrationHttpRequest {
    session_id: String,
    credential: Value,
}

pub async fn start_passkey_registration(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartPasskeyRegistrationHttpRequest>,
) -> AppResult<Json<StartPasskeyRegistrationHttpResponse>> {
    if req.name.len() > 100 {
        return Err(super::error::map_api_error(
            crate::impls::ApiError::InvalidInput("name must be at most 100 characters".to_string()),
        ));
    }
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let state_for_request = state.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                let passkey_service = require_passkey_service(&state_for_request)?;
                let profile = state_for_request
                    .user_service
                    .get_user(&auth.user_id)
                    .await
                    .map_err(crate::impls::ApiError::from)?;
                let credential_name = if req.name.trim().is_empty() {
                    None
                } else {
                    Some(req.name.trim().to_string())
                };
                let challenge = passkey_service
                    .start_registration(&profile, credential_name)
                    .await
                    .map_err(crate::impls::ApiError::from)?;
                let options = passkey_options_to_value(&challenge.options_json)?;
                Ok::<_, crate::impls::ApiError>(StartPasskeyRegistrationHttpResponse {
                    session_id: challenge.session_id,
                    options,
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn finish_passkey_registration(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<FinishPasskeyRegistrationHttpRequest>,
) -> AppResult<Json<PasskeyCredentialResponse>> {
    validate_passkey_session_id(&req.session_id).map_err(super::error::map_api_error)?;
    let credential_json =
        passkey_credential_to_json_bytes(&req.credential).map_err(super::error::map_api_error)?;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let state_for_request = state.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                let passkey_service = require_passkey_service(&state_for_request)?;
                let credential = passkey_service
                    .finish_registration(&req.session_id, &credential_json, &auth.user_id)
                    .await
                    .map_err(crate::impls::ApiError::from)?;
                Ok::<_, crate::impls::ApiError>(PasskeyCredentialResponse {
                    credential: Some(passkey_credential_to_proto(&credential)),
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn list_passkeys(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<ListPasskeysResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let state_for_request = state.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |auth| async move {
                let passkey_service = require_passkey_service(&state_for_request)?;
                let credentials = passkey_service
                    .list_credentials(&auth.user_id)
                    .await
                    .map_err(crate::impls::ApiError::from)?
                    .iter()
                    .map(passkey_credential_to_proto)
                    .collect();
                Ok::<_, crate::impls::ApiError>(ListPasskeysResponse { credentials })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn delete_passkey(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(credential_id): Path<String>,
) -> AppResult<Json<DeletePasskeyResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let state_for_request = state.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                let passkey_service = require_passkey_service(&state_for_request)?;
                let credential_id =
                    synctv_core::service::PasskeyService::decode_credential_id(&credential_id)
                        .map_err(crate::impls::ApiError::from)?;
                let deleted = passkey_service
                    .delete_credential(&auth.user_id, &credential_id)
                    .await
                    .map_err(crate::impls::ApiError::from)?;
                Ok::<_, crate::impls::ApiError>(DeletePasskeyResponse { deleted })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// List rooms related to the current user.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user/rooms",
        tag = "User",
        params(
            crate::proto::client::ListMyRoomsRequest
        ),
        responses(
            (status = 200, description = "Rooms related to the current user", body = ListMyRoomsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_my_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ValidatedQuery(req): ValidatedQuery<crate::proto::client::ListMyRoomsRequest>,
) -> AppResult<Json<ListMyRoomsResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |auth| async move { client_api.list_my_rooms(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Delete the current user's own account (soft-delete)
///
/// Sets `deleted_at = NOW()` on the user row and cleans up `OAuth2` mappings.
/// The current token will return 401 on the next request because the security
/// pipeline checks for deleted users.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/user/me",
        tag = "User",
        responses(
            (status = 204, description = "Current user deleted"),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_me(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<axum::http::StatusCode> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
    executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api.delete_current_user(&auth.user_id).await?;
                Ok::<(), crate::impls::ApiError>(())
            },
        )
        .await
        .map_err(super::AppError::from)?;

    Ok(axum::http::StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_list_my_rooms_request_deserializes_numeric_fields() {
        let query: crate::proto::client::ListMyRoomsRequest = serde_urlencoded::from_str(
            "page=2&page_size=25&search=room&status=1&is_banned=false&relation=2&sort_by=5&sort_direction=1",
        )
        .expect("query should deserialize");

        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.search, "room");
        assert_eq!(query.status, 1);
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(query.relation, 2);
        assert_eq!(query.sort_by, 5);
        assert_eq!(query.sort_direction, 1);
    }

    #[test]
    fn test_list_my_rooms_request_query_defaults_to_proto_zero_values() {
        let query: crate::proto::client::ListMyRoomsRequest =
            serde_urlencoded::from_str("").expect("query should deserialize");

        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert!(query.search.is_empty());
        assert_eq!(query.status, 0);
        assert_eq!(query.is_banned, None);
        assert_eq!(query.relation, 0);
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
    }
}
