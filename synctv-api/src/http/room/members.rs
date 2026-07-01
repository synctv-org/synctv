use axum::{
    extract::{Path, State},
    Json,
};

use super::execute::execute_room_actor_endpoint;
use crate::http::validation::ProtoQuery;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{GetRoomMembersRequest, GetRoomMembersResponse};

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/members",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            GetRoomMembersRequest
        ),
        responses(
            (status = 200, description = "Room members", body = GetRoomMembersResponse),
            (status = 401, description = "Authentication required", body = crate::openapi::GoogleRpcStatusSchema),
            (status = 404, description = "Room not found", body = crate::openapi::GoogleRpcStatusSchema)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_room_members(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetRoomMembersRequest>,
) -> AppResult<Json<GetRoomMembersResponse>> {
    let room_id = path.room_id;
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomMembers,
            move |client_api, actor| async move {
                client_api.get_room_members_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}
