use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn get_room_members(
    service: &ClientServiceImpl,
    request: Request<GetRoomMembersRequest>,
) -> Result<Response<GetRoomMembersResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.get_room_members_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn list_room_streams(
    service: &ClientServiceImpl,
    request: Request<ListRoomStreamsRequest>,
) -> Result<Response<ListRoomStreamsResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                client_api
                    .list_room_streams(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_room_stream_info(
    service: &ClientServiceImpl,
    request: Request<GetRoomStreamInfoRequest>,
) -> Result<Response<GetRoomStreamInfoResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                client_api
                    .get_room_stream_info(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn kick_room_stream(
    service: &ClientServiceImpl,
    request: Request<KickRoomStreamRequest>,
) -> Result<Response<KickRoomStreamResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .kick_room_stream(&authenticated.user_id(), room_id.as_str(), req)
                    .await
                    .map(|()| KickRoomStreamResponse {})
            },
        )
        .await
        .map(Response::new)
        .map_err(map_api_error)
}

pub(super) async fn add_member(
    service: &ClientServiceImpl,
    request: Request<AddMemberRequest>,
) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .add_member(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn list_room_join_reviews(
    service: &ClientServiceImpl,
    request: Request<ListRoomJoinReviewsRequest>,
) -> Result<Response<ListRoomJoinReviewsResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                client_api
                    .list_room_join_reviews(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn approve_room_join_review(
    service: &ClientServiceImpl,
    request: Request<ApproveRoomJoinReviewRequest>,
) -> Result<Response<ApproveRoomJoinReviewResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .approve_room_join_review(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn reject_room_join_review(
    service: &ClientServiceImpl,
    request: Request<RejectRoomJoinReviewRequest>,
) -> Result<Response<RoomJoinReview>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .reject_room_join_review(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn update_member_permissions(
    service: &ClientServiceImpl,
    request: Request<UpdateMemberPermissionsRequest>,
) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .update_member_permissions(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn update_member_remark_name(
    service: &ClientServiceImpl,
    request: Request<UpdateMemberRemarkNameRequest>,
) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .update_member_remark_name(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn update_member_display_tag(
    service: &ClientServiceImpl,
    request: Request<UpdateMemberDisplayTagRequest>,
) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .update_member_display_tag(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn kick_member(
    service: &ClientServiceImpl,
    request: Request<KickMemberRequest>,
) -> Result<Response<KickMemberResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .kick_member(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}
