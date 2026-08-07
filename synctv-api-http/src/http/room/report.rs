use axum::{
    extract::{Path, State},
    Json,
};

use super::execute::execute_room_actor_endpoint;
use crate::http::{middleware::RequestMetadata, validation::ProtoQuery, AppResult, AppState};
use synctv_api_common::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    report_content_request, ContentReport, GetRoomContentReportRequest,
    ListRoomContentReportsRequest, ListRoomContentReportsResponse, ReportContentRequest,
    ReportContentResponse, ReportRoomTarget, UpdateRoomContentReportStatusRequest,
    UpdateRoomContentReportStatusResponse,
};

fn inject_room_report_target(req: &mut ReportContentRequest, room_id: &str) {
    match &mut req.target {
        Some(report_content_request::Target::Room(target)) => {
            target.room_id = room_id.to_string();
        }
        Some(report_content_request::Target::RoomMember(target)) => {
            target.room_id = room_id.to_string();
        }
        Some(report_content_request::Target::ChatMessage(target)) => {
            target.room_id = room_id.to_string();
        }
        Some(report_content_request::Target::User(_)) => {}
        None => {
            req.target = Some(report_content_request::Target::Room(ReportRoomTarget {
                room_id: room_id.to_string(),
            }));
        }
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/reports",
        tag = "Room",
        params(("roomId" = String, Path, description = "Room ID")),
        request_body = ReportContentRequest,
        responses(
            (status = 200, description = "Content report created", body = ReportContentResponse),
            (status = 400, description = "Invalid request", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Insufficient room access", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Target not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn report_content(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(mut req): Json<ReportContentRequest>,
) -> AppResult<Json<ReportContentResponse>> {
    let room_id = path.room_id;
    inject_room_report_target(&mut req, &room_id);
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomReportsCreate,
            move |client_api, actor| async move {
                client_api.report_content_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/reports",
        tag = "Room",
        params(("roomId" = String, Path, description = "Room ID")),
        responses(
            (status = 200, description = "Room-scoped content reports", body = ListRoomContentReportsResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Insufficient room access", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn list_room_content_reports(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<ListRoomContentReportsRequest>,
) -> AppResult<Json<ListRoomContentReportsResponse>> {
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomReportsRead,
        move |client_api, actor| async move {
            client_api
                .list_room_content_reports_for_actor(&actor, req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/reports/{reportId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("reportId" = String, Path, description = "Content report ID")
        ),
        responses(
            (status = 200, description = "Room-scoped content report", body = ContentReport),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Insufficient room access", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Report not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn get_room_content_report(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path((room_id, report_id)): Path<(String, String)>,
) -> AppResult<Json<ContentReport>> {
    let req = GetRoomContentReportRequest { report_id };
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomReportsRead,
        move |client_api, actor| async move {
            client_api
                .get_room_content_report_for_actor(&actor, req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        post,
        path = "/api/rooms/{roomId}/reports/{reportId}/status",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("reportId" = String, Path, description = "Content report ID")
        ),
        request_body = UpdateRoomContentReportStatusRequest,
        responses(
            (status = 200, description = "Room-scoped content report updated", body = UpdateRoomContentReportStatusResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 403, description = "Insufficient room access", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Report not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(("bearer_auth" = []))
    )
)]
pub async fn update_room_content_report_status(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path((room_id, report_id)): Path<(String, String)>,
    Json(mut req): Json<UpdateRoomContentReportStatusRequest>,
) -> AppResult<Json<UpdateRoomContentReportStatusResponse>> {
    req.report_id = report_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomReportsWrite,
        move |client_api, actor| async move {
            client_api
                .update_room_content_report_status_for_actor(&actor, req)
                .await
        },
    )
    .await?;
    Ok(Json(response))
}
