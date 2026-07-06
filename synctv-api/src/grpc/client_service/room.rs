use tonic::{Request, Response, Status};

use super::{map_api_error, ClientServiceImpl};
use crate::impls::EndpointRateLimitCategory;
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
    ) -> Result<Response<Room>, Status> {
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
    ) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
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
    ) -> Result<Response<RoomJoinReview>, Status> {
        members::reject_room_join_review(self, request).await
    }

    async fn update_member_permissions(
        &self,
        request: Request<UpdateMemberPermissionsRequest>,
    ) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
        members::update_member_permissions(self, request).await
    }

    async fn update_member_remark_name(
        &self,
        request: Request<UpdateMemberRemarkNameRequest>,
    ) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
        members::update_member_remark_name(self, request).await
    }

    async fn update_member_display_tag(
        &self,
        request: Request<UpdateMemberDisplayTagRequest>,
    ) -> Result<Response<synctv_proto::common::RoomMember>, Status> {
        members::update_member_display_tag(self, request).await
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
    ) -> Result<Response<RoomSettings>, Status> {
        settings::reset_room_settings(self, request).await
    }

    async fn transfer_room_ownership(
        &self,
        request: Request<TransferRoomOwnershipRequest>,
    ) -> Result<Response<Room>, Status> {
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

    type MessageStreamStream = super::GrpcStatusStream<ServerMessage>;
    type WatchPlaybackStateStream = super::GrpcStatusStream<WatchPlaybackStateEvent>;
    type WatchPlaybackStream = super::GrpcStatusStream<WatchPlaybackEvent>;
    type WatchRoomSettingsStream = super::GrpcStatusStream<WatchRoomSettingsEvent>;
    type WatchPlaylistItemsStream = super::GrpcStatusStream<WatchPlaylistItemsEvent>;
    type WatchRoomMemberEventsStream = super::GrpcStatusStream<WatchRoomMemberEventsEvent>;
    type WatchChatEventsStream = super::GrpcStatusStream<WatchChatEventsEvent>;
    type WatchChatPinEventsStream = super::GrpcStatusStream<WatchChatPinEventsEvent>;
    type GetChatAttachmentObjectStream = super::GrpcStatusStream<ChatAttachmentObjectResponse>;
    type GetRoomCoverObjectStream =
        super::GrpcStatusStream<synctv_proto::client::RoomCoverObjectResponse>;
    type GetMediaCoverObjectStream = super::GrpcStatusStream<MediaCoverObjectResponse>;
    type GetMediaThumbnailObjectStream =
        super::GrpcStatusStream<synctv_proto::client::MediaThumbnailObjectResponse>;
    type GetPlaylistCoverObjectStream =
        super::GrpcStatusStream<synctv_proto::client::PlaylistCoverObjectResponse>;

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

    async fn watch_room_member_events(
        &self,
        request: Request<WatchRoomMemberEventsRequest>,
    ) -> Result<Response<Self::WatchRoomMemberEventsStream>, Status> {
        streaming::watch_room_member_events(self, request).await
    }

    async fn watch_chat_events(
        &self,
        request: Request<WatchChatEventsRequest>,
    ) -> Result<Response<Self::WatchChatEventsStream>, Status> {
        streaming::watch_chat_events(self, request).await
    }

    async fn watch_chat_pin_events(
        &self,
        request: Request<WatchChatPinEventsRequest>,
    ) -> Result<Response<Self::WatchChatPinEventsStream>, Status> {
        streaming::watch_chat_pin_events(self, request).await
    }

    async fn create_chat_attachment_upload_session(
        &self,
        request: Request<CreateChatAttachmentUploadSessionRequest>,
    ) -> Result<Response<CreateChatAttachmentUploadSessionResponse>, Status> {
        chat::create_chat_attachment_upload_session(self, request).await
    }

    async fn upload_chat_attachment_object(
        &self,
        request: Request<UploadChatAttachmentObjectRequest>,
    ) -> Result<Response<UploadChatAttachmentObjectResponse>, Status> {
        chat::upload_chat_attachment_object(self, request).await
    }

    async fn complete_chat_attachment_upload_session(
        &self,
        request: Request<CompleteChatAttachmentUploadSessionRequest>,
    ) -> Result<Response<CompleteChatAttachmentUploadSessionResponse>, Status> {
        chat::complete_chat_attachment_upload_session(self, request).await
    }

    async fn get_chat_attachment_object(
        &self,
        request: Request<GetChatAttachmentObjectRequest>,
    ) -> Result<Response<Self::GetChatAttachmentObjectStream>, Status> {
        chat::get_chat_attachment_object(self, request).await
    }

    async fn get_chat_history(
        &self,
        request: Request<GetChatHistoryRequest>,
    ) -> Result<Response<GetChatHistoryResponse>, Status> {
        chat::get_chat_history(self, request).await
    }

    async fn search_chat_messages(
        &self,
        request: Request<SearchChatMessagesRequest>,
    ) -> Result<Response<SearchChatMessagesResponse>, Status> {
        chat::search_chat_messages(self, request).await
    }

    async fn get_chat_message(
        &self,
        request: Request<GetChatMessageRequest>,
    ) -> Result<Response<ChatMessageReceive>, Status> {
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

    async fn get_chat_message_read_receipts(
        &self,
        request: Request<GetChatMessageReadReceiptsRequest>,
    ) -> Result<Response<GetChatMessageReadReceiptsResponse>, Status> {
        chat::get_chat_message_read_receipts(self, request).await
    }

    async fn report_content(
        &self,
        request: Request<ReportContentRequest>,
    ) -> Result<Response<ReportContentResponse>, Status> {
        chat::report_content(self, request).await
    }

    async fn list_room_content_reports(
        &self,
        request: Request<ListRoomContentReportsRequest>,
    ) -> Result<Response<ListRoomContentReportsResponse>, Status> {
        chat::list_room_content_reports(self, request).await
    }

    async fn get_room_content_report(
        &self,
        request: Request<GetRoomContentReportRequest>,
    ) -> Result<Response<ContentReport>, Status> {
        chat::get_room_content_report(self, request).await
    }

    async fn update_room_content_report_status(
        &self,
        request: Request<UpdateRoomContentReportStatusRequest>,
    ) -> Result<Response<UpdateRoomContentReportStatusResponse>, Status> {
        chat::update_room_content_report_status(self, request).await
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

    async fn list_pinned_chat_messages(
        &self,
        request: Request<ListPinnedChatMessagesRequest>,
    ) -> Result<Response<ListPinnedChatMessagesResponse>, Status> {
        chat::list_pinned_chat_messages(self, request).await
    }

    async fn pin_chat_message(
        &self,
        request: Request<PinChatMessageRequest>,
    ) -> Result<Response<ChatPinEventResponse>, Status> {
        chat::pin_chat_message(self, request).await
    }

    async fn unpin_chat_message(
        &self,
        request: Request<UnpinChatMessageRequest>,
    ) -> Result<Response<ChatPinEventResponse>, Status> {
        chat::unpin_chat_message(self, request).await
    }

    async fn set_chat_reaction(
        &self,
        request: Request<SetChatReactionRequest>,
    ) -> Result<Response<ChatMessageEvent>, Status> {
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
    ) -> Result<Response<Media>, Status> {
        media::add_media(self, request).await
    }

    async fn get_media(
        &self,
        request: Request<GetMediaRequest>,
    ) -> Result<Response<Media>, Status> {
        media::get_media(self, request).await
    }

    async fn create_media_cover_upload_session(
        &self,
        request: Request<CreateMediaCoverUploadSessionRequest>,
    ) -> Result<Response<CreateMediaCoverUploadSessionResponse>, Status> {
        media::create_media_cover_upload_session(self, request).await
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

    async fn complete_room_cover_upload_session(
        &self,
        request: Request<synctv_proto::client::CompleteRoomCoverUploadSessionRequest>,
    ) -> Result<Response<synctv_proto::client::CompleteRoomCoverUploadSessionResponse>, Status>
    {
        media::complete_room_cover_upload_session(self, request).await
    }

    async fn get_room_cover_object(
        &self,
        request: Request<synctv_proto::client::GetRoomCoverObjectRequest>,
    ) -> Result<Response<Self::GetRoomCoverObjectStream>, Status> {
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

    async fn upload_media_cover_object(
        &self,
        request: Request<UploadMediaCoverObjectRequest>,
    ) -> Result<Response<UploadMediaCoverObjectResponse>, Status> {
        media::upload_media_cover_object(self, request).await
    }

    async fn complete_media_cover_upload_session(
        &self,
        request: Request<CompleteMediaCoverUploadSessionRequest>,
    ) -> Result<Response<CompleteMediaCoverUploadSessionResponse>, Status> {
        media::complete_media_cover_upload_session(self, request).await
    }

    async fn get_media_cover_object(
        &self,
        request: Request<GetMediaCoverObjectRequest>,
    ) -> Result<Response<Self::GetMediaCoverObjectStream>, Status> {
        media::get_media_cover_object(self, request).await
    }

    async fn update_media_cover(
        &self,
        request: Request<UpdateMediaCoverRequest>,
    ) -> Result<Response<Media>, Status> {
        media::update_media_cover(self, request).await
    }

    async fn clear_media_cover(
        &self,
        request: Request<synctv_proto::client::ClearMediaCoverRequest>,
    ) -> Result<Response<Media>, Status> {
        media::clear_media_cover(self, request).await
    }

    async fn create_media_thumbnail_upload_session(
        &self,
        request: Request<synctv_proto::client::CreateMediaThumbnailUploadSessionRequest>,
    ) -> Result<Response<synctv_proto::client::CreateMediaThumbnailUploadSessionResponse>, Status>
    {
        media::create_media_thumbnail_upload_session(self, request).await
    }

    async fn upload_media_thumbnail_object(
        &self,
        request: Request<synctv_proto::client::UploadMediaThumbnailObjectRequest>,
    ) -> Result<Response<synctv_proto::client::UploadMediaThumbnailObjectResponse>, Status> {
        media::upload_media_thumbnail_object(self, request).await
    }

    async fn complete_media_thumbnail_upload_session(
        &self,
        request: Request<synctv_proto::client::CompleteMediaThumbnailUploadSessionRequest>,
    ) -> Result<Response<synctv_proto::client::CompleteMediaThumbnailUploadSessionResponse>, Status>
    {
        media::complete_media_thumbnail_upload_session(self, request).await
    }

    async fn get_media_thumbnail_object(
        &self,
        request: Request<synctv_proto::client::GetMediaThumbnailObjectRequest>,
    ) -> Result<Response<Self::GetMediaThumbnailObjectStream>, Status> {
        media::get_media_thumbnail_object(self, request).await
    }

    async fn update_media_thumbnail(
        &self,
        request: Request<synctv_proto::client::UpdateMediaThumbnailRequest>,
    ) -> Result<Response<Media>, Status> {
        media::update_media_thumbnail(self, request).await
    }

    async fn clear_media_thumbnail(
        &self,
        request: Request<synctv_proto::client::ClearMediaThumbnailRequest>,
    ) -> Result<Response<Media>, Status> {
        media::clear_media_thumbnail(self, request).await
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

    async fn complete_playlist_cover_upload_session(
        &self,
        request: Request<synctv_proto::client::CompletePlaylistCoverUploadSessionRequest>,
    ) -> Result<Response<synctv_proto::client::CompletePlaylistCoverUploadSessionResponse>, Status>
    {
        media::complete_playlist_cover_upload_session(self, request).await
    }

    async fn get_playlist_cover_object(
        &self,
        request: Request<synctv_proto::client::GetPlaylistCoverObjectRequest>,
    ) -> Result<Response<Self::GetPlaylistCoverObjectStream>, Status> {
        media::get_playlist_cover_object(self, request).await
    }

    async fn update_playlist_cover(
        &self,
        request: Request<synctv_proto::client::UpdatePlaylistCoverRequest>,
    ) -> Result<Response<Playlist>, Status> {
        media::update_playlist_cover(self, request).await
    }

    async fn clear_playlist_cover(
        &self,
        request: Request<synctv_proto::client::ClearPlaylistCoverRequest>,
    ) -> Result<Response<Playlist>, Status> {
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
    ) -> Result<Response<Media>, Status> {
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

    async fn update_playback_state(
        &self,
        request: Request<UpdatePlaybackStateRequest>,
    ) -> Result<Response<PlaybackState>, Status> {
        playback::update_playback_state(self, request).await
    }

    // Playlist Management
    async fn create_playlist(
        &self,
        request: Request<CreatePlaylistRequest>,
    ) -> Result<Response<Playlist>, Status> {
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
    ) -> Result<Response<Playlist>, Status> {
        playlists::update_playlist(self, request).await
    }

    async fn move_playlist(
        &self,
        request: Request<MovePlaylistRequest>,
    ) -> Result<Response<Playlist>, Status> {
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

    async fn list_room_categories(
        &self,
        request: Request<ListRoomCategoriesRequest>,
    ) -> Result<Response<ListRoomCategoriesResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.list_room_categories(req).await
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }

    async fn list_room_labels(
        &self,
        request: Request<ListRoomLabelsRequest>,
    ) -> Result<Response<ListRoomLabelsResponse>, Status> {
        let metadata = self.request_metadata(&request)?;
        let req = request.into_inner();
        let executor = self.client_api.clone();
        let client_api = self.client_api.clone();
        let response = executor
            .execute_public_endpoint(&metadata, EndpointRateLimitCategory::Read, || async move {
                client_api.list_room_labels(req).await
            })
            .await
            .map_err(map_api_error)?;
        Ok(Response::new(response))
    }
}
