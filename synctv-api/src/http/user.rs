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
    DeletePasskeyRequest, DeletePasskeyResponse, ListMyRoomsResponse, ListPasskeysResponse,
    PasskeyCredentialResponse,
};
use crate::proto::client::{
    FinishOpaquePasswordUpdateRequest, FinishOpaquePasswordUpdateResponse,
    StartOpaquePasswordUpdateRequest, StartOpaquePasswordUpdateResponse,
};
use crate::proto::client::{
    GetUserPreferencesResponse, UpdateUserPreferencesRequest, UpdateUserPreferencesResponse,
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

pub async fn get_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<GetUserPreferencesResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
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

pub async fn update_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<UpdateUserPreferencesRequest>,
) -> AppResult<Json<UpdateUserPreferencesResponse>> {
    let request_meta = request_meta
        .0
        .with_timeout(Some(synctv_core::resilience::timeout::HTTP_REQUEST_TIMEOUT));
    let executor = state.client_api.clone();
    let client_api = state.client_api.clone();
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

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyBindHttpRequest {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct StartPasskeyBindHttpResponse {
    session_id: String,
    options: Value,
}

#[derive(Debug, Clone, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct FinishPasskeyBindHttpRequest {
    session_id: String,
    credential: Value,
}

pub async fn start_passkey_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<StartPasskeyBindHttpRequest>,
) -> AppResult<Json<StartPasskeyBindHttpResponse>> {
    if req.name.len() > 100 {
        return Err(super::error::map_api_error(
            crate::impls::ApiError::InvalidInput("name must be at most 100 characters".to_string()),
        ));
    }
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
                let challenge = client_api
                    .start_passkey_bind_challenge(&auth.user_id, req.name)
                    .await?;
                let options = passkey_options_to_value(&challenge.options_json)?;
                Ok::<_, crate::impls::ApiError>(StartPasskeyBindHttpResponse {
                    session_id: challenge.session_id,
                    options,
                })
            },
        )
        .await
        .map_err(super::error::map_api_error)?;

    Ok(Json(response))
}

pub async fn finish_passkey_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<FinishPasskeyBindHttpRequest>,
) -> AppResult<Json<PasskeyCredentialResponse>> {
    validate_passkey_session_id(&req.session_id).map_err(super::error::map_api_error)?;
    let credential_json =
        passkey_credential_to_json_bytes(&req.credential).map_err(super::error::map_api_error)?;
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
                    .finish_passkey_bind(&auth.user_id, &req.session_id, &credential_json)
                    .await
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
    let client_api = state.client_api.clone();
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

pub async fn delete_passkey(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(credential_id): Path<String>,
) -> AppResult<Json<DeletePasskeyResponse>> {
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
                    .delete_passkey(&auth.user_id, DeletePasskeyRequest { credential_id })
                    .await
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
