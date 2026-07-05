use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use crate::impls::EndpointRateLimitCategory;
use futures::StreamExt;
use synctv_proto::client::*;

pub(super) async fn get_ice_servers(
    service: &ClientServiceImpl,
    request: Request<GetIceServersRequest>,
) -> Result<Response<GetIceServersResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let response =
        service
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.get_ice_servers_for_actor(&actor).await
                },
            )
            .await?;
    Ok(Response::new(response))
}

pub(super) async fn add_media(
    service: &ClientServiceImpl,
    request: Request<AddMediaRequest>,
) -> Result<Response<Media>, Status> {
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
                    .add_media(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_media(
    service: &ClientServiceImpl,
    request: Request<GetMediaRequest>,
) -> Result<Response<Media>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.get_media_for_actor(&actor, &req.media_id).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn create_media_cover_upload_session(
    service: &ClientServiceImpl,
    request: Request<CreateMediaCoverUploadSessionRequest>,
) -> Result<Response<CreateMediaCoverUploadSessionResponse>, Status> {
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
                    .create_media_cover_upload_session(
                        &authenticated.user_id,
                        room_id.as_str(),
                        req,
                    )
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn create_room_cover_upload_session(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::CreateRoomCoverUploadSessionRequest>,
) -> Result<Response<synctv_proto::client::CreateRoomCoverUploadSessionResponse>, Status> {
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
                    .create_room_cover_upload_session(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn upload_room_cover_object(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::UploadRoomCoverObjectRequest>,
) -> Result<Response<synctv_proto::client::UploadRoomCoverObjectResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move { client_api.upload_room_cover_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn complete_room_cover_upload_session(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::CompleteRoomCoverUploadSessionRequest>,
) -> Result<Response<synctv_proto::client::CompleteRoomCoverUploadSessionResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move { client_api.complete_room_cover_upload_session(req).await }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_room_cover_object(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::GetRoomCoverObjectRequest>,
) -> Result<
    Response<<ClientServiceImpl as room_service_server::RoomService>::GetRoomCoverObjectStream>,
    Status,
> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let download = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, move || {
            let client_api = service.client_api.clone();
            async move { client_api.get_room_cover_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    let stream = crate::impls::client::file_download::room_cover_chunk_stream(download)
        .map(|result| result.map_err(map_api_error));
    Ok(Response::new(Box::pin(stream)))
}

pub(super) async fn update_room_cover(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::UpdateRoomCoverRequest>,
) -> Result<Response<GetRoomResponse>, Status> {
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
                    .update_room_cover(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn clear_room_cover(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::ClearRoomCoverRequest>,
) -> Result<Response<GetRoomResponse>, Status> {
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
                    .clear_room_cover(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn upload_media_cover_object(
    service: &ClientServiceImpl,
    request: Request<UploadMediaCoverObjectRequest>,
) -> Result<Response<UploadMediaCoverObjectResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move { client_api.upload_media_cover_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn complete_media_cover_upload_session(
    service: &ClientServiceImpl,
    request: Request<CompleteMediaCoverUploadSessionRequest>,
) -> Result<Response<CompleteMediaCoverUploadSessionResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move { client_api.complete_media_cover_upload_session(req).await }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_media_cover_object(
    service: &ClientServiceImpl,
    request: Request<GetMediaCoverObjectRequest>,
) -> Result<
    Response<<ClientServiceImpl as room_service_server::RoomService>::GetMediaCoverObjectStream>,
    Status,
> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let download = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, move || {
            let client_api = service.client_api.clone();
            async move { client_api.get_media_cover_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    let stream = crate::impls::client::file_download::media_cover_chunk_stream(download)
        .map(|result| result.map_err(map_api_error));
    Ok(Response::new(Box::pin(stream)))
}

pub(super) async fn update_media_cover(
    service: &ClientServiceImpl,
    request: Request<UpdateMediaCoverRequest>,
) -> Result<Response<Media>, Status> {
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
                    .update_media_cover(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn clear_media_cover(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::ClearMediaCoverRequest>,
) -> Result<Response<Media>, Status> {
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
                    .clear_media_cover(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn create_playlist_cover_upload_session(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::CreatePlaylistCoverUploadSessionRequest>,
) -> Result<Response<synctv_proto::client::CreatePlaylistCoverUploadSessionResponse>, Status> {
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
                    .create_playlist_cover_upload_session(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn upload_playlist_cover_object(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::UploadPlaylistCoverObjectRequest>,
) -> Result<Response<synctv_proto::client::UploadPlaylistCoverObjectResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move { client_api.upload_playlist_cover_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn complete_playlist_cover_upload_session(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::CompletePlaylistCoverUploadSessionRequest>,
) -> Result<Response<synctv_proto::client::CompletePlaylistCoverUploadSessionResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move { client_api.complete_playlist_cover_upload_session(req).await }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_playlist_cover_object(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::GetPlaylistCoverObjectRequest>,
) -> Result<
    Response<<ClientServiceImpl as room_service_server::RoomService>::GetPlaylistCoverObjectStream>,
    Status,
> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let download = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, move || {
            let client_api = service.client_api.clone();
            async move { client_api.get_playlist_cover_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    let stream = crate::impls::client::file_download::playlist_cover_chunk_stream(download)
        .map(|result| result.map_err(map_api_error));
    Ok(Response::new(Box::pin(stream)))
}

pub(super) async fn update_playlist_cover(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::UpdatePlaylistCoverRequest>,
) -> Result<Response<Playlist>, Status> {
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
                    .update_playlist_cover(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn clear_playlist_cover(
    service: &ClientServiceImpl,
    request: Request<synctv_proto::client::ClearPlaylistCoverRequest>,
) -> Result<Response<Playlist>, Status> {
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
                    .clear_playlist_cover(&authenticated.user_id, &room_id, req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn delete_media(
    service: &ClientServiceImpl,
    request: Request<DeleteMediaRequest>,
) -> Result<Response<DeleteMediaResponse>, Status> {
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
                    .delete_media(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn delete_entries(
    service: &ClientServiceImpl,
    request: Request<DeleteEntriesRequest>,
) -> Result<Response<DeleteEntriesResponse>, Status> {
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
                    .delete_entries(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn edit_media(
    service: &ClientServiceImpl,
    request: Request<EditMediaRequest>,
) -> Result<Response<Media>, Status> {
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
                    .edit_media(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn list_playlist_items(
    service: &ClientServiceImpl,
    request: Request<ListPlaylistItemsRequest>,
) -> Result<Response<ListPlaylistItemsResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.list_playlist_items_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn move_media(
    service: &ClientServiceImpl,
    request: Request<MoveMediaRequest>,
) -> Result<Response<MoveMediaResponse>, Status> {
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
                    .move_media(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn clear_playlist(
    service: &ClientServiceImpl,
    request: Request<ClearPlaylistRequest>,
) -> Result<Response<ClearPlaylistResponse>, Status> {
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
                    .clear_playlist(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn add_media_batch(
    service: &ClientServiceImpl,
    request: Request<AddMediaBatchRequest>,
) -> Result<Response<AddMediaBatchResponse>, Status> {
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
                    .add_media_batch(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}
