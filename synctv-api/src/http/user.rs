//! User management HTTP handlers
// This layer now uses proto types and delegates to the impls layer for business logic

use axum::{
    extract::{Path, State},
    Json,
};

use super::{
    middleware::RequestMetadata,
    validation::{ProtoJson, ProtoQuery},
    AppResult, AppState,
};
use crate::impls::EndpointRateLimitCategory;
use crate::proto::client::GetProfileResponse;
use crate::proto::client::{
    CloseAccountRequest, CloseAccountResponse, DeletePasskeyRequest, DeletePasskeyResponse,
    FinishPasskeyBindRequest, ListMyRoomsResponse, ListPasskeysResponse, PasskeyCredentialResponse,
    StartPasskeyBindRequest, StartPasskeyBindResponse,
};
use crate::proto::client::{
    ConfirmEmailBindRequest, ConfirmEmailBindResponse, GetUserPreferencesResponse,
    UpdateUserPreferencesRequest, UpdateUserPreferencesResponse,
};
use crate::proto::client::{
    FinishOpaquePasswordUpdateRequest, FinishOpaquePasswordUpdateResponse,
    StartOpaquePasswordUpdateRequest, StartOpaquePasswordUpdateResponse,
};
pub use crate::proto::client::{
    SetUsernameRequest, SetUsernameResponse, StartEmailBindRequest, StartEmailBindResponse,
};

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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
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
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn update_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<UpdateUserPreferencesRequest>,
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
    ProtoJson(req): ProtoJson<SetUsernameRequest>,
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/user/email/bind/start",
        tag = "User",
        request_body = StartEmailBindRequest,
        responses(
            (status = 200, description = "Email bind confirmation sent", body = StartEmailBindResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_email_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<StartEmailBindRequest>,
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
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn confirm_email_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<ConfirmEmailBindRequest>,
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
        path = "/api/user/opaque-password/update/start",
        tag = "User",
        request_body = StartOpaquePasswordUpdateRequest,
        responses(
            (status = 200, description = "OPAQUE password update challenge created", body = StartOpaquePasswordUpdateResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_opaque_password_update(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<StartOpaquePasswordUpdateRequest>,
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
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_opaque_password_update(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<FinishOpaquePasswordUpdateRequest>,
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
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn start_passkey_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<StartPasskeyBindRequest>,
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
            (status = 400, description = "Invalid request", body = crate::openapi::ErrorResponseDoc),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn finish_passkey_bind(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(req): ProtoJson<FinishPasskeyBindRequest>,
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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
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
        path = "/api/user/passkeys/{credential_id}",
        tag = "User",
        params(
            ("credential_id" = String, Path, description = "Passkey credential id")
        ),
        responses(
            (status = 200, description = "Passkey deleted", body = DeletePasskeyResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc),
            (status = 404, description = "Passkey not found", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_passkey(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<DeletePasskeyRequest>,
) -> AppResult<Json<DeletePasskeyResponse>> {
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
    ProtoQuery(req): ProtoQuery<crate::proto::client::ListMyRoomsRequest>,
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
            (status = 401, description = "Authentication required", body = crate::openapi::ErrorResponseDoc)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn close_account(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoJson(_req): ProtoJson<CloseAccountRequest>,
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
