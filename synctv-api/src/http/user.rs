//! User management HTTP handlers

use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};

use super::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::client::GetProfileResponse;
use synctv_proto::client::{
    CloseAccountRequest, CloseAccountResponse, DeletePasskeyRequest, DeletePasskeyResponse,
    FinishPasskeyBindRequest, FinishSensitiveOperationVerificationRequest,
    FinishSensitiveOperationVerificationResponse, ListMyRoomsResponse, ListPasskeysResponse,
    PasskeyCredentialResponse, RequestSensitiveOperationEmailCodeRequest,
    RequestSensitiveOperationEmailCodeResponse, StartPasskeyBindRequest, StartPasskeyBindResponse,
    StartSensitiveOperationPasskeyRequest, StartSensitiveOperationPasskeyResponse,
    StartSensitiveOperationVerificationRequest, StartSensitiveOperationVerificationResponse,
};
use synctv_proto::client::{
    CompleteUserAvatarUploadSessionRequest, CompleteUserAvatarUploadSessionResponse,
    CreateUserAvatarUploadSessionRequest, CreateUserAvatarUploadSessionResponse,
    GetProfileResponse as UserAvatarUpdateResponse, UpdateUserAvatarRequest,
};
use synctv_proto::client::{
    ConfirmEmailBindRequest, ConfirmEmailBindResponse, GetUserPreferencesResponse,
    UnbindEmailRequest, UnbindEmailResponse, UpdateUserPreferencesRequest,
    UpdateUserPreferencesResponse,
};
use synctv_proto::client::{
    FinishOpaquePasswordUpdateRequest, FinishOpaquePasswordUpdateResponse,
    StartOpaquePasswordUpdateRequest, StartOpaquePasswordUpdateResponse,
};
use synctv_proto::client::{
    SetUsernameRequest, SetUsernameResponse, StartEmailBindRequest, StartEmailBindResponse,
};

fn file_upload_range_to_proto(
    range: synctv_core::models::FileUploadRange,
) -> synctv_proto::client::FileUploadRange {
    synctv_proto::client::FileUploadRange {
        start: range.start,
        end_inclusive: range.end_inclusive,
        total_size: range.total_size,
    }
}

fn upload_response_headers(
    complete: bool,
    uploaded_size_bytes: i64,
    uploaded_parts: &[i32],
) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        HeaderName::from_static("x-synctv-upload-complete"),
        HeaderValue::from_static(if complete { "true" } else { "false" }),
    );
    if let Ok(value) = HeaderValue::from_str(&uploaded_size_bytes.to_string()) {
        headers.insert(
            HeaderName::from_static("x-synctv-uploaded-size-bytes"),
            value,
        );
    }
    let uploaded_parts = uploaded_parts
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    if let Ok(value) = HeaderValue::from_str(&uploaded_parts) {
        headers.insert(HeaderName::from_static("x-synctv-uploaded-parts"), value);
    }
    headers
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserAvatarObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct UserAvatarObjectQuery {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyCredentialPath {
    pub credential_id: String,
}

/// Get current user info
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user",
        tag = "User",
        responses(
            (status = 200, description = "Current user profile", body = GetProfileResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
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
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user/preferences",
        tag = "User",
        responses(
            (status = 200, description = "Current user preferences", body = GetUserPreferencesResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<GetUserPreferencesResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |auth| async move { client_api.get_user_preferences(&auth.user_id).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/user/preferences",
        tag = "User",
        request_body = UpdateUserPreferencesRequest,
        responses(
            (status = 200, description = "User preferences updated", body = UpdateUserPreferencesResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserPreferencesRequest>,
) -> AppResult<Json<UpdateUserPreferencesResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.update_user_preferences(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

/// Set the current user's username.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/user",
        tag = "User",
        request_body = SetUsernameRequest,
        responses(
            (status = 200, description = "Username updated", body = SetUsernameResponse),
            (status = 400, description = "Invalid update request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<SetUsernameRequest>,
) -> AppResult<Json<SetUsernameResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.set_username(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn create_user_avatar_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<CreateUserAvatarUploadSessionRequest>,
) -> AppResult<Json<CreateUserAvatarUploadSessionResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .create_user_avatar_upload_session(&auth.user_id, req)
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn update_user_avatar(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserAvatarRequest>,
) -> AppResult<Json<UserAvatarUpdateResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.update_user_avatar(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn clear_user_avatar(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<UserAvatarUpdateResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.clear_user_avatar(&auth.user_id).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn upload_user_avatar_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<UserAvatarObjectPath>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let upload_token = super::required_header_str(
        &headers,
        synctv_core::service::FILE_UPLOAD_TOKEN_HEADER,
        "Missing file upload token",
    )?;
    let content_type = super::optional_header_str(&headers, &header::CONTENT_TYPE)?;
    let range = super::optional_content_range(&headers)?;
    let req = synctv_proto::client::UploadUserAvatarObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: upload_token.to_string(),
        content_type: content_type.map(str::to_string),
        content_range: range.map(file_upload_range_to_proto),
        data: body.to_vec(),
    };
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_public_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move || async move { client_api.upload_user_avatar_object(req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;
    Ok((
        upload_response_headers(
            response.complete,
            response.uploaded_size_bytes,
            &response.uploaded_parts,
        ),
        StatusCode::NO_CONTENT,
    )
        .into_response())
}

pub async fn complete_user_avatar_upload_session(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<UserAvatarObjectPath>,
    Json(mut req): Json<CompleteUserAvatarUploadSessionRequest>,
) -> AppResult<Json<CompleteUserAvatarUploadSessionResponse>> {
    req.encoded_object_key = path.encoded_object_key;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_public_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            move || async move { client_api.complete_user_avatar_upload_session(req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;
    Ok(Json(response))
}

pub async fn get_user_avatar_object(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<UserAvatarObjectPath>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<UserAvatarObjectQuery>,
) -> AppResult<Response> {
    let range = super::optional_file_range(&headers)?;
    let req = synctv_proto::client::GetUserAvatarObjectRequest {
        encoded_object_key: path.encoded_object_key,
        token: query.token,
        range: range.map(super::file_range_request_to_proto),
    };
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let download = executor
        .execute_public_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            move || async move { client_api.get_user_avatar_object(req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;
    super::file_object_download_response(download, None)
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/email/bind/start",
        tag = "User",
        request_body = StartEmailBindRequest,
        responses(
            (status = 200, description = "Email bind confirmation sent", body = StartEmailBindResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_email_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartEmailBindRequest>,
) -> AppResult<Json<StartEmailBindResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.start_email_bind(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/email/bind/confirm",
        tag = "User",
        request_body = ConfirmEmailBindRequest,
        responses(
            (status = 200, description = "Email bind confirmed", body = ConfirmEmailBindResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn confirm_email_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<ConfirmEmailBindRequest>,
) -> AppResult<Json<ConfirmEmailBindResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.confirm_email_bind(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/email/unbind",
        tag = "User",
        request_body = UnbindEmailRequest,
        responses(
            (status = 200, description = "Email unbound", body = UnbindEmailResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn unbind_email(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UnbindEmailRequest>,
) -> AppResult<Json<UnbindEmailResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.unbind_email(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/sensitive-verification/start",
        tag = "User",
        request_body = StartSensitiveOperationVerificationRequest,
        responses(
            (status = 200, description = "Sensitive operation verification started", body = StartSensitiveOperationVerificationResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_sensitive_operation_verification(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartSensitiveOperationVerificationRequest>,
) -> AppResult<Json<StartSensitiveOperationVerificationResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .start_sensitive_operation_verification(
                        &auth.user_id,
                        crate::impls::client::token_auth_context_from_claims(&auth.claims),
                        req,
                    )
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/sensitive-verification/passkey/start",
        tag = "User",
        request_body = StartSensitiveOperationPasskeyRequest,
        responses(
            (status = 200, description = "Sensitive operation passkey challenge created", body = StartSensitiveOperationPasskeyResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_sensitive_operation_passkey(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartSensitiveOperationPasskeyRequest>,
) -> AppResult<Json<StartSensitiveOperationPasskeyResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .start_sensitive_operation_passkey(&auth.user_id, req)
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/sensitive-verification/email/request",
        tag = "User",
        request_body = RequestSensitiveOperationEmailCodeRequest,
        responses(
            (status = 200, description = "Sensitive operation email code sent", body = RequestSensitiveOperationEmailCodeResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn request_sensitive_operation_email_code(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<RequestSensitiveOperationEmailCodeRequest>,
) -> AppResult<Json<RequestSensitiveOperationEmailCodeResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .request_sensitive_operation_email_code(&auth.user_id, req)
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/sensitive-verification/finish",
        tag = "User",
        request_body = FinishSensitiveOperationVerificationRequest,
        responses(
            (status = 200, description = "Sensitive operation verification progressed", body = FinishSensitiveOperationVerificationResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_sensitive_operation_verification(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<FinishSensitiveOperationVerificationRequest>,
) -> AppResult<Json<FinishSensitiveOperationVerificationResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let client_ip = request_meta.client_ip;
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint_with_control(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |request_control, auth| async move {
                client_api
                    .finish_sensitive_operation_verification(
                        &auth.user_id,
                        req,
                        client_ip,
                        Some(&request_control),
                    )
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/opaque-password/update/start",
        tag = "User",
        request_body = StartOpaquePasswordUpdateRequest,
        responses(
            (status = 200, description = "OPAQUE password update challenge created", body = StartOpaquePasswordUpdateResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_opaque_password_update(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartOpaquePasswordUpdateRequest>,
) -> AppResult<Json<StartOpaquePasswordUpdateResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .start_opaque_password_update(&auth.user_id, req)
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/opaque-password/update/finish",
        tag = "User",
        request_body = FinishOpaquePasswordUpdateRequest,
        responses(
            (status = 200, description = "OPAQUE password update completed", body = FinishOpaquePasswordUpdateResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_opaque_password_update(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<FinishOpaquePasswordUpdateRequest>,
) -> AppResult<Json<FinishOpaquePasswordUpdateResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .finish_opaque_password_update(&auth.user_id, req)
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/passkeys/bind/start",
        tag = "User",
        request_body = StartPasskeyBindRequest,
        responses(
            (status = 200, description = "Passkey bind challenge created", body = StartPasskeyBindResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_passkey_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartPasskeyBindRequest>,
) -> AppResult<Json<StartPasskeyBindResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.start_passkey_bind(&auth.user_id, req).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/passkeys/bind/finish",
        tag = "User",
        request_body = FinishPasskeyBindRequest,
        responses(
            (status = 200, description = "Passkey bound to current user", body = PasskeyCredentialResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_passkey_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<FinishPasskeyBindRequest>,
) -> AppResult<Json<PasskeyCredentialResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move {
                client_api
                    .finish_passkey_bind_request(&auth.user_id, req)
                    .await
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/user/passkeys",
        tag = "User",
        responses(
            (status = 200, description = "Passkeys for current user", body = ListPasskeysResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_passkeys(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<ListPasskeysResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Read,
            |auth| async move { client_api.list_passkeys(&auth.user_id).await },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/user/passkeys/{credentialId}",
        tag = "User",
        request_body = DeletePasskeyRequest,
        params(
            ("credentialId" = String, Path, description = "Passkey credential id")
        ),
        responses(
            (status = 200, description = "Passkey deleted", body = DeletePasskeyResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Passkey not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_passkey(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<PasskeyCredentialPath>,
    Json(mut req): Json<DeletePasskeyRequest>,
) -> AppResult<Json<DeletePasskeyResponse>> {
    req.credential_id = path.credential_id;
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.delete_passkey(&auth.user_id, req).await },
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
            synctv_proto::client::ListMyRoomsRequest
        ),
        responses(
            (status = 200, description = "Rooms related to the current user", body = ListMyRoomsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_my_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<synctv_proto::client::ListMyRoomsRequest>,
) -> AppResult<Json<ListMyRoomsResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
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

/// Close the authenticated account
///
/// Sets `deleted_at = NOW()` on the user row and cleans up `OAuth2` mappings.
/// The current token will return 401 on the next request because the security
/// pipeline checks for deleted users.
#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/account-closure",
        tag = "User",
        request_body = CloseAccountRequest,
        responses(
            (status = 200, description = "Account closure completed", body = CloseAccountResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn close_account(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(_req): Json<CloseAccountRequest>,
) -> AppResult<Json<CloseAccountResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.shared_api_runtime.client_api.clone();
    let client_api = state.shared_api_runtime.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &request_meta,
            EndpointRateLimitCategory::Write,
            |auth| async move { client_api.close_account(&auth.user_id).await },
        )
        .await
        .map_err(super::AppError::from)?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use super::UserAvatarObjectQuery;
    use synctv_proto::client::{CompleteUserAvatarUploadSessionRequest, DeletePasskeyRequest};

    type TestResult<T = ()> = anyhow::Result<T>;

    #[test]
    fn test_list_my_rooms_request_deserializes_numeric_fields() -> TestResult {
        let query: synctv_proto::client::ListMyRoomsRequest = serde_urlencoded::from_str(
            "page=2&pageSize=25&search=room&status=1&isBanned=false&relation=2&sortBy=5&sortDirection=1",
        )?;

        assert_eq!(query.page, 2);
        assert_eq!(query.page_size, 25);
        assert_eq!(query.search, "room");
        assert_eq!(query.status, 1);
        assert_eq!(query.is_banned, Some(false));
        assert_eq!(query.relation, 2);
        assert_eq!(query.sort_by, 5);
        assert_eq!(query.sort_direction, 1);
        Ok(())
    }

    #[test]
    fn test_list_my_rooms_request_query_defaults_to_proto_zero_values() -> TestResult {
        let query: synctv_proto::client::ListMyRoomsRequest = serde_urlencoded::from_str("")?;

        assert_eq!(query.page, 0);
        assert_eq!(query.page_size, 0);
        assert!(query.search.is_empty());
        assert_eq!(query.status, 0);
        assert_eq!(query.is_banned, None);
        assert_eq!(query.relation, 0);
        assert_eq!(query.sort_by, 0);
        assert_eq!(query.sort_direction, 0);
        Ok(())
    }

    #[test]
    fn test_user_avatar_object_query_ignores_unknown_fields() -> TestResult {
        let query = serde_urlencoded::from_str::<UserAvatarObjectQuery>("token=token&extra=true")?;
        assert_eq!(query.token, "token");
        Ok(())
    }

    #[test]
    fn test_delete_passkey_request_overrides_path_credential_id() -> TestResult {
        let mut req: DeletePasskeyRequest =
            serde_json::from_str(r#"{"credentialId":"body_cred","verificationId":"verify_1"}"#)?;
        req.credential_id = "cred_1".to_string();

        assert_eq!(req.credential_id, "cred_1");
        assert_eq!(req.verification_id, "verify_1");
        Ok(())
    }

    #[test]
    fn test_complete_user_avatar_upload_session_request_overrides_path_object_key() -> TestResult {
        let mut req: CompleteUserAvatarUploadSessionRequest = serde_json::from_str(
            r#"{"encodedObjectKey":"body-key","token":"upload-token","uploadId":"upload-1"}"#,
        )?;
        req.encoded_object_key = "avatar-key".to_string();

        assert_eq!(req.encoded_object_key, "avatar-key");
        assert_eq!(req.token, "upload-token");
        assert_eq!(req.upload_id.as_deref(), Some("upload-1"));
        Ok(())
    }
}
