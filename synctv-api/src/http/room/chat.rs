use axum::{
    extract::{Path, State},
    Json,
};

use super::execute::execute_room_actor_endpoint;
use super::types::{ChatMessagePath, ChatReactionPath};
use crate::http::validation::ProtoQuery;
use crate::http::{middleware::RequestMetadata, AppResult, AppState};
use crate::impls::{EndpointRateLimitCategory, EndpointRateLimitScope};
use synctv_proto::client::{
    ChatMessageEventResponse, ChatReadStateResponse, DeleteChatMessageRequest,
    EditChatMessageRequest, GetChatHistoryRequest, GetChatHistoryResponse,
    GetChatMessageContextRequest, GetChatMessageContextResponse, GetChatMessageRequest,
    GetChatMessageResponse, GetChatPlaybackMessagesRequest, GetChatPlaybackMessagesResponse,
    GetChatReadStateRequest, ListChatReactionUsersRequest, ListChatReactionUsersResponse,
    MarkChatReadRequest, SendChatMessageRequest, SetChatReactionRequest, SetChatReactionResponse,
};

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/history",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            GetChatHistoryRequest
        ),
        responses(
            (status = 200, description = "Chat history", body = GetChatHistoryResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_history(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetChatHistoryRequest>,
) -> AppResult<Json<GetChatHistoryResponse>> {
    let room_id = path.room_id;
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomChat,
            move |client_api, actor| async move {
                client_api.get_chat_history_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("include_deleted" = Option<bool>, Query, description = "Include soft-deleted message metadata when allowed")
        ),
        responses(
            (status = 200, description = "Chat message", body = GetChatMessageResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Insufficient room permission", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    ProtoQuery(mut req): ProtoQuery<GetChatMessageRequest>,
) -> AppResult<Json<GetChatMessageResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Read,
            EndpointRateLimitScope::RoomChat,
            move |client_api, actor| async move {
                client_api.get_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/context",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Anchor chat message ID"),
            ("before_limit" = Option<i32>, Query, description = "Messages before anchor"),
            ("after_limit" = Option<i32>, Query, description = "Messages after anchor"),
            ("include_deleted" = Option<bool>, Query, description = "Include soft-deleted messages when allowed")
        ),
        responses(
            (status = 200, description = "Chat message context", body = GetChatMessageContextResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Insufficient room permission", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_message_context(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    ProtoQuery(mut req): ProtoQuery<GetChatMessageContextRequest>,
) -> AppResult<Json<GetChatMessageContextResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .get_chat_message_context_for_actor(&actor, req)
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
        path = "/api/rooms/{room_id}/chat/playback-messages",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("playback_media_id" = Option<String>, Query, description = "Playback media ID"),
            ("playback_playlist_id" = Option<String>, Query, description = "Playback playlist ID"),
            ("playback_target" = Option<Vec<u8>>, Query, description = "Playback target bytes"),
            ("position_seconds" = Option<f64>, Query, description = "Playback position in seconds"),
            ("before_seconds" = Option<f64>, Query, description = "Seconds before position"),
            ("after_seconds" = Option<f64>, Query, description = "Seconds after position"),
            ("limit" = Option<i32>, Query, description = "Maximum messages to return"),
            ("include_deleted" = Option<bool>, Query, description = "Include deleted messages")
        ),
        responses(
            (status = 200, description = "Chat messages around playback position", body = GetChatPlaybackMessagesResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Insufficient room permission", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_playback_messages(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetChatPlaybackMessagesRequest>,
) -> AppResult<Json<GetChatPlaybackMessagesResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .get_chat_playback_messages_for_actor(&actor, req)
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
        path = "/api/rooms/{room_id}/chat/messages",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = SendChatMessageRequest,
        responses(
            (status = 200, description = "Chat message event", body = ChatMessageEventResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Chat disabled or insufficient permission", body = synctv_proto::client::ApiErrorResponse),
            (status = 429, description = "Rate limited", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn send_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<SendChatMessageRequest>,
) -> AppResult<Json<ChatMessageEventResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.send_chat_message_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        patch,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID")
        ),
        request_body = EditChatMessageRequest,
        responses(
            (status = 200, description = "Chat message edited event", body = ChatMessageEventResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Only the sender can edit this message", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Optimistic lock conflict", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn edit_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    Json(mut req): Json<EditChatMessageRequest>,
) -> AppResult<Json<ChatMessageEventResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.edit_chat_message_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID")
        ),
        request_body = DeleteChatMessageRequest,
        responses(
            (status = 200, description = "Chat message deleted event", body = ChatMessageEventResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Sender or DELETE_CHAT permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Optimistic lock conflict", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn delete_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    Json(mut req): Json<DeleteChatMessageRequest>,
) -> AppResult<Json<ChatMessageEventResponse>> {
    let room_id = path.room_id;
    req.message_id = path.message_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.delete_chat_message_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("reaction_key" = String, Path, description = "Reaction key, for example like or an emoji")
        ),
        responses(
            (status = 200, description = "Chat reaction changed event", body = SetChatReactionResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn set_chat_reaction(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatReactionPath>,
) -> AppResult<Json<SetChatReactionResponse>> {
    let req = SetChatReactionRequest {
        message_id: path.message_id,
        reaction_key: path.reaction_key,
        enabled: true,
    };
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.set_chat_reaction_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("reaction_key" = String, Path, description = "Reaction key, for example like or an emoji")
        ),
        responses(
            (status = 200, description = "Chat reaction changed event", body = SetChatReactionResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn clear_chat_reaction(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatReactionPath>,
) -> AppResult<Json<SetChatReactionResponse>> {
    let req = SetChatReactionRequest {
        message_id: path.message_id,
        reaction_key: path.reaction_key,
        enabled: false,
    };
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.set_chat_reaction_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/messages/{message_id}/reactions/{reaction_key}/users",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID"),
            ("message_id" = String, Path, description = "Chat message ID"),
            ("reaction_key" = String, Path, description = "Reaction key, for example like or an emoji"),
            ListChatReactionUsersRequest
        ),
        responses(
            (status = 200, description = "Users who reacted to the chat message", body = ListChatReactionUsersResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_chat_reaction_users(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatReactionPath>,
    ProtoQuery(mut req): ProtoQuery<ListChatReactionUsersRequest>,
) -> AppResult<Json<ListChatReactionUsersResponse>> {
    req.message_id = path.message_id;
    req.reaction_key = path.reaction_key;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .list_chat_reaction_users_for_actor(&actor, req)
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
        path = "/api/rooms/{room_id}/chat/read-state",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        request_body = MarkChatReadRequest,
        responses(
            (status = 200, description = "Chat read state", body = ChatReadStateResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn mark_chat_read(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    Json(req): Json<MarkChatReadRequest>,
) -> AppResult<Json<ChatReadStateResponse>> {
    let room_id = path.room_id;
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            room_id,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomChat,
            move |client_api, actor| async move {
                client_api.mark_chat_read_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{room_id}/chat/read-state",
        tag = "Room",
        params(
            ("room_id" = String, Path, description = "Room ID")
        ),
        responses(
            (status = 200, description = "Chat read state", body = ChatReadStateResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_read_state(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<GetChatReadStateRequest>,
) -> AppResult<Json<ChatReadStateResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.get_chat_read_state_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}
