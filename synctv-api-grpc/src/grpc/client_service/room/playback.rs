use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn start_playback(
    service: &ClientServiceImpl,
    request: Request<StartPlaybackRequest>,
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
                    .start_playback(&authenticated.user_id(), room_id.as_str(), req)
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
                    .stop_playback(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn play_next(
    service: &ClientServiceImpl,
    request: Request<PlayNextRequest>,
) -> Result<Response<PlaybackState>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let client_api = service.client_api.clone();
    let response = service
        .client_api
        .clone()
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .play_next(&authenticated.user_id(), &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn play_previous(
    service: &ClientServiceImpl,
    request: Request<PlayPreviousRequest>,
) -> Result<Response<PlaybackState>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let client_api = service.client_api.clone();
    let response = service
        .client_api
        .clone()
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .play_previous(&authenticated.user_id(), &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn list_playback_history(
    service: &ClientServiceImpl,
    request: Request<ListPlaybackHistoryRequest>,
) -> Result<Response<ListPlaybackHistoryResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let client_api = service.client_api.clone();
    let response = service
        .client_api
        .clone()
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Read,
            move |authenticated| async move {
                client_api
                    .list_playback_history(&authenticated.user_id(), &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn play_history_entry(
    service: &ClientServiceImpl,
    request: Request<PlayHistoryEntryRequest>,
) -> Result<Response<PlaybackState>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let client_api = service.client_api.clone();
    let response = service
        .client_api
        .clone()
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Media,
            move |authenticated| async move {
                client_api
                    .play_history_entry(&authenticated.user_id(), &room_id, req)
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
                    .update_playback_state(&authenticated.user_id(), room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}
