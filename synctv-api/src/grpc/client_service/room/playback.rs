use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn start_playback(
    service: &ClientServiceImpl,
    request: Request<StartPlaybackRequest>,
) -> Result<Response<StartPlaybackResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .start_playback(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn stop_playback(
    service: &ClientServiceImpl,
    request: Request<StopPlaybackRequest>,
) -> Result<Response<StopPlaybackResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .stop_playback(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_playback(
    service: &ClientServiceImpl,
    request: Request<GetPlaybackRequest>,
) -> Result<Response<GetPlaybackResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint_with_control(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, request_control, actor| async move {
                client_api
                    .get_playback_for_actor(&actor, req, Some(&request_control))
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn update_playback_state(
    service: &ClientServiceImpl,
    request: Request<UpdatePlaybackStateRequest>,
) -> Result<Response<PlaybackState>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .update_playback_state(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}
