use tonic::{Request, Response, Status};

use super::super::{map_api_error, ClientServiceImpl};
use futures::StreamExt;
use synctv_api_common::impls::EndpointRateLimitCategory;
use synctv_proto::client::*;

pub(super) async fn create_chat_attachment_upload_session(
    service: &ClientServiceImpl,
    request: Request<CreateChatAttachmentUploadSessionRequest>,
) -> Result<Response<CreateChatAttachmentUploadSessionResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api
                    .create_chat_attachment_upload_session_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn upload_chat_attachment_object(
    service: &ClientServiceImpl,
    request: Request<UploadChatAttachmentObjectRequest>,
) -> Result<Response<UploadChatAttachmentObjectResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move { client_api.upload_chat_attachment_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn complete_chat_attachment_upload_session(
    service: &ClientServiceImpl,
    request: Request<CompleteChatAttachmentUploadSessionRequest>,
) -> Result<Response<CompleteChatAttachmentUploadSessionResponse>, Status> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let response = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Write, move || {
            let client_api = service.client_api.clone();
            async move {
                client_api
                    .complete_chat_attachment_upload_session(req)
                    .await
            }
        })
        .await
        .map_err(map_api_error)?;
    Ok(Response::new(response))
}

pub(super) async fn get_chat_attachment_object(
    service: &ClientServiceImpl,
    request: Request<GetChatAttachmentObjectRequest>,
) -> Result<
    Response<
        <ClientServiceImpl as room_service_server::RoomService>::GetChatAttachmentObjectStream,
    >,
    Status,
> {
    let metadata = service.request_metadata(&request)?;
    let req = request.into_inner();
    let room_id = req.room_id.clone();
    let download = service
        .client_api
        .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, move || {
            let client_api = service.client_api.clone();
            async move { client_api.get_chat_attachment_object(req).await }
        })
        .await
        .map_err(map_api_error)?;
    let stream = synctv_api_common::impls::client::file_download::chat_attachment_chunk_stream(
        room_id, download,
    )
    .map(|result| result.map_err(map_api_error));
    Ok(Response::new(Box::pin(stream)))
}

pub(super) async fn get_chat_history(
    service: &ClientServiceImpl,
    request: Request<GetChatHistoryRequest>,
) -> Result<Response<GetChatHistoryResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.get_chat_history_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn search_chat_messages(
    service: &ClientServiceImpl,
    request: Request<SearchChatMessagesRequest>,
) -> Result<Response<SearchChatMessagesResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.search_chat_messages_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn get_chat_message(
    service: &ClientServiceImpl,
    request: Request<GetChatMessageRequest>,
) -> Result<Response<ChatMessageReceive>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.get_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn get_chat_message_context(
    service: &ClientServiceImpl,
    request: Request<GetChatMessageContextRequest>,
) -> Result<Response<GetChatMessageContextResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .get_chat_message_context_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn get_chat_playback_messages(
    service: &ClientServiceImpl,
    request: Request<GetChatPlaybackMessagesRequest>,
) -> Result<Response<GetChatPlaybackMessagesResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .get_chat_playback_messages_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn mark_chat_read(
    service: &ClientServiceImpl,
    request: Request<MarkChatReadRequest>,
) -> Result<Response<ChatReadStateResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.mark_chat_read_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn get_chat_read_state(
    service: &ClientServiceImpl,
    request: Request<GetChatReadStateRequest>,
) -> Result<Response<ChatReadStateResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api.get_chat_read_state_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn get_chat_message_read_receipts(
    service: &ClientServiceImpl,
    request: Request<GetChatMessageReadReceiptsRequest>,
) -> Result<Response<GetChatMessageReadReceiptsResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .get_chat_message_read_receipts_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn report_content(
    service: &ClientServiceImpl,
    request: Request<ReportContentRequest>,
) -> Result<Response<ReportContentResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.report_content_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn list_room_content_reports(
    service: &ClientServiceImpl,
    request: Request<ListRoomContentReportsRequest>,
) -> Result<Response<ListRoomContentReportsResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .list_room_content_reports_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn get_room_content_report(
    service: &ClientServiceImpl,
    request: Request<GetRoomContentReportRequest>,
) -> Result<Response<ContentReport>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .get_room_content_report_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn update_room_content_report_status(
    service: &ClientServiceImpl,
    request: Request<UpdateRoomContentReportStatusRequest>,
) -> Result<Response<UpdateRoomContentReportStatusResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api
                    .update_room_content_report_status_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn send_chat_message(
    service: &ClientServiceImpl,
    request: Request<SendChatMessageRequest>,
) -> Result<Response<ChatMessageEventResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.send_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn edit_chat_message(
    service: &ClientServiceImpl,
    request: Request<EditChatMessageRequest>,
) -> Result<Response<ChatMessageEventResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.edit_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn delete_chat_message(
    service: &ClientServiceImpl,
    request: Request<DeleteChatMessageRequest>,
) -> Result<Response<ChatMessageEventResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.delete_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn list_pinned_chat_messages(
    service: &ClientServiceImpl,
    request: Request<ListPinnedChatMessagesRequest>,
) -> Result<Response<ListPinnedChatMessagesResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .list_pinned_chat_messages_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn pin_chat_message(
    service: &ClientServiceImpl,
    request: Request<PinChatMessageRequest>,
) -> Result<Response<ChatPinEventResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.pin_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn unpin_chat_message(
    service: &ClientServiceImpl,
    request: Request<UnpinChatMessageRequest>,
) -> Result<Response<ChatPinEventResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.unpin_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn set_chat_reaction(
    service: &ClientServiceImpl,
    request: Request<SetChatReactionRequest>,
) -> Result<Response<ChatMessageEvent>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Write,
            move |client_api, actor| async move {
                client_api.set_chat_reaction_for_actor(&actor, req).await
            },
        )
        .await?;
    Ok(Response::new(response))
}

pub(super) async fn list_chat_reaction_users(
    service: &ClientServiceImpl,
    request: Request<ListChatReactionUsersRequest>,
) -> Result<Response<ListChatReactionUsersResponse>, Status> {
    let (metadata, room_id) = service.room_request_context(&request)?;
    let req = request.into_inner();
    let response = service
        .execute_room_actor_endpoint(
            metadata,
            room_id,
            EndpointRateLimitCategory::Read,
            move |client_api, actor| async move {
                client_api
                    .list_chat_reaction_users_for_actor(&actor, req)
                    .await
            },
        )
        .await?;
    Ok(Response::new(response))
}
