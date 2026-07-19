use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn create_playlist(
    service: &ClientServiceImpl,
    request: Request<CreatePlaylistRequest>,
) -> Result<Response<Playlist>, Status> {
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
                    .create_playlist(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_playlist(
    service: &ClientServiceImpl,
    request: Request<GetPlaylistRequest>,
) -> Result<Response<GetPlaylistResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let playlist_id = req.playlist_id;
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .get_playlist_for_actor(&actor, &playlist_id)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn update_playlist(
    service: &ClientServiceImpl,
    request: Request<UpdatePlaylistRequest>,
) -> Result<Response<Playlist>, Status> {
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
                    .update_playlist(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn move_playlist(
    service: &ClientServiceImpl,
    request: Request<MovePlaylistRequest>,
) -> Result<Response<Playlist>, Status> {
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
                    .move_playlist(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn delete_playlist(
    service: &ClientServiceImpl,
    request: Request<DeletePlaylistRequest>,
) -> Result<Response<DeletePlaylistResponse>, Status> {
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
                    .delete_playlist(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn list_playlists(
    service: &ClientServiceImpl,
    request: Request<ListPlaylistsRequest>,
) -> Result<Response<ListPlaylistsResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.list_playlists_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}
