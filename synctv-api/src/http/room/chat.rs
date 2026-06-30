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
    ChatMessageEventResponse, ChatPinEventResponse, ChatReadStateResponse,
    DeleteChatMessageRequest, EditChatMessageRequest, GetChatHistoryRequest,
    GetChatHistoryResponse, GetChatMessageContextRequest, GetChatMessageContextResponse,
    GetChatMessageReadReceiptsRequest, GetChatMessageReadReceiptsResponse, GetChatMessageRequest,
    GetChatMessageResponse, GetChatPlaybackMessagesRequest, GetChatPlaybackMessagesResponse,
    GetChatReadStateRequest, ListChatReactionUsersRequest, ListChatReactionUsersResponse,
    ListPinnedChatMessagesRequest, ListPinnedChatMessagesResponse, MarkChatReadRequest,
    PinChatMessageRequest, SearchChatMessagesRequest, SearchChatMessagesResponse,
    SendChatMessageRequest, SetChatReactionRequest, SetChatReactionResponse,
    UnpinChatMessageRequest,
};

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct GetChatMessageQuery {
    #[serde(default)]
    include_deleted: bool,
}

impl GetChatMessageQuery {
    fn into_request(self, message_id: String) -> GetChatMessageRequest {
        GetChatMessageRequest {
            message_id,
            include_deleted: self.include_deleted,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct GetChatMessageContextQuery {
    #[serde(default)]
    before_limit: i32,
    #[serde(default)]
    after_limit: i32,
    #[serde(default)]
    include_deleted: bool,
}

impl GetChatMessageContextQuery {
    fn into_request(self, message_id: String) -> GetChatMessageContextRequest {
        GetChatMessageContextRequest {
            message_id,
            before_limit: self.before_limit,
            after_limit: self.after_limit,
            include_deleted: self.include_deleted,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct GetChatPlaybackMessagesQuery {
    #[serde(default)]
    playback_media_id: String,
    #[serde(default)]
    playback_playlist_id: String,
    #[serde(default)]
    playback_target: String,
    #[serde(default)]
    position_seconds: f64,
    #[serde(default)]
    before_seconds: f64,
    #[serde(default)]
    after_seconds: f64,
    #[serde(default)]
    limit: i32,
    #[serde(default)]
    include_deleted: bool,
}

impl GetChatPlaybackMessagesQuery {
    pub(super) fn into_request(
        self,
    ) -> Result<GetChatPlaybackMessagesRequest, crate::http::AppError> {
        let playback_target = if self.playback_target.trim().is_empty() {
            None
        } else {
            Some(
                serde_json::from_str(&self.playback_target).map_err(|error| {
                    crate::http::AppError::bad_request(format!(
                        "Invalid playbackTarget JSON: {error}"
                    ))
                })?,
            )
        };
        Ok(GetChatPlaybackMessagesRequest {
            playback_media_id: self.playback_media_id,
            playback_playlist_id: self.playback_playlist_id,
            playback_target,
            position_seconds: self.position_seconds,
            before_seconds: self.before_seconds,
            after_seconds: self.after_seconds,
            limit: self.limit,
            include_deleted: self.include_deleted,
        })
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct UnpinChatMessageQuery {
    #[serde(default)]
    client_operation_id: String,
}

impl UnpinChatMessageQuery {
    fn into_request(self, message_id: String) -> UnpinChatMessageRequest {
        UnpinChatMessageRequest {
            message_id,
            client_operation_id: self.client_operation_id,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct ListChatReactionUsersQuery {
    #[serde(default)]
    limit: i32,
    #[serde(default)]
    cursor: String,
}

impl ListChatReactionUsersQuery {
    fn into_request(
        self,
        message_id: String,
        reaction_key: String,
    ) -> ListChatReactionUsersRequest {
        ListChatReactionUsersRequest {
            message_id,
            reaction_key,
            limit: self.limit,
            cursor: self.cursor,
        }
    }
}

#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[cfg_attr(feature = "openapi", derive(utoipa::IntoParams))]
#[cfg_attr(feature = "openapi", into_params(parameter_in = Query))]
pub struct GetChatMessageReadReceiptsQuery {
    #[serde(default)]
    page: i32,
    #[serde(default)]
    page_size: i32,
}

impl GetChatMessageReadReceiptsQuery {
    fn into_request(self, message_id: String) -> GetChatMessageReadReceiptsRequest {
        GetChatMessageReadReceiptsRequest {
            message_id,
            page: self.page,
            page_size: self.page_size,
        }
    }
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/chat/history",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
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
        path = "/api/rooms/{roomId}/chat/search",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            SearchChatMessagesRequest
        ),
        responses(
            (status = 200, description = "Chat search results", body = SearchChatMessagesResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "Insufficient room permission", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn search_chat_messages(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<SearchChatMessagesRequest>,
) -> AppResult<Json<SearchChatMessagesResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.search_chat_messages_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/chat/messages/{messageId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID"),
            GetChatMessageQuery
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
    ProtoQuery(query): ProtoQuery<GetChatMessageQuery>,
) -> AppResult<Json<GetChatMessageResponse>> {
    let room_id = path.room_id;
    let req = query.into_request(path.message_id);
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
        path = "/api/rooms/{roomId}/chat/messages/{messageId}/context",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Anchor chat message ID"),
            GetChatMessageContextQuery
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
    ProtoQuery(query): ProtoQuery<GetChatMessageContextQuery>,
) -> AppResult<Json<GetChatMessageContextResponse>> {
    let room_id = path.room_id;
    let req = query.into_request(path.message_id);
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
        path = "/api/rooms/{roomId}/chat/playback-messages",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("playbackMediaId" = Option<String>, Query, description = "Playback media ID"),
            ("playbackPlaylistId" = Option<String>, Query, description = "Playback playlist ID"),
            ("playbackTarget" = Option<String>, Query, description = "Provider playback target as ProtoJSON object"),
            ("positionSeconds" = Option<f64>, Query, description = "Playback position in seconds"),
            ("beforeSeconds" = Option<f64>, Query, description = "Seconds before position"),
            ("afterSeconds" = Option<f64>, Query, description = "Seconds after position"),
            ("limit" = Option<i32>, Query, description = "Maximum messages to return"),
            ("includeDeleted" = Option<bool>, Query, description = "Include deleted messages")
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
    ProtoQuery(query): ProtoQuery<GetChatPlaybackMessagesQuery>,
) -> AppResult<Json<GetChatPlaybackMessagesResponse>> {
    let room_id = path.room_id;
    let req = query.into_request()?;
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
        path = "/api/rooms/{roomId}/chat/messages",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
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
        path = "/api/rooms/{roomId}/chat/messages/{messageId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID")
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
        path = "/api/rooms/{roomId}/chat/messages/{messageId}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID")
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
        get,
        path = "/api/rooms/{roomId}/chat/pinned-messages",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("limit" = Option<i32>, Query, description = "Maximum pinned messages to return")
        ),
        responses(
            (status = 200, description = "Pinned chat messages", body = ListPinnedChatMessagesResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission required", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn list_pinned_chat_messages(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<synctv_proto::client::RoomPathRequest>,
    ProtoQuery(req): ProtoQuery<ListPinnedChatMessagesRequest>,
) -> AppResult<Json<ListPinnedChatMessagesResponse>> {
    let room_id = path.room_id;
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .list_pinned_chat_messages_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/rooms/{roomId}/chat/messages/{messageId}/pin",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID")
        ),
        request_body = PinChatMessageRequest,
        responses(
            (status = 200, description = "Chat message pinned event", body = ChatPinEventResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "DELETE_CHAT permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse),
            (status = 409, description = "Message state conflict", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn pin_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    Json(mut req): Json<PinChatMessageRequest>,
) -> AppResult<Json<ChatPinEventResponse>> {
    req.message_id = path.message_id;
    let response =
        execute_room_actor_endpoint(
            &state,
            request_meta,
            path.room_id,
            EndpointRateLimitCategory::Write,
            EndpointRateLimitScope::RoomChat,
            move |client_api, actor| async move {
                client_api.pin_chat_message_for_actor(&actor, req).await
            },
        )
        .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        delete,
        path = "/api/rooms/{roomId}/chat/messages/{messageId}/pin",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID"),
            UnpinChatMessageQuery
        ),
        responses(
            (status = 200, description = "Chat message unpinned event", body = ChatPinEventResponse),
            (status = 400, description = "Invalid request", body = synctv_proto::client::ApiErrorResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "DELETE_CHAT permission required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message or pin not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn unpin_chat_message(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    ProtoQuery(query): ProtoQuery<UnpinChatMessageQuery>,
) -> AppResult<Json<ChatPinEventResponse>> {
    let req = query.into_request(path.message_id);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        path.room_id,
        EndpointRateLimitCategory::Write,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api.unpin_chat_message_for_actor(&actor, req).await
        },
    )
    .await?;

    Ok(Json(response))
}

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        put,
        path = "/api/rooms/{roomId}/chat/messages/{messageId}/reactions/{reactionKey}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID"),
            ("reactionKey" = String, Path, description = "Reaction key, for example like or an emoji")
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
        path = "/api/rooms/{roomId}/chat/messages/{messageId}/reactions/{reactionKey}",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID"),
            ("reactionKey" = String, Path, description = "Reaction key, for example like or an emoji")
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
        path = "/api/rooms/{roomId}/chat/messages/{messageId}/reactions/{reactionKey}/users",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID"),
            ("reactionKey" = String, Path, description = "Reaction key, for example like or an emoji"),
            ListChatReactionUsersQuery
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
    ProtoQuery(query): ProtoQuery<ListChatReactionUsersQuery>,
) -> AppResult<Json<ListChatReactionUsersResponse>> {
    let req = query.into_request(path.message_id, path.reaction_key);
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
        path = "/api/rooms/{roomId}/chat/read-state",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
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
        path = "/api/rooms/{roomId}/chat/read-state",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID")
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

#[cfg_attr(
    feature = "openapi",
    utoipa::path(
        get,
        path = "/api/rooms/{roomId}/chat/messages/{messageId}/read-receipts",
        tag = "Room",
        params(
            ("roomId" = String, Path, description = "Room ID"),
            ("messageId" = String, Path, description = "Chat message ID"),
            GetChatMessageReadReceiptsQuery
        ),
        responses(
            (status = 200, description = "Chat message read receipts", body = GetChatMessageReadReceiptsResponse),
            (status = 401, description = "Authentication required", body = synctv_proto::client::ApiErrorResponse),
            (status = 403, description = "VIEW_CHAT_HISTORY permission and message ownership required", body = synctv_proto::client::ApiErrorResponse),
            (status = 404, description = "Message not found", body = synctv_proto::client::ApiErrorResponse)
        ),
        security(
            ("bearer_auth" = [])
        )
    )
)]
pub async fn get_chat_message_read_receipts(
    request_meta: RequestMetadata,
    State(state): State<AppState>,
    Path(path): Path<ChatMessagePath>,
    ProtoQuery(query): ProtoQuery<GetChatMessageReadReceiptsQuery>,
) -> AppResult<Json<GetChatMessageReadReceiptsResponse>> {
    let room_id = path.room_id;
    let req = query.into_request(path.message_id);
    let response = execute_room_actor_endpoint(
        &state,
        request_meta,
        room_id,
        EndpointRateLimitCategory::Read,
        EndpointRateLimitScope::RoomChat,
        move |client_api, actor| async move {
            client_api
                .get_chat_message_read_receipts_for_actor(&actor, req)
                .await
        },
    )
    .await?;

    Ok(Json(response))
}
