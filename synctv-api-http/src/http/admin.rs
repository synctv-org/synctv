//! Admin HTTP handlers
//!
//! All admin routes require authentication and admin/root role.
//! Thin handlers that delegate to `AdminApiImpl`.

use axum::{
    extract::{Path, State},
    routing::{get, patch, post},
    Json, Router,
};
use std::sync::Arc;

use super::{
    admin_execute::{execute_admin_endpoint, execute_root_endpoint},
    middleware::RequestMetadata,
    validation::ProtoQuery,
    AppError, AppResult, AppState,
};
use synctv_proto::{admin, client};

fn require_admin_api(
    state: &AppState,
) -> Result<Arc<synctv_api_common::impls::AdminApiImpl>, AppError> {
    state.shared_api_runtime.admin_api.clone().ok_or_else(|| {
        AppError::new(
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            "Admin service is not available on this server.",
        )
    })
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RoomMemberTargetPath {
    room_id: String,
    user_id: String,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub(crate) struct AdminGetUserRoomsQuery {
    #[serde(default)]
    page: i32,
    #[serde(default)]
    page_size: i32,
    #[serde(default)]
    status: i32,
    #[serde(default)]
    search: String,
    is_banned: Option<bool>,
    #[serde(default)]
    sort_by: i32,
    #[serde(default)]
    sort_direction: i32,
}

impl AdminGetUserRoomsQuery {
    fn into_request(self, user_id: String) -> admin::GetUserRoomsRequest {
        admin::GetUserRoomsRequest {
            user_id,
            page: self.page,
            page_size: self.page_size,
            status: self.status,
            search: self.search,
            is_banned: self.is_banned,
            sort_by: self.sort_by,
            sort_direction: self.sort_direction,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub(crate) struct AdminGetRoomMembersQuery {
    #[serde(default)]
    page: i32,
    #[serde(default)]
    page_size: i32,
    #[serde(default)]
    search: String,
    #[serde(default)]
    role: i32,
    #[serde(default)]
    sort_by: i32,
    #[serde(default)]
    sort_direction: i32,
}

impl AdminGetRoomMembersQuery {
    fn into_request(self, room_id: String) -> admin::GetRoomMembersRequest {
        admin::GetRoomMembersRequest {
            room_id,
            page: self.page,
            page_size: self.page_size,
            search: self.search,
            role: self.role,
            sort_by: self.sort_by,
            sort_direction: self.sort_direction,
        }
    }
}

// Router

pub(crate) fn create_admin_router() -> Router<AppState> {
    Router::new()
        // Service state
        .route("/service-state", get(get_service_state))
        .route("/server-state", get(get_server_state))
        .route("/slice-cache", get(get_slice_cache_stats))
        .route("/slice-cache/purge", post(purge_slice_cache))
        .route(
            "/slice-cache/evict-expired",
            post(evict_expired_slice_cache),
        )
        // Review workflow
        .route(
            "/reviews/user-registrations",
            get(list_user_registration_reviews),
        )
        .route(
            "/reviews/user-registrations/approve",
            post(approve_user_registration_review),
        )
        .route(
            "/reviews/user-registrations/reject",
            post(reject_user_registration_review),
        )
        .route("/reviews/room-creations", get(list_room_creation_reviews))
        .route(
            "/reviews/room-creations/approve",
            post(approve_room_creation_review),
        )
        .route(
            "/reviews/room-creations/reject",
            post(reject_room_creation_review),
        )
        .route("/reviews/room-joins", get(list_room_join_reviews))
        .route(
            "/reviews/room-joins/approve",
            post(approve_room_join_review),
        )
        .route("/reviews/room-joins/reject", post(reject_room_join_review))
        // Moderation bans
        .route("/bans", get(list_ban_records))
        // Moderation reports
        .route("/reports", get(list_content_reports))
        .route("/reports/{reportId}", get(get_content_report))
        .route(
            "/reports/{reportId}/status",
            post(update_content_report_status),
        )
        // Settings
        .route("/settings", get(get_settings).post(set_settings))
        // Email
        .route("/email/test", post(send_test_email))
        // User management
        .route("/users", get(list_users).post(create_user))
        .route("/users/{userId}", get(get_user).delete(delete_user))
        .route(
            "/users/{userId}/preferences",
            get(get_user_preferences).patch(update_user_preferences),
        )
        .route("/users/{userId}/role", post(set_user_role))
        .route("/users/{userId}/password", post(set_user_password))
        .route("/users/{userId}/username", post(set_user_username))
        .route("/users/{userId}/ban", post(ban_user))
        .route("/users/{userId}/unban", post(unban_user))
        .route("/users/{userId}/rooms", get(get_user_rooms))
        // Batch user operations
        .route("/users/batch/ban", post(batch_ban_users))
        .route("/users/batch/delete", post(batch_delete_users))
        // Room management
        .route("/rooms", get(list_rooms))
        .route(
            "/rooms/categories",
            get(list_room_categories).post(upsert_room_category),
        )
        .route(
            "/rooms/categories/{categoryId}",
            axum::routing::delete(delete_room_category),
        )
        .route(
            "/rooms/labels",
            get(list_room_labels).post(upsert_room_label),
        )
        .route(
            "/rooms/labels/{labelId}",
            axum::routing::delete(delete_room_label),
        )
        .route("/rooms/{roomId}", get(get_room).delete(delete_room))
        .route("/rooms/{roomId}/taxonomy", patch(update_room_taxonomy))
        .route("/rooms/{roomId}/password", post(set_room_password))
        .route(
            "/rooms/{roomId}/members",
            get(get_room_members).post(add_member),
        )
        .route(
            "/rooms/{roomId}/members/{userId}",
            patch(update_member_permissions).delete(kick_member),
        )
        .route(
            "/rooms/{roomId}/members/{userId}/remark-name",
            patch(update_member_remark_name),
        )
        .route(
            "/rooms/{roomId}/members/{userId}/display-tag",
            patch(update_member_display_tag),
        )
        .route("/rooms/{roomId}/ban", post(ban_room))
        .route("/rooms/{roomId}/unban", post(unban_room))
        .route(
            "/rooms/{roomId}/settings",
            get(get_room_settings).post(set_room_settings),
        )
        .route("/rooms/{roomId}/settings/reset", post(reset_room_settings))
        // Batch room operations
        .route("/rooms/batch/ban", post(batch_ban_rooms))
        .route("/rooms/batch/delete", post(batch_delete_rooms))
        // Stream management
        .route("/streams", get(list_streams))
        .route("/streams/kick", post(kick_stream))
        // Admin management (root only)
        .route("/admins", get(list_admins))
        .route("/admins/{userId}", post(add_admin).delete(remove_admin))
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub(crate) struct AdminServerStateQuery {
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    all_nodes: bool,
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub(crate) struct AdminSliceCacheQuery {
    #[serde(default)]
    node_id: String,
    #[serde(default)]
    all_nodes: bool,
}

async fn get_server_state(
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    ProtoQuery(query): ProtoQuery<AdminServerStateQuery>,
) -> AppResult<Json<synctv_api_common::status::ServerStateResponse>> {
    execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        |_api, _validated, _ctx| async { Ok(()) },
    )
    .await?;

    let response = state
        .shared_api_runtime
        .server_state_runtime
        .collect_server_state(synctv_api_common::status::ServerStateSelection {
            node_id: (!query.node_id.trim().is_empty()).then_some(query.node_id),
            all_nodes: query.all_nodes,
        })
        .await?;
    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/slice-cache",
        tag = "Admin",
        params(AdminSliceCacheQuery),
        responses(
            (status = 200, description = "Slice cache stats", body = admin::GetSliceCacheStatsResponse),
            (status = 400, description = "Invalid target selection", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Slice cache management unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
async fn get_slice_cache_stats(
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    ProtoQuery(query): ProtoQuery<AdminSliceCacheQuery>,
) -> AppResult<Json<admin::GetSliceCacheStatsResponse>> {
    let req = admin::GetSliceCacheStatsRequest {
        node_id: query.node_id,
        all_nodes: query.all_nodes,
    };
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        |_api, _validated, _ctx| async { Ok(()) },
    )
    .await?;

    let response = state
        .shared_api_runtime
        .slice_cache_management_runtime
        .get_stats(synctv_api_common::status::SliceCacheSelection {
            node_id: (!req.node_id.trim().is_empty()).then_some(req.node_id),
            all_nodes: req.all_nodes,
        })
        .await?;
    Ok(Json(
        synctv_api_common::impls::admin::slice_cache_stats_to_admin_proto(response),
    ))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/slice-cache/purge",
        tag = "Admin",
        request_body = admin::PurgeSliceCacheRequest,
        responses(
            (status = 200, description = "Slice cache purged", body = admin::PurgeSliceCacheResponse),
            (status = 400, description = "Invalid target selection", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Slice cache management unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
async fn purge_slice_cache(
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    Json(req): Json<admin::PurgeSliceCacheRequest>,
) -> AppResult<Json<admin::PurgeSliceCacheResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        |_api, _validated, _ctx| async { Ok(()) },
    )
    .await?;

    let response = state
        .shared_api_runtime
        .slice_cache_management_runtime
        .purge(synctv_api_common::status::SliceCacheSelection {
            node_id: (!req.node_id.trim().is_empty()).then_some(req.node_id),
            all_nodes: req.all_nodes,
        })
        .await?;
    Ok(Json(
        synctv_api_common::impls::admin::slice_cache_purge_to_admin_proto(response),
    ))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/slice-cache/evict-expired",
        tag = "Admin",
        request_body = admin::EvictExpiredSliceCacheRequest,
        responses(
            (status = 200, description = "Expired slice cache entries evicted", body = admin::EvictExpiredSliceCacheResponse),
            (status = 400, description = "Invalid target selection", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 503, description = "Slice cache management unavailable", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
async fn evict_expired_slice_cache(
    State(state): State<AppState>,
    request_meta: RequestMetadata,
    Json(req): Json<admin::EvictExpiredSliceCacheRequest>,
) -> AppResult<Json<admin::EvictExpiredSliceCacheResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        |_api, _validated, _ctx| async { Ok(()) },
    )
    .await?;

    let response = state
        .shared_api_runtime
        .slice_cache_management_runtime
        .evict_expired(synctv_api_common::status::SliceCacheSelection {
            node_id: (!req.node_id.trim().is_empty()).then_some(req.node_id),
            all_nodes: req.all_nodes,
        })
        .await?;
    Ok(Json(
        synctv_api_common::impls::admin::slice_cache_evict_expired_to_admin_proto(response),
    ))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reviews/user-registrations",
        tag = "Admin",
        params(admin::ListUserRegistrationReviewsRequest),
        responses(
            (status = 200, description = "User registration reviews", body = admin::ListUserRegistrationReviewsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_user_registration_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListUserRegistrationReviewsRequest>,
) -> AppResult<Json<admin::ListUserRegistrationReviewsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_user_registration_reviews(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/user-registrations/approve",
        tag = "Admin",
        request_body = admin::ApproveUserRegistrationReviewRequest,
        responses(
            (status = 200, description = "User registration review approved", body = admin::ApproveUserRegistrationReviewResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_user_registration_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::ApproveUserRegistrationReviewRequest>,
) -> AppResult<Json<admin::ApproveUserRegistrationReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.approve_user_registration_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/user-registrations/reject",
        tag = "Admin",
        request_body = admin::RejectUserRegistrationReviewRequest,
        responses(
            (status = 200, description = "User registration review rejected", body = admin::UserRegistrationReview),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reject_user_registration_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::RejectUserRegistrationReviewRequest>,
) -> AppResult<Json<admin::UserRegistrationReview>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.reject_user_registration_review(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reviews/room-creations",
        tag = "Admin",
        params(admin::ListRoomCreationReviewsRequest),
        responses((status = 200, description = "Room creation reviews", body = admin::ListRoomCreationReviewsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_room_creation_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListRoomCreationReviewsRequest>,
) -> AppResult<Json<admin::ListRoomCreationReviewsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_room_creation_reviews(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-creations/approve",
        tag = "Admin",
        request_body = admin::ApproveRoomCreationReviewRequest,
        responses((status = 200, description = "Room creation review approved", body = admin::ApproveRoomCreationReviewResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_room_creation_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::ApproveRoomCreationReviewRequest>,
) -> AppResult<Json<admin::ApproveRoomCreationReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.approve_room_creation_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-creations/reject",
        tag = "Admin",
        request_body = admin::RejectRoomCreationReviewRequest,
        responses((status = 200, description = "Room creation review rejected", body = admin::RoomCreationReview)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reject_room_creation_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::RejectRoomCreationReviewRequest>,
) -> AppResult<Json<admin::RoomCreationReview>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.reject_room_creation_review(req, &validated.user_id)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reviews/room-joins",
        tag = "Admin",
        params(admin::ListRoomJoinReviewsRequest),
        responses((status = 200, description = "Room join reviews", body = admin::ListRoomJoinReviewsResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_room_join_reviews(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListRoomJoinReviewsRequest>,
) -> AppResult<Json<admin::ListRoomJoinReviewsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_room_join_reviews(req, &validated.user_id).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-joins/approve",
        tag = "Admin",
        request_body = admin::ApproveRoomJoinReviewRequest,
        responses((status = 200, description = "Room join review approved", body = admin::ApproveRoomJoinReviewResponse)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn approve_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::ApproveRoomJoinReviewRequest>,
) -> AppResult<Json<admin::ApproveRoomJoinReviewResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.approve_room_join_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reviews/room-joins/reject",
        tag = "Admin",
        request_body = admin::RejectRoomJoinReviewRequest,
        responses((status = 200, description = "Room join review rejected", body = admin::RoomJoinReview)),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reject_room_join_review(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::RejectRoomJoinReviewRequest>,
) -> AppResult<Json<admin::RoomJoinReview>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.reject_room_join_review(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/bans",
        tag = "Admin",
        params(admin::ListBanRecordsRequest),
        responses(
            (status = 200, description = "Ban records", body = admin::ListBanRecordsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_ban_records(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListBanRecordsRequest>,
) -> AppResult<Json<admin::ListBanRecordsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move { api.list_ban_records(req, &validated.user_id).await },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reports",
        tag = "Admin",
        params(
            ("page" = Option<i32>, Query, description = "Page number"),
            ("pageSize" = Option<i32>, Query, description = "Page size"),
            ("status" = Option<i32>, Query, description = "Content report status"),
            ("targetType" = Option<i32>, Query, description = "Content report target type"),
            ("reporterUserId" = Option<String>, Query, description = "Reporter public user id"),
            ("roomId" = Option<String>, Query, description = "Related public room id"),
            ("targetRoomId" = Option<String>, Query, description = "Reported room public id"),
            ("targetUserId" = Option<String>, Query, description = "Reported public user id"),
            ("targetMemberRoomId" = Option<String>, Query, description = "Reported member room public id"),
            ("targetMemberUserId" = Option<String>, Query, description = "Reported room member public user id"),
            ("targetChatMessageId" = Option<i64>, Query, description = "Reported chat message id"),
            ("scope" = Option<i32>, Query, description = "Report list scope"),
            ("search" = Option<String>, Query, description = "Search text")
        ),
        responses(
            (status = 200, description = "Content reports", body = admin::ListContentReportsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_content_reports(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListContentReportsRequest>,
) -> AppResult<Json<admin::ListContentReportsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_content_reports(req, &validated.user_id).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/reports/{reportId}",
        tag = "Admin",
        params(("reportId" = String, Path, description = "Content report public id")),
        responses(
            (status = 200, description = "Content report", body = admin::ContentReport),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_content_report(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(report_id): Path<String>,
) -> AppResult<Json<admin::ContentReport>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            api.get_content_report(
                admin::GetContentReportRequest { report_id },
                &validated.user_id,
            )
            .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/reports/{reportId}/status",
        tag = "Admin",
        params(("reportId" = String, Path, description = "Content report public id")),
        request_body = admin::UpdateContentReportStatusRequest,
        responses(
            (status = 200, description = "Content report status updated", body = admin::UpdateContentReportStatusResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_content_report_status(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(report_id): Path<String>,
    Json(mut req): Json<admin::UpdateContentReportStatusRequest>,
) -> AppResult<Json<admin::UpdateContentReportStatusResponse>> {
    req.report_id = report_id;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_content_report_status(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Service state

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/service-state",
        tag = "Admin",
        responses(
            (status = 200, description = "Service state", body = admin::GetServiceStateResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_service_state(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<admin::GetServiceStateResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move { api.get_service_state().await },
    )
    .await?;
    Ok(Json(resp))
}

// Settings

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/settings",
        tag = "Admin",
        responses(
            (status = 200, description = "Runtime settings", body = admin::RuntimeSettings),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
) -> AppResult<Json<admin::RuntimeSettings>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.get_settings(&validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/settings",
        tag = "Admin",
        request_body = admin::UpdateSettingsRequest,
        responses(
            (status = 200, description = "Settings updated", body = admin::RuntimeSettings),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::UpdateSettingsRequest>,
) -> AppResult<Json<admin::RuntimeSettings>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_settings(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Email

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/email/test",
        tag = "Admin",
        request_body = admin::SendTestEmailRequest,
        responses(
            (status = 200, description = "Test email sent", body = admin::SendTestEmailResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn send_test_email(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::SendTestEmailRequest>,
) -> AppResult<Json<admin::SendTestEmailResponse>> {
    let request_meta = request_meta.0;
    let api = require_admin_api(&state)?.clone();
    let executor = api.clone();
    let resp = executor
        .execute_admin_endpoint_with_control(&request_meta, move |request_control, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.send_test_email_with_control(req, Some(&request_control))
                .await
        })
        .await
        .map_err(AppError::from)?;
    Ok(Json(resp))
}

// User Management

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users",
        tag = "Admin",
        params(admin::ListUsersRequest),
        responses(
            (status = 200, description = "Users list", body = admin::ListUsersResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListUsersRequest>,
) -> AppResult<Json<admin::ListUsersResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_users(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users/{userId}",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User detail", body = admin::AdminUser),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "User not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetUserRequest>,
) -> AppResult<Json<admin::AdminUser>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_user(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users/{userId}/preferences",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User preferences", body = admin::GetUserPreferencesResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "User not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetUserPreferencesRequest>,
) -> AppResult<Json<admin::GetUserPreferencesResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_user_preferences(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/admin/users/{userId}/preferences",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        request_body = admin::UpdateUserPreferencesRequest,
        responses(
            (status = 200, description = "User preferences updated", body = admin::UpdateUserPreferencesResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "User not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_user_preferences(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(mut req): Json<admin::UpdateUserPreferencesRequest>,
) -> AppResult<Json<admin::UpdateUserPreferencesResponse>> {
    req.user_id = path.user_id;
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_user_preferences(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users",
        tag = "Admin",
        request_body = admin::CreateUserRequest,
        responses(
            (status = 200, description = "User created", body = admin::AdminUser),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn create_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::CreateUserRequest>,
) -> AppResult<Json<admin::AdminUser>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.create_user(req, validated.role, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/users/{userId}",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User deleted", body = admin::DeleteUserResponse),
            (status = 401, description = "Root authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::DeleteUserRequest>,
) -> AppResult<Json<admin::DeleteUserResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp =
        execute_root_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.delete_user(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{userId}/role",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        request_body = admin::UpdateUserRoleRequest,
        responses(
            (status = 200, description = "User role updated", body = admin::AdminUser),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_user_role(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(mut req): Json<admin::UpdateUserRoleRequest>,
) -> AppResult<Json<admin::AdminUser>> {
    req.user_id = path.user_id;
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_user_role(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{userId}/password",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        request_body = admin::SetUserPasswordRequest,
        responses(
            (status = 200, description = "User password updated", body = admin::SetUserPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_user_password(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(mut req): Json<admin::SetUserPasswordRequest>,
) -> AppResult<Json<admin::SetUserPasswordResponse>> {
    req.user_id = path.user_id;
    if req.reason.is_empty() {
        req.reason = "Admin set user password".to_string();
    }
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.set_user_password(req, validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{userId}/username",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        request_body = admin::UpdateUserUsernameRequest,
        responses(
            (status = 200, description = "Username updated", body = admin::AdminUser),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_user_username(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(mut req): Json<admin::UpdateUserUsernameRequest>,
) -> AppResult<Json<admin::AdminUser>> {
    req.user_id = path.user_id;
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_user_username(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{userId}/ban",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        request_body = admin::BanUserRequest,
        responses(
            (status = 200, description = "User banned", body = admin::AdminUser),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn ban_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    Json(mut req): Json<admin::BanUserRequest>,
) -> AppResult<Json<admin::AdminUser>> {
    req.user_id = path.user_id;
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.ban_user(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/{userId}/unban",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "User unbanned", body = admin::AdminUser),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn unban_user(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::UnbanUserRequest>,
) -> AppResult<Json<admin::AdminUser>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.unban_user(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/users/{userId}/rooms",
        tag = "Admin",
        params(
            ("userId" = String, Path, description = "User ID"),
            AdminGetUserRoomsQuery
        ),
        responses(
            (status = 200, description = "Rooms belonging to user", body = admin::GetUserRoomsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_user_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::UserPathRequest>,
    ProtoQuery(query): ProtoQuery<AdminGetUserRoomsQuery>,
) -> AppResult<Json<admin::GetUserRoomsResponse>> {
    let req = query.into_request(path.user_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_user_rooms(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Batch User Operations

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/batch/ban",
        tag = "Admin",
        request_body = admin::BatchBanUsersRequest,
        responses(
            (status = 200, description = "Users batch banned", body = admin::BatchBanUsersResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_ban_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchBanUsersRequest>,
) -> AppResult<Json<admin::BatchBanUsersResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_ban_users(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/users/batch/delete",
        tag = "Admin",
        request_body = admin::BatchDeleteUsersRequest,
        responses(
            (status = 200, description = "Users batch deleted", body = admin::BatchDeleteUsersResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Root authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_delete_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchDeleteUsersRequest>,
) -> AppResult<Json<admin::BatchDeleteUsersResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_root_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_delete_users(req, &validated.user_id, validated.role, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Room Management

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms",
        tag = "Admin",
        params(admin::ListRoomsRequest),
        responses(
            (status = 200, description = "Admin room list", body = admin::ListRoomsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListRoomsRequest>,
) -> AppResult<Json<admin::ListRoomsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_rooms(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/categories",
        tag = "Admin",
        params(admin::ListRoomCategoriesRequest),
        responses(
            (status = 200, description = "Room categories", body = admin::ListRoomCategoriesResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_room_categories(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListRoomCategoriesRequest>,
) -> AppResult<Json<admin::ListRoomCategoriesResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_room_categories(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/categories",
        tag = "Admin",
        request_body = admin::UpsertRoomCategoryRequest,
        responses(
            (status = 200, description = "Room category upserted", body = client::RoomCategory),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn upsert_room_category(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::UpsertRoomCategoryRequest>,
) -> AppResult<Json<client::RoomCategory>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.upsert_room_category(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/rooms/categories/{categoryId}",
        tag = "Admin",
        params(("categoryId" = String, Path, description = "Room category ID")),
        responses(
            (status = 200, description = "Room category deleted", body = admin::DeleteRoomCategoryResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_room_category(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::DeleteRoomCategoryRequest>,
) -> AppResult<Json<admin::DeleteRoomCategoryResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.delete_room_category(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/labels",
        tag = "Admin",
        params(admin::ListRoomLabelsRequest),
        responses(
            (status = 200, description = "Room labels", body = admin::ListRoomLabelsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_room_labels(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListRoomLabelsRequest>,
) -> AppResult<Json<admin::ListRoomLabelsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_room_labels(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/labels",
        tag = "Admin",
        request_body = admin::UpsertRoomLabelRequest,
        responses(
            (status = 200, description = "Room label upserted", body = client::RoomLabel),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn upsert_room_label(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::UpsertRoomLabelRequest>,
) -> AppResult<Json<client::RoomLabel>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.upsert_room_label(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/rooms/labels/{labelId}",
        tag = "Admin",
        params(("labelId" = String, Path, description = "Room label ID")),
        responses(
            (status = 200, description = "Room label deleted", body = admin::DeleteRoomLabelResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_room_label(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::DeleteRoomLabelRequest>,
) -> AppResult<Json<admin::DeleteRoomLabelResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.delete_room_label(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/admin/rooms/{roomId}/taxonomy",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = admin::UpdateRoomTaxonomyRequest,
        responses(
            (status = 200, description = "Room taxonomy updated", body = admin::Room),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_room_taxonomy(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::GetRoomRequest>,
    Json(mut req): Json<admin::UpdateRoomTaxonomyRequest>,
) -> AppResult<Json<admin::Room>> {
    req.room_id = path.room_id;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_room_taxonomy(req, &validated.user_id).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/{roomId}",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Admin room detail", body = admin::Room),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetRoomRequest>,
) -> AppResult<Json<admin::Room>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_room(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/rooms/{roomId}",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room deleted", body = admin::DeleteRoomResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn delete_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::DeleteRoomRequest>,
) -> AppResult<Json<admin::DeleteRoomResponse>> {
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.delete_room(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{roomId}/password",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = admin::UpdateRoomPasswordRequest,
        responses(
            (status = 200, description = "Room password updated", body = admin::UpdateRoomPasswordResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_room_password(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Json(mut req): Json<admin::UpdateRoomPasswordRequest>,
) -> AppResult<Json<admin::UpdateRoomPasswordResponse>> {
    req.room_id = path.room_id;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_room_password(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/{roomId}/members",
        tag = "Admin",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            AdminGetRoomMembersQuery
        ),
        responses(
            (status = 200, description = "Room members", body = admin::GetRoomMembersResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_room_members(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    ProtoQuery(query): ProtoQuery<AdminGetRoomMembersQuery>,
) -> AppResult<Json<admin::GetRoomMembersResponse>> {
    let req = query.into_request(path.room_id);
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_room_members(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{roomId}/members",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = admin::AddMemberRequest,
        responses(
            (status = 200, description = "Room member added", body = synctv_proto::common::RoomMember),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn add_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Json(mut req): Json<admin::AddMemberRequest>,
) -> AppResult<Json<synctv_proto::common::RoomMember>> {
    req.room_id = path.room_id;
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.add_member(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/admin/rooms/{roomId}/members/{userId}",
        tag = "Admin",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("userId" = String, Path, description = "Target user ID")
        ),
        request_body = admin::UpdateMemberPermissionsRequest,
        responses(
            (status = 200, description = "Room member permissions updated", body = synctv_proto::common::RoomMember),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_member_permissions(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomMemberTargetPath>,
    Json(mut req): Json<admin::UpdateMemberPermissionsRequest>,
) -> AppResult<Json<synctv_proto::common::RoomMember>> {
    req.room_id = path.room_id;
    req.user_id = path.user_id;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_member_permissions(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/admin/rooms/{roomId}/members/{userId}/remark-name",
        tag = "Admin",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("userId" = String, Path, description = "Target user ID")
        ),
        request_body = admin::UpdateMemberRemarkNameRequest,
        responses(
            (status = 200, description = "Room member remark name updated", body = synctv_proto::common::RoomMember),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_member_remark_name(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomMemberTargetPath>,
    Json(mut req): Json<admin::UpdateMemberRemarkNameRequest>,
) -> AppResult<Json<synctv_proto::common::RoomMember>> {
    req.room_id = path.room_id;
    req.user_id = path.user_id;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_member_remark_name(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/admin/rooms/{roomId}/members/{userId}/display-tag",
        tag = "Admin",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("userId" = String, Path, description = "Target user ID")
        ),
        request_body = admin::UpdateMemberDisplayTagRequest,
        responses(
            (status = 200, description = "Room member display tag updated", body = synctv_proto::common::RoomMember),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn update_member_display_tag(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomMemberTargetPath>,
    Json(mut req): Json<admin::UpdateMemberDisplayTagRequest>,
) -> AppResult<Json<synctv_proto::common::RoomMember>> {
    req.room_id = path.room_id;
    req.user_id = path.user_id;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.update_member_display_tag(req, &validated.user_id, &rctx)
                .await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/rooms/{roomId}/members/{userId}",
        tag = "Admin",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("userId" = String, Path, description = "Target user ID")
        ),
        request_body = admin::KickMemberRequest,
        responses(
            (status = 200, description = "Room member kicked", body = admin::KickMemberResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn kick_member(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<RoomMemberTargetPath>,
    Json(mut req): Json<admin::KickMemberRequest>,
) -> AppResult<Json<admin::KickMemberResponse>> {
    req.room_id = path.room_id;
    req.user_id = path.user_id;
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.kick_member(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{roomId}/ban",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = admin::BanRoomRequest,
        responses(
            (status = 200, description = "Room banned", body = admin::Room),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn ban_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Json(mut req): Json<admin::BanRoomRequest>,
) -> AppResult<Json<admin::Room>> {
    req.room_id = path.room_id;
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.ban_room(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{roomId}/unban",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room unbanned", body = admin::Room),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn unban_room(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::UnbanRoomRequest>,
) -> AppResult<Json<admin::Room>> {
    let resp =
        execute_admin_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.unban_room(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/rooms/{roomId}/settings",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room settings", body = admin::GetRoomSettingsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn get_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::GetRoomSettingsRequest>,
) -> AppResult<Json<admin::GetRoomSettingsResponse>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.get_room_settings(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{roomId}/settings",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = admin::UpdateRoomSettingsRequest,
        responses(
            (status = 200, description = "Room settings updated", body = admin::Room),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn set_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<admin::RoomPathRequest>,
    Json(mut req): Json<admin::UpdateRoomSettingsRequest>,
) -> AppResult<Json<admin::Room>> {
    req.room_id = path.room_id;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.update_room_settings(req, &validated.user_id).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/{roomId}/settings/reset",
        tag = "Admin",
        params(("roomId" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room settings reset", body = admin::Room),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn reset_room_settings(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::ResetRoomSettingsRequest>,
) -> AppResult<Json<admin::Room>> {
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.reset_room_settings(req, &validated.user_id).await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Batch Room Operations

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/batch/ban",
        tag = "Admin",
        request_body = admin::BatchBanRoomsRequest,
        responses(
            (status = 200, description = "Rooms batch banned", body = admin::BatchBanRoomsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_ban_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchBanRoomsRequest>,
) -> AppResult<Json<admin::BatchBanRoomsResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_ban_rooms(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/rooms/batch/delete",
        tag = "Admin",
        request_body = admin::BatchDeleteRoomsRequest,
        responses(
            (status = 200, description = "Rooms batch deleted", body = admin::BatchDeleteRoomsResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn batch_delete_rooms(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::BatchDeleteRoomsRequest>,
) -> AppResult<Json<admin::BatchDeleteRoomsResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.batch_delete_rooms(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

// Stream Management

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/streams",
        tag = "Admin",
        params(admin::ListActiveStreamsRequest),
        responses(
            (status = 200, description = "Active streams", body = admin::ListActiveStreamsResponse),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_streams(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListActiveStreamsRequest>,
) -> AppResult<Json<admin::ListActiveStreamsResponse>> {
    let response = execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_active_streams(req).await
        },
    )
    .await?;
    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/streams/kick",
        tag = "Admin",
        request_body = admin::KickStreamRequest,
        responses(
            (status = 200, description = "Stream kicked", body = admin::KickStreamResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Admin authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn kick_stream(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Json(req): Json<admin::KickStreamRequest>,
) -> AppResult<Json<admin::KickStreamResponse>> {
    execute_admin_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.kick_stream(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(admin::KickStreamResponse {}))
}

// Admin Management (Root Only)

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/admin/admins",
        tag = "Admin",
        responses(
            (status = 200, description = "Admins list", body = admin::ListAdminsResponse),
            (status = 401, description = "Root authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn list_admins(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    ProtoQuery(req): ProtoQuery<admin::ListAdminsRequest>,
) -> AppResult<Json<admin::ListAdminsResponse>> {
    let resp = execute_root_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, _, _| async move {
            synctv_api_common::impls::validate_proto_request(&req)?;
            api.list_admins(req).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/admin/admins/{userId}",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "Admin added", body = admin::AdminUser),
            (status = 401, description = "Root authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn add_admin(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::AddAdminRequest>,
) -> AppResult<Json<admin::AdminUser>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp =
        execute_root_endpoint(
            &state,
            request_meta,
            require_admin_api,
            move |api, validated, rctx| async move {
                api.add_admin(req, &validated.user_id, &rctx).await
            },
        )
        .await?;
    Ok(Json(resp))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/admin/admins/{userId}",
        tag = "Admin",
        params(("userId" = String, Path, description = "User ID")),
        responses(
            (status = 200, description = "Admin removed", body = admin::RemoveAdminResponse),
            (status = 401, description = "Root authentication required", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub(crate) async fn remove_admin(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(req): Path<admin::RemoveAdminRequest>,
) -> AppResult<Json<admin::RemoveAdminResponse>> {
    synctv_api_common::impls::validate_proto_request(&req).map_err(super::error::map_api_error)?;
    let resp = execute_root_endpoint(
        &state,
        request_meta,
        require_admin_api,
        move |api, validated, rctx| async move {
            api.remove_admin(req, &validated.user_id, &rctx).await
        },
    )
    .await?;
    Ok(Json(resp))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        extract::{ConnectInfo, FromRequestParts},
        http::Request,
    };
    use std::net::SocketAddr;

    type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

    fn test_error(message: impl Into<String>) -> Box<dyn std::error::Error + Send + Sync> {
        anyhow::anyhow!(message.into()).into()
    }

    #[test]
    fn test_update_user_role_request_rejects_string_role() {
        let err = serde_json::from_str::<admin::UpdateUserRoleRequest>(
            r#"{"userId":"usr_1","role":"admin"}"#,
        )
        .expect_err("string role should be rejected");
        assert!(err.is_data());
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_request_context_uses_trusted_proxy_headers_for_audit_ip() -> TestResult {
        let mut state = crate::http::tests::test_app_state();
        {
            let router_options = std::sync::Arc::make_mut(&mut state.router_options);
            let config = std::sync::Arc::make_mut(&mut router_options.runtime_settings);
            config.server.trusted_proxies = vec!["127.0.0.1".to_string()];
        }

        let mut request = Request::builder()
            .uri("/admin/test")
            .header("X-Forwarded-For", "203.0.113.10")
            .header("User-Agent", "audit-test")
            .body(())
            .map_err(|err| test_error(format!("request should build: {err}")))?;
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:8080".parse::<SocketAddr>()?));

        let (mut parts, ()) = request.into_parts();
        let request_meta =
            crate::http::middleware::RequestMetadata::from_request_parts(&mut parts, &state)
                .await
                .map_err(|err| test_error(format!("extractor should not fail: {err}")))?;
        let ctx = crate::http::admin_execute::request_context(&request_meta.0);

        assert_eq!(ctx.ip_address.as_deref(), Some("203.0.113.10"));
        assert_eq!(ctx.user_agent.as_deref(), Some("audit-test"));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_request_context_ignores_forwarded_headers_from_untrusted_proxy() -> TestResult {
        let state = crate::http::tests::test_app_state();

        let mut request = Request::builder()
            .uri("/admin/test")
            .header("X-Forwarded-For", "203.0.113.10")
            .body(())
            .map_err(|err| test_error(format!("request should build: {err}")))?;
        request
            .extensions_mut()
            .insert(ConnectInfo("198.51.100.7:8080".parse::<SocketAddr>()?));

        let (mut parts, ()) = request.into_parts();
        let request_meta =
            crate::http::middleware::RequestMetadata::from_request_parts(&mut parts, &state)
                .await
                .map_err(|err| test_error(format!("extractor should not fail: {err}")))?;
        let ctx = crate::http::admin_execute::request_context(&request_meta.0);

        assert_eq!(ctx.ip_address.as_deref(), Some("198.51.100.7"));
        Ok(())
    }

    #[tokio::test]
    #[ignore = "Requires Docker-backed PostgreSQL"]
    async fn test_require_admin_api_error() -> TestResult {
        let mut state = crate::http::tests::test_app_state();
        Arc::make_mut(&mut state.shared_api_runtime).admin_api = None;

        let Err(err) = require_admin_api(&state) else {
            return Err(test_error("missing admin api should fail"));
        };
        assert_eq!(err.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            err.message(),
            "Admin service is not available on this server."
        );
        Ok(())
    }
}
