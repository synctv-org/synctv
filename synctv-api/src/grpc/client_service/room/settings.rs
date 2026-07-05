use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use crate::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn update_room_settings(
    service: &ClientServiceImpl,
    request: Request<UpdateRoomSettingsRequest>,
) -> Result<Response<Room>, Status> {
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
                    .update_room_settings(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_room_settings(
    service: &ClientServiceImpl,
    request: Request<GetRoomSettingsRequest>,
) -> Result<Response<GetRoomSettingsResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let response =
        service
            .execute_room_actor_endpoint(
                metadata,
                room_id,
                EndpointRateLimitCategory::Read,
                move |client_api, actor| async move {
                    client_api.get_room_settings_for_actor(&actor).await
                },
            )
            .await?;
    Ok(Response::new(response))
}

pub(super) async fn reset_room_settings(
    service: &ClientServiceImpl,
    request: Request<ResetRoomSettingsRequest>,
) -> Result<Response<RoomSettings>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .reset_room_settings(&authenticated.user_id, room_id.as_str())
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn transfer_room_ownership(
    service: &ClientServiceImpl,
    request: Request<TransferRoomOwnershipRequest>,
) -> Result<Response<Room>, Status> {
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
                    .transfer_room_ownership(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn start_room_password_registration(
    service: &ClientServiceImpl,
    request: Request<StartRoomPasswordRegistrationRequest>,
) -> Result<Response<StartRoomPasswordRegistrationResponse>, Status> {
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
                    .start_room_password_registration(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn finish_room_password_registration(
    service: &ClientServiceImpl,
    request: Request<FinishRoomPasswordRegistrationRequest>,
) -> Result<Response<SetRoomPasswordResponse>, Status> {
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
                    .finish_room_password_registration(
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

pub(super) async fn clear_room_password(
    service: &ClientServiceImpl,
    request: Request<ClearRoomPasswordRequest>,
) -> Result<Response<SetRoomPasswordResponse>, Status> {
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
                    .clear_room_password(&authenticated.user_id, room_id.as_str(), req)
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}
