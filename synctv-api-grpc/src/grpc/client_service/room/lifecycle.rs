use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn leave_room(
    service: &ClientServiceImpl,
    request: Request<LeaveRoomRequest>,
) -> Result<Response<LeaveRoomResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .leave_room(&authenticated.user_id(), room_id.as_str())
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn delete_room(
    service: &ClientServiceImpl,
    request: Request<DeleteRoomRequest>,
) -> Result<Response<DeleteRoomResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let executor = service.client_api.clone();
    let client_api = service.client_api.clone();
    let response = executor
        .execute_user_endpoint(
            &metadata,
            EndpointRateLimitCategory::Write,
            move |authenticated| async move {
                client_api
                    .delete_room(&authenticated.user_id(), room_id.as_str())
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn create_web_socket_ticket(
    service: &ClientServiceImpl,
    request: Request<CreateWebSocketTicketRequest>,
) -> Result<Response<CreateWebSocketTicketResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let public_room_id = req.room_id.clone();
    let client_api = service.client_api.clone();
    let response =
        synctv_api_common::impls::ClientApiImpl::execute_room_actor_endpoint_with_control(
            client_api.clone(),
            &metadata,
            public_room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, request_control, actor| async move {
                client_api
                    .create_websocket_ticket_for_actor_with_control(
                        actor,
                        req,
                        Some(&request_control),
                    )
                    .await
            },
        )
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}
