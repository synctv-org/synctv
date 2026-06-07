use tonic::{Request, Response, Status};

use super::ClientServiceImpl;
use synctv_proto::client::room_service_server::RoomService;
use synctv_proto::client::*;

mod chat;
mod lifecycle;
mod media;
mod members;
mod playback;
mod playlists;
mod settings;
mod streaming;

#[tonic::async_trait]
// Tonic generated service traits require `Result<Response<_>, tonic::Status>`.
#[allow(clippy::result_large_err)]
impl RoomService for ClientServiceImpl {
    async fn update_room_settings(
        &self,
        request: Request<UpdateRoomSettingsRequest>,
    ) -> Result<Response<UpdateRoomSettingsResponse>, Status> {
        settings::update_room_settings(self, request).await
    }

    async fn get_room_members(
        &self,
        request: Request<GetRoomMembersRequest>,
    ) -> Result<Response<GetRoomMembersResponse>, Status> {
        members::get_room_members(self, request).await
    }

    async fn list_room_streams(
        &self,
        request: Request<ListRoomStreamsRequest>,
    ) -> Result<Response<ListRoomStreamsResponse>, Status> {
        members::list_room_streams(self, request).await
    }

    async fn get_room_stream_info(
        &self,
        request: Request<GetRoomStreamInfoRequest>,
    ) -> Result<Response<GetRoomStreamInfoResponse>, Status> {
        members::get_room_stream_info(self, request).await
    }

    async fn kick_room_stream(
        &self,
        request: Request<KickRoomStreamRequest>,
    ) -> Result<Response<KickRoomStreamResponse>, Status> {
        members::kick_room_stream(self, request).await
    }

    async fn add_member(
        &self,
        request: Request<AddMemberRequest>,
    ) -> Result<Response<AddMemberResponse>, Status> {
        members::add_member(self, request).await
    }

    async fn list_room_join_reviews(
        &self,
        request: Request<ListRoomJoinReviewsRequest>,
    ) -> Result<Response<ListRoomJoinReviewsResponse>, Status> {
        members::list_room_join_reviews(self, request).await
    }

    async fn approve_room_join_review(
        &self,
        request: Request<ApproveRoomJoinReviewRequest>,
    ) -> Result<Response<ApproveRoomJoinReviewResponse>, Status> {
        members::approve_room_join_review(self, request).await
    }

    async fn reject_room_join_review(
        &self,
        request: Request<RejectRoomJoinReviewRequest>,
    ) -> Result<Response<RejectRoomJoinReviewResponse>, Status> {
        members::reject_room_join_review(self, request).await
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<UpdateMemberPermissionsResponse>, Status> {
        members::update_member_permissions(self, request).await
    }

    async fn kick_member(
        &self,
        request: Request<KickMemberRequest>,
    ) -> Result<Response<KickMemberResponse>, Status> {
        members::kick_member(self, request).await
    }

    async fn get_room_settings(
        &self,
        request: Request<GetRoomSettingsRequest>,
    ) -> Result<Response<GetRoomSettingsResponse>, Status> {
        settings::get_room_settings(self, request).await
    }

    async fn reset_room_settings(
        &self,
        request: Request<ResetRoomSettingsRequest>,
    ) -> Result<Response<ResetRoomSettingsResponse>, Status> {
        settings::reset_room_settings(self, request).await
    }

    async fn transfer_room_ownership(
        &self,
        request: Request<TransferRoomOwnershipRequest>,
    ) -> Result<Response<TransferRoomOwnershipResponse>, Status> {
        settings::transfer_room_ownership(self, request).await
    }

    async fn start_room_password_registration(
        &self,
        request: Request<StartRoomPasswordRegistrationRequest>,
    ) -> Result<Response<StartRoomPasswordRegistrationResponse>, Status> {
        settings::start_room_password_registration(self, request).await
    }

    async fn finish_room_password_registration(
        &self,
        request: Request<FinishRoomPasswordRegistrationRequest>,
    ) -> Result<Response<SetRoomPasswordResponse>, Status> {
        settings::finish_room_password_registration(self, request).await
    }

    async fn clear_room_password(
        &self,
        request: Request<ClearRoomPasswordRequest>,
    ) -> Result<Response<SetRoomPasswordResponse>, Status> {
        settings::clear_room_password(self, request).await
    }

    async fn leave_room(
        &self,
        request: Request<LeaveRoomRequest>,
    ) -> Result<Response<LeaveRoomResponse>, Status> {
        lifecycle::leave_room(self, request).await
    }

    async fn delete_room(
        &self,
        request: Request<DeleteRoomRequest>,
    ) -> Result<Response<DeleteRoomResponse>, Status> {
        lifecycle::delete_room(self, request).await
    }

    type MessageStreamStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<ServerMessage, Status>> + Send + 'static>,
    >;
    type WatchPlaybackStateStream = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<WatchPlaybackStateEvent, Status>>
                + Send
                + 'static,
        >,
    >;
    type WatchPlaybackStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<WatchPlaybackEvent, Status>> + Send + 'static>,
    >;
    type WatchRoomSettingsStream = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<WatchRoomSettingsEvent, Status>>
                + Send
                + 'static,
        >,
    >;
    type WatchPlaylistItemsStream = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<WatchPlaylistItemsEvent, Status>>
                + Send
                + 'static,
        >,
    >;
    type WatchRoomMembersStream = std::pin::Pin<
        Box<
            dyn tokio_stream::Stream<Item = Result<WatchRoomMembersEvent, Status>> + Send + 'static,
        >,
    >;
    type WatchChatEventsStream = std::pin::Pin<
        Box<dyn tokio_stream::Stream<Item = Result<WatchChatEventsEvent, Status>> + Send + 'static>,
    >;

    async fn create_web_socket_ticket(
        &self,
        request: Request<CreateWebSocketTicketRequest>,
    ) -> Result<Response<CreateWebSocketTicketResponse>, Status> {
        lifecycle::create_web_socket_ticket(self, request).await
    }

    async fn message_stream(
        &self,
        request: Request<tonic::Streaming<ClientMessage>>,
    ) -> Result<Response<Self::MessageStreamStream>, Status> {
        streaming::message_stream(self, request).await
    }

    async fn watch_playback_state(
        &self,
        request: Request<WatchPlaybackStateRequest>,
    ) -> Result<Response<Self::WatchPlaybackStateStream>, Status> {
        streaming::watch_playback_state(self, request).await
    }

    async fn watch_playback(
        &self,
        request: Request<WatchPlaybackRequest>,
    ) -> Result<Response<Self::WatchPlaybackStream>, Status> {
        streaming::watch_playback(self, request).await
    }

    async fn watch_room_settings(
        &self,
        request: Request<WatchRoomSettingsRequest>,
    ) -> Result<Response<Self::WatchRoomSettingsStream>, Status> {
        streaming::watch_room_settings(self, request).await
    }

    async fn watch_playlist_items(
        &self,
        request: Request<WatchPlaylistItemsRequest>,
    ) -> Result<Response<Self::WatchPlaylistItemsStream>, Status> {
        streaming::watch_playlist_items(self, request).await
    }

    async fn watch_room_members(
        &self,
        request: Request<WatchRoomMembersRequest>,
    ) -> Result<Response<Self::WatchRoomMembersStream>, Status> {
        streaming::watch_room_members(self, request).await
    }

    async fn watch_chat_events(
        &self,
        request: Request<WatchChatEventsRequest>,
    ) -> Result<Response<Self::WatchChatEventsStream>, Status> {
        streaming::watch_chat_events(self, request).await
    }

    async fn create_chat_image_upload_session(
        &self,
        request: Request<CreateChatImageUploadSessionRequest>,
    ) -> Result<Response<CreateChatImageUploadSessionResponse>, Status> {
        chat::create_chat_image_upload_session(self, request).await
    }

    async fn upload_chat_image_object(
        &self,
        request: Request<UploadChatImageObjectRequest>,
    ) -> Result<Response<UploadChatImageObjectResponse>, Status> {
        chat::upload_chat_image_object(self, request).await
    }

    async fn get_chat_image_object(
        &self,
        request: Request<GetChatImageObjectRequest>,
    ) -> Result<Response<ChatImageObjectResponse>, Status> {
        chat::get_chat_image_object(self, request).await
    }

    async fn get_chat_history(
        &self,
        request: Request<GetChatHistoryRequest>,
    ) -> Result<Response<GetChatHistoryResponse>, Status> {
        chat::get_chat_history(self, request).await
    }

    async fn get_chat_message(
        &self,
        request: Request<GetChatMessageRequest>,
    ) -> Result<Response<GetChatMessageResponse>, Status> {
        chat::get_chat_message(self, request).await
    }

    async fn get_chat_message_context(
        &self,
        request: Request<GetChatMessageContextRequest>,
    ) -> Result<Response<GetChatMessageContextResponse>, Status> {
        chat::get_chat_message_context(self, request).await
    }

    async fn get_chat_playback_messages(
        &self,
        request: Request<GetChatPlaybackMessagesRequest>,
    ) -> Result<Response<GetChatPlaybackMessagesResponse>, Status> {
        chat::get_chat_playback_messages(self, request).await
    }

    async fn mark_chat_read(
        &self,
        request: Request<MarkChatReadRequest>,
    ) -> Result<Response<ChatReadStateResponse>, Status> {
        chat::mark_chat_read(self, request).await
    }

    async fn get_chat_read_state(
        &self,
        request: Request<GetChatReadStateRequest>,
    ) -> Result<Response<ChatReadStateResponse>, Status> {
        chat::get_chat_read_state(self, request).await
    }

    async fn send_chat_message(
        &self,
        request: Request<SendChatMessageRequest>,
    ) -> Result<Response<ChatMessageEventResponse>, Status> {
        chat::send_chat_message(self, request).await
    }

    async fn edit_chat_message(
        &self,
        request: Request<EditChatMessageRequest>,
    ) -> Result<Response<ChatMessageEventResponse>, Status> {
        chat::edit_chat_message(self, request).await
    }

    async fn delete_chat_message(
        &self,
        request: Request<DeleteChatMessageRequest>,
    ) -> Result<Response<ChatMessageEventResponse>, Status> {
        chat::delete_chat_message(self, request).await
    }

    async fn set_chat_reaction(
        &self,
        request: Request<SetChatReactionRequest>,
    ) -> Result<Response<SetChatReactionResponse>, Status> {
        chat::set_chat_reaction(self, request).await
    }

    async fn list_chat_reaction_users(
        &self,
        request: Request<ListChatReactionUsersRequest>,
    ) -> Result<Response<ListChatReactionUsersResponse>, Status> {
        chat::list_chat_reaction_users(self, request).await
    }

    async fn get_ice_servers(
        &self,
        request: Request<GetIceServersRequest>,
    ) -> Result<Response<GetIceServersResponse>, Status> {
        media::get_ice_servers(self, request).await
    }

    async fn add_media(
        &self,
        request: Request<AddMediaRequest>,
    ) -> Result<Response<AddMediaResponse>, Status> {
        media::add_media(self, request).await
    }

    async fn get_media(
        &self,
        request: Request<GetMediaRequest>,
    ) -> Result<Response<Media>, Status> {
        media::get_media(self, request).await
    }

    async fn create_video_cover_upload_session(
        &self,
        request: Request<CreateVideoCoverUploadSessionRequest>,
    ) -> Result<Response<CreateVideoCoverUploadSessionResponse>, Status> {
        media::create_video_cover_upload_session(self, request).await
    }

    async fn create_room_cover_upload_session(
        &self,
        request: Request<synctv_proto::client::CreateRoomCoverUploadSessionRequest>,
    ) -> Result<Response<synctv_proto::client::CreateRoomCoverUploadSessionResponse>, Status> {
        media::create_room_cover_upload_session(self, request).await
    }

    async fn upload_room_cover_object(
        &self,
        request: Request<synctv_proto::client::UploadRoomCoverObjectRequest>,
    ) -> Result<Response<synctv_proto::client::UploadRoomCoverObjectResponse>, Status> {
        media::upload_room_cover_object(self, request).await
    }

    async fn get_room_cover_object(
        &self,
        request: Request<synctv_proto::client::GetRoomCoverObjectRequest>,
    ) -> Result<Response<synctv_proto::client::RoomCoverObjectResponse>, Status> {
        media::get_room_cover_object(self, request).await
    }

    async fn update_room_cover(
        &self,
        request: Request<synctv_proto::client::UpdateRoomCoverRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        media::update_room_cover(self, request).await
    }

    async fn clear_room_cover(
        &self,
        request: Request<synctv_proto::client::ClearRoomCoverRequest>,
    ) -> Result<Response<GetRoomResponse>, Status> {
        media::clear_room_cover(self, request).await
    }

    async fn upload_video_cover_object(
        &self,
        request: Request<UploadVideoCoverObjectRequest>,
    ) -> Result<Response<UploadVideoCoverObjectResponse>, Status> {
        media::upload_video_cover_object(self, request).await
    }

    async fn get_video_cover_object(
        &self,
        request: Request<GetVideoCoverObjectRequest>,
    ) -> Result<Response<VideoCoverObjectResponse>, Status> {
        media::get_video_cover_object(self, request).await
    }

    async fn update_video_cover(
        &self,
        request: Request<UpdateVideoCoverRequest>,
    ) -> Result<Response<EditMediaResponse>, Status> {
        media::update_video_cover(self, request).await
    }

    async fn clear_video_cover(
        &self,
        request: Request<synctv_proto::client::ClearVideoCoverRequest>,
    ) -> Result<Response<EditMediaResponse>, Status> {
        media::clear_video_cover(self, request).await
    }

    async fn create_playlist_cover_upload_session(
        &self,
        request: Request<synctv_proto::client::CreatePlaylistCoverUploadSessionRequest>,
    ) -> Result<Response<synctv_proto::client::CreatePlaylistCoverUploadSessionResponse>, Status>
    {
        media::create_playlist_cover_upload_session(self, request).await
    }

    async fn upload_playlist_cover_object(
        &self,
        request: Request<synctv_proto::client::UploadPlaylistCoverObjectRequest>,
    ) -> Result<Response<synctv_proto::client::UploadPlaylistCoverObjectResponse>, Status> {
        media::upload_playlist_cover_object(self, request).await
    }

    async fn get_playlist_cover_object(
        &self,
        request: Request<synctv_proto::client::GetPlaylistCoverObjectRequest>,
    ) -> Result<Response<synctv_proto::client::PlaylistCoverObjectResponse>, Status> {
        media::get_playlist_cover_object(self, request).await
    }

    async fn update_playlist_cover(
        &self,
        request: Request<synctv_proto::client::UpdatePlaylistCoverRequest>,
    ) -> Result<Response<UpdatePlaylistResponse>, Status> {
        media::update_playlist_cover(self, request).await
    }

    async fn clear_playlist_cover(
        &self,
        request: Request<synctv_proto::client::ClearPlaylistCoverRequest>,
    ) -> Result<Response<UpdatePlaylistResponse>, Status> {
        media::clear_playlist_cover(self, request).await
    }

    async fn delete_media(
        &self,
        request: Request<DeleteMediaRequest>,
    ) -> Result<Response<DeleteMediaResponse>, Status> {
        media::delete_media(self, request).await
    }

    async fn delete_entries(
        &self,
        request: Request<DeleteEntriesRequest>,
    ) -> Result<Response<DeleteEntriesResponse>, Status> {
        media::delete_entries(self, request).await
    }

    async fn edit_media(
        &self,
        request: Request<EditMediaRequest>,
    ) -> Result<Response<EditMediaResponse>, Status> {
        media::edit_media(self, request).await
    }

    async fn list_playlist_items(
        &self,
        request: Request<ListPlaylistItemsRequest>,
    ) -> Result<Response<ListPlaylistItemsResponse>, Status> {
        media::list_playlist_items(self, request).await
    }

    async fn move_media(
        &self,
        request: Request<MoveMediaRequest>,
    ) -> Result<Response<MoveMediaResponse>, Status> {
        media::move_media(self, request).await
    }

    async fn clear_playlist(
        &self,
        request: Request<ClearPlaylistRequest>,
    ) -> Result<Response<ClearPlaylistResponse>, Status> {
        media::clear_playlist(self, request).await
    }

    async fn add_media_batch(
        &self,
        request: Request<AddMediaBatchRequest>,
    ) -> Result<Response<AddMediaBatchResponse>, Status> {
        media::add_media_batch(self, request).await
    }

    async fn start_playback(
        &self,
        request: Request<StartPlaybackRequest>,
    ) -> Result<Response<StartPlaybackResponse>, Status> {
        playback::start_playback(self, request).await
    }

    async fn stop_playback(
        &self,
        request: Request<StopPlaybackRequest>,
    ) -> Result<Response<StopPlaybackResponse>, Status> {
        playback::stop_playback(self, request).await
    }

    async fn get_playback(
        &self,
        request: Request<GetPlaybackRequest>,
    ) -> Result<Response<GetPlaybackResponse>, Status> {
        playback::get_playback(self, request).await
    }

    async fn update_playback(
        &self,
        request: Request<UpdatePlaybackRequest>,
    ) -> Result<Response<GetPlaybackResponse>, Status> {
        playback::update_playback(self, request).await
    }

    // Playlist Management
    async fn create_playlist(
        &self,
        request: Request<CreatePlaylistRequest>,
    ) -> Result<Response<CreatePlaylistResponse>, Status> {
        playlists::create_playlist(self, request).await
    }

    async fn get_playlist(
        &self,
        request: Request<GetPlaylistRequest>,
    ) -> Result<Response<GetPlaylistResponse>, Status> {
        playlists::get_playlist(self, request).await
    }

    async fn update_playlist(
        &self,
        request: Request<UpdatePlaylistRequest>,
    ) -> Result<Response<UpdatePlaylistResponse>, Status> {
        playlists::update_playlist(self, request).await
    }

    async fn move_playlist(
        &self,
        request: Request<MovePlaylistRequest>,
    ) -> Result<Response<MovePlaylistResponse>, Status> {
        playlists::move_playlist(self, request).await
    }

    async fn delete_playlist(
        &self,
        request: Request<DeletePlaylistRequest>,
    ) -> Result<Response<DeletePlaylistResponse>, Status> {
        playlists::delete_playlist(self, request).await
    }

    async fn list_playlists(
        &self,
        request: Request<ListPlaylistsRequest>,
    ) -> Result<Response<ListPlaylistsResponse>, Status> {
        playlists::list_playlists(self, request).await
    }
}
