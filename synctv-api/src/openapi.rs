use axum::Router;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::http::providers::{
    alist, bilibili, common, emby,
    playback_provider::{
        alist as playback_provider_alist, bilibili as playback_provider_bilibili,
        direct_url as playback_provider_direct_url, emby as playback_provider_emby,
        live_proxy as playback_provider_live_proxy, rtmp as playback_provider_rtmp,
    },
    rtmp,
};
use crate::http::{
    admin, auth, email, health, notifications, oauth2, public, room, room_extra, ticket, user,
    webrtc, websocket, AppState,
};
use synctv_proto::client;

#[derive(utoipa::ToSchema)]
#[schema(rename_all = "camelCase")]
#[allow(dead_code)]
pub struct GoogleRpcStatusSchema {
    pub code: i32,
    pub message: String,
    pub details: Vec<serde_json::Value>,
}

#[derive(OpenApi)]
#[openapi(
    paths(
        health::liveness_check,
        health::readiness_check,
        public::get_public_settings,
        auth::confirm_email_login,
        auth::create_guest_token,
        auth::register_with_direct_password,
        auth::login_with_direct_password,
        auth::request_email_registration,
        auth::confirm_email_registration,
        auth::start_opaque_registration,
        auth::finish_opaque_registration,
        auth::start_opaque_login,
        auth::finish_opaque_login,
        auth::start_passkey_registration,
        auth::finish_passkey_registration,
        auth::start_passkey_login,
        auth::finish_passkey_login,
        auth::request_email_login,
        auth::request_mfa_email_code,
        auth::verify_mfa_email_code,
        auth::start_mfa_passkey,
        auth::finish_mfa_passkey,
        auth::refresh_token,
        auth::logout,
        ticket::create_ticket,
        webrtc::get_ice_servers,
        user::get_me,
        user::update_user,
        user::get_user_preferences,
        user::update_user_preferences,
        user::start_sensitive_operation_verification,
        user::start_sensitive_operation_passkey,
        user::request_sensitive_operation_email_code,
        user::finish_sensitive_operation_verification,
        user::start_opaque_password_update,
        user::finish_opaque_password_update,
        user::start_passkey_bind,
        user::finish_passkey_bind,
        user::list_passkeys,
        user::delete_passkey,
        user::list_my_rooms,
        user::close_account,
        email::request_password_reset,
        email::start_opaque_password_reset,
        email::finish_opaque_password_reset,
        notifications::list_notifications,
        notifications::get_notification,
        notifications::mark_as_read,
        notifications::mark_all_as_read,
        notifications::delete_notification,
        notifications::delete_all_read,
        oauth2::get_authorize_url,
        oauth2::exchange_authorization_code,
        oauth2::get_bind_authorize_url,
        oauth2::unlink_provider,
        oauth2::list_available_providers,
        oauth2::get_linked_providers,
        common::list_instances,
        common::list_provider_instances,
        common::add_provider_instance,
        common::update_provider_instance,
        common::delete_provider_instance,
        common::reconnect_provider_instance,
        common::enable_provider_instance,
        common::disable_provider_instance,
        common::list_backends,
        alist::login,
        alist::list,
        alist::search,
        alist::me,
        alist::logout,
        alist::binds,
        emby::login,
        emby::list,
        emby::me,
        emby::logout,
        emby::binds,
        bilibili::parse,
        bilibili::login_qr,
        bilibili::qr_check,
        bilibili::sms_start,
        bilibili::sms_send,
        bilibili::sms_login,
        bilibili::user_info,
        bilibili::binds,
        bilibili::logout,
        emby::thumbnail,
        rtmp::generate_publish_key,
        rtmp::handle_stream_info,
        playback_provider_direct_url::get_direct_url_stream,
        playback_provider_direct_url::head_direct_url_stream,
        playback_provider_direct_url::get_direct_url_hls_manifest,
        playback_provider_direct_url::get_direct_url_hls_segment,
        playback_provider_direct_url::head_direct_url_hls_segment,
        playback_provider_direct_url::get_direct_url_subtitle,
        playback_provider_alist::get_alist_file_stream,
        playback_provider_alist::head_alist_file_stream,
        playback_provider_alist::get_alist_transcoded_hls_manifest,
        playback_provider_alist::get_alist_transcoded_hls_segment,
        playback_provider_alist::head_alist_transcoded_hls_segment,
        playback_provider_alist::get_alist_subtitle,
        playback_provider_alist::get_alist_thumbnail,
        playback_provider_emby::get_emby_media_stream,
        playback_provider_emby::head_emby_media_stream,
        playback_provider_emby::get_emby_hls_manifest,
        playback_provider_emby::get_emby_hls_segment,
        playback_provider_emby::head_emby_hls_segment,
        playback_provider_emby::get_emby_subtitle,
        playback_provider_bilibili::get_bilibili_media_stream,
        playback_provider_bilibili::head_bilibili_media_stream,
        playback_provider_bilibili::get_bilibili_hls_manifest,
        playback_provider_bilibili::get_bilibili_hls_segment,
        playback_provider_bilibili::head_bilibili_hls_segment,
        playback_provider_bilibili::get_bilibili_dash_manifest,
        playback_provider_bilibili::get_bilibili_dash_segment,
        playback_provider_bilibili::head_bilibili_dash_segment,
        playback_provider_bilibili::get_bilibili_subtitle,
        playback_provider_bilibili::get_bilibili_danmaku_file,
        playback_provider_bilibili::watch_bilibili_live_danmaku,
        playback_provider_rtmp::get_rtmp_flv_stream,
        playback_provider_rtmp::head_rtmp_flv_stream,
        playback_provider_rtmp::get_rtmp_hls_playlist,
        playback_provider_rtmp::get_rtmp_hls_segment,
        playback_provider_rtmp::head_rtmp_hls_segment,
        playback_provider_live_proxy::get_live_proxy_flv_stream,
        playback_provider_live_proxy::head_live_proxy_flv_stream,
        playback_provider_live_proxy::get_live_proxy_hls_playlist,
        playback_provider_live_proxy::get_live_proxy_hls_segment,
        playback_provider_live_proxy::head_live_proxy_hls_segment,
        websocket::websocket_room_connect_doc,
        room::create_room,
        room::list_or_get_rooms,
        room::get_hot_rooms,
        room::check_room,
        room::get_room,
        room::join_room,
        room::leave_room,
        room::get_room_members,
        room::list_room_streams,
        room::get_room_stream_info,
        room::kick_room_stream,
        room::get_playback,
        room::get_room_settings,
        room::update_room_settings,
        room::transfer_room_ownership,
        room::list_playlists,
        room::create_playlist,
        room::get_playlist,
        room::get_media,
        room::add_media,
        room::clear_playlist,
        room::delete_media,
        room::edit_media,
        room::delete_entries,
        room::push_media_batch,
        room::move_media,
        room::start_playback,
        room::stop_playback,
        room::update_playback_state,
        room::start_room_password_login,
        room::finish_room_password_login,
        room::start_room_password_registration,
        room::finish_room_password_registration,
        room::clear_room_password,
        room::watch_chat_events,
        room::watch_chat_pin_events,
        room::get_chat_history,
        room::search_chat_messages,
        room::get_chat_message,
        room::get_chat_message_context,
        room::get_chat_message_read_receipts,
        room::send_chat_message,
        room::create_chat_attachment_upload_session,
        room::edit_chat_message,
        room::delete_chat_message,
        room::list_pinned_chat_messages,
        room::pin_chat_message,
        room::unpin_chat_message,
        room::set_chat_reaction,
        room::clear_chat_reaction,
        room::list_chat_reaction_users,
        room::mark_chat_read,
        room::get_chat_read_state,
        room::report_content,
        room::list_room_content_reports,
        room::get_room_content_report,
        room::update_room_content_report_status,
        room::list_playlist_items,
        room::delete_playlist,
        room::update_playlist,
        room::move_playlist,
        room::reset_room_settings,
        room_extra::kick_member,
        room_extra::set_member_permissions,
        room_extra::list_room_join_reviews,
        room_extra::approve_room_join_review,
        room_extra::reject_room_join_review,
        admin::get_system_stats,
        admin::list_user_registration_reviews,
        admin::approve_user_registration_review,
        admin::reject_user_registration_review,
        admin::list_room_creation_reviews,
        admin::approve_room_creation_review,
        admin::reject_room_creation_review,
        admin::list_room_join_reviews,
        admin::approve_room_join_review,
        admin::reject_room_join_review,
        admin::list_ban_records,
        admin::list_content_reports,
        admin::get_content_report,
        admin::update_content_report_status,
        admin::get_settings,
        admin::set_settings,
        admin::send_test_email,
        admin::list_users,
        admin::create_user,
        admin::get_user,
        admin::delete_user,
        admin::get_user_preferences,
        admin::update_user_preferences,
        admin::set_user_role,
        admin::set_user_password,
        admin::set_user_username,
        admin::ban_user,
        admin::unban_user,
        admin::get_user_rooms,
        admin::batch_ban_users,
        admin::batch_delete_users,
        admin::list_rooms,
        admin::list_room_categories,
        admin::upsert_room_category,
        admin::delete_room_category,
        admin::list_room_labels,
        admin::upsert_room_label,
        admin::delete_room_label,
        admin::get_room,
        admin::update_room_taxonomy,
        admin::delete_room,
        admin::set_room_password,
        admin::get_room_members,
        admin::add_member,
        admin::update_member_permissions,
        admin::kick_member,
        admin::ban_room,
        admin::unban_room,
        admin::get_room_settings,
        admin::set_room_settings,
        admin::reset_room_settings,
        admin::batch_ban_rooms,
        admin::batch_delete_rooms,
        admin::list_streams,
        admin::kick_stream,
        admin::list_admins,
        admin::add_admin,
        admin::remove_admin
    ),
    components(
        schemas(
            GoogleRpcStatusSchema,
            client::HealthResponse,
            client::HealthDetails,
            client::MemoryHealth,
            client::CreateWebSocketTicketRequest,
            client::CreateWebSocketTicketResponse,
            client::SetUsernameRequest,
            client::SetUsernameResponse,
            client::RegisterResponse,
            client::RegisterWithDirectPasswordRequest,
            client::LoginWithDirectPasswordRequest,
            client::ConfirmEmailLoginRequest,
            client::LoginResponse,
            client::CreateGuestTokenRequest,
            client::CreateGuestTokenResponse,
            client::StartOpaqueRegistrationRequest,
            client::StartOpaqueRegistrationResponse,
            client::FinishOpaqueRegistrationRequest,
            client::StartOpaqueLoginRequest,
            client::StartOpaqueLoginResponse,
            client::FinishOpaqueLoginRequest,
            client::StartPasskeyRegistrationRequest,
            client::StartPasskeyRegistrationResponse,
            client::FinishPasskeyRegistrationRequest,
            client::StartPasskeyLoginRequest,
            client::StartPasskeyLoginResponse,
            client::FinishPasskeyLoginRequest,
            client::RequestEmailLoginRequest,
            client::RequestEmailLoginResponse,
            client::RequestEmailRegistrationRequest,
            client::RequestEmailRegistrationResponse,
            client::ConfirmEmailRegistrationRequest,
            client::RequestMfaEmailCodeRequest,
            client::RequestMfaEmailCodeResponse,
            client::VerifyMfaEmailCodeRequest,
            client::StartMfaPasskeyRequest,
            client::StartMfaPasskeyResponse,
            client::FinishMfaPasskeyRequest,
            client::RefreshTokenRequest,
            client::RefreshTokenResponse,
            client::LogoutResponse,
            synctv_proto::providers::rtmp::CreatePublishKeyResponse,
            client::RequestPasswordResetRequest,
            client::RequestPasswordResetResponse,
            client::StartOpaquePasswordResetRequest,
            client::StartOpaquePasswordResetResponse,
            client::FinishOpaquePasswordResetRequest,
            client::ConfirmPasswordResetResponse,
            client::GetPublicSettingsResponse,
            client::GetServerInfoResponse,
            client::GetProfileResponse,
            client::SensitiveOperationVerificationChallenge,
            client::StartSensitiveOperationVerificationRequest,
            client::StartSensitiveOperationVerificationResponse,
            client::StartSensitiveOperationPasskeyRequest,
            client::StartSensitiveOperationPasskeyResponse,
            client::RequestSensitiveOperationEmailCodeRequest,
            client::RequestSensitiveOperationEmailCodeResponse,
            client::FinishSensitiveOperationVerificationRequest,
            client::FinishSensitiveOperationVerificationResponse,
            client::StartOpaquePasswordUpdateRequest,
            client::StartOpaquePasswordUpdateResponse,
            client::FinishOpaquePasswordUpdateRequest,
            client::FinishOpaquePasswordUpdateResponse,
            client::StartPasskeyBindRequest,
            client::StartPasskeyBindResponse,
            client::FinishPasskeyBindRequest,
            client::PasskeyCredentialResponse,
            client::ListPasskeysResponse,
            client::DeletePasskeyRequest,
            client::DeletePasskeyResponse,
            client::CloseAccountRequest,
            client::CloseAccountResponse,
            client::GetUserPreferencesResponse,
            client::UpdateUserPreferencesRequest,
            client::UpdateUserPreferencesResponse,
            client::GetIceServersResponse,
            client::ListMyRoomsResponse,
            client::DeleteRoomResponse,
            client::CreateRoomRequest,
            client::CreateRoomResponse,
            client::ListRoomsResponse,
            client::RoomCategory,
            client::RoomLabel,
            client::ListRoomCategoriesResponse,
            client::ListRoomLabelsResponse,
            client::GetHotRoomsResponse,
            client::CheckRoomResponse,
            client::GetRoomResponse,
            client::JoinRoomRequest,
            client::JoinRoomResponse,
            client::LeaveRoomResponse,
            client::GetRoomMembersResponse,
            client::ListRoomStreamsResponse,
            client::StreamEntry,
            client::GetPlaybackResponse,
            client::GetRoomSettingsResponse,
            client::UpdateRoomSettingsRequest,
            client::Room,
            client::ListPlaylistsResponse,
            client::CreatePlaylistRequest,
            client::CreatePlaylistResponse,
            client::GetPlaylistResponse,
            client::Playlist,
            client::Media,
            client::AddMediaRequest,
            client::AddMediaResponse,
            client::ClearPlaylistResponse,
            client::DeleteMediaResponse,
            client::EditMediaRequest,
            client::EditMediaResponse,
            client::DeleteEntriesRequest,
            client::DeleteEntriesResponse,
            client::AddMediaBatchRequest,
            client::AddMediaBatchResponse,
            client::MoveMediaRequest,
            client::MoveMediaResponse,
            client::StartPlaybackRequest,
            client::StartPlaybackResponse,
            client::StopPlaybackRequest,
            client::StopPlaybackResponse,
            client::UpdatePlaybackStateRequest,
            client::StartRoomPasswordLoginRequest,
            client::StartRoomPasswordLoginResponse,
            client::FinishRoomPasswordLoginRequest,
            client::StartRoomPasswordRegistrationRequest,
            client::StartRoomPasswordRegistrationResponse,
            client::FinishRoomPasswordRegistrationRequest,
            client::ClearRoomPasswordRequest,
            client::SetRoomPasswordResponse,
            client::GetChatHistoryResponse,
            client::ChatReactionSummary,
            client::ChatReactionUser,
            client::ListChatReactionUsersRequest,
            client::ListChatReactionUsersResponse,
            client::SetChatReactionRequest,
            client::SetChatReactionResponse,
            client::ReportContentRequest,
            client::ReportContentResponse,
            client::ReportRoomTarget,
            client::ReportUserTarget,
            client::ReportRoomMemberTarget,
            client::ReportChatMessageTarget,
            client::ListPlaylistItemsRequest,
            client::ListPlaylistItemsResponse,
            client::DeletePlaylistResponse,
            client::UpdatePlaylistRequest,
            client::UpdatePlaylistResponse,
            client::MovePlaylistRequest,
            client::MovePlaylistResponse,
            client::ResetRoomSettingsResponse,
            client::KickMemberResponse,
            client::UpdateMemberPermissionsRequest,
            client::RoomJoinReview,
            client::ListRoomJoinReviewsRequest,
            client::ListRoomJoinReviewsResponse,
            client::ApproveRoomJoinReviewRequest,
            client::ApproveRoomJoinReviewResponse,
            client::RejectRoomJoinReviewRequest,
            client::RejectRoomJoinReviewResponse,
            client::NotificationProto,
            client::ListNotificationsResponse,
            client::GetNotificationResponse,
            client::MarkAsReadRequest,
            client::MarkAllAsReadRequest,
            client::GetAuthorizationUrlResponse,
            client::GetAuthorizationUrlForBindResponse,
            client::ExchangeAuthorizationCodeRequest,
            client::ExchangeAuthorizationCodeResponse,
            client::ListAvailableProvidersResponse,
            client::OAuth2ProviderInstance,
            client::UnlinkProviderResponse,
            client::GetLinkedProvidersResponse,
            client::LinkedProvider,
            synctv_proto::providers::common::ProviderInstancesResponse,
            synctv_proto::providers::common::ProviderBackendsResponse,
            synctv_proto::providers::alist::LoginRequest,
            synctv_proto::providers::alist::LoginResponse,
            synctv_proto::providers::alist::GetBindsResponse,
            synctv_proto::providers::alist::ListRequest,
            synctv_proto::providers::alist::ListResponse,
            synctv_proto::providers::alist::FileItem,
            synctv_proto::providers::alist::SearchRequest,
            synctv_proto::providers::alist::SearchResponse,
            synctv_proto::providers::alist::SearchItem,
            synctv_proto::providers::alist::GetMeRequest,
            synctv_proto::providers::alist::GetMeResponse,
            synctv_proto::providers::alist::LogoutRequest,
            synctv_proto::providers::alist::LogoutResponse,
            synctv_proto::providers::emby::LoginRequest,
            synctv_proto::providers::emby::LoginResponse,
            synctv_proto::providers::emby::GetBindsResponse,
            synctv_proto::providers::emby::ListRequest,
            synctv_proto::providers::emby::ListResponse,
            synctv_proto::providers::emby::MediaItem,
            synctv_proto::providers::emby::GetMeRequest,
            synctv_proto::providers::emby::GetMeResponse,
            synctv_proto::providers::emby::LogoutRequest,
            synctv_proto::providers::emby::LogoutResponse,
            synctv_proto::providers::bilibili::ParseRequest,
            synctv_proto::providers::bilibili::ParseResponse,
            synctv_proto::providers::bilibili::VideoInfo,
            synctv_proto::providers::bilibili::QrCodeResponse,
            synctv_proto::providers::bilibili::CheckQrRequest,
            synctv_proto::providers::bilibili::QrStatusResponse,
            synctv_proto::providers::bilibili::StartSmsLoginRequest,
            synctv_proto::providers::bilibili::StartSmsLoginResponse,
            synctv_proto::providers::bilibili::SendSmsRequest,
            synctv_proto::providers::bilibili::SendSmsResponse,
            synctv_proto::providers::bilibili::LoginSmsRequest,
            synctv_proto::providers::bilibili::LoginSmsResponse,
            synctv_proto::providers::bilibili::UserInfoRequest,
            synctv_proto::providers::bilibili::UserInfoResponse,
            synctv_proto::providers::bilibili::GetBindsResponse,
            synctv_proto::providers::bilibili::LogoutRequest,
            synctv_proto::providers::bilibili::LogoutResponse,
            synctv_proto::providers::bilibili::QrLoginStatus,
            synctv_proto::providers::rtmp::GetStreamInfoResponse,
            synctv_proto::providers::rtmp::StreamPublisherInfo,
            synctv_proto::client::GetRoomStreamInfoResponse,
            synctv_proto::client::RoomStreamPublisherInfo,
            synctv_proto::client::KickRoomStreamRequest,
            synctv_proto::client::KickRoomStreamResponse,
            synctv_proto::admin::GetSystemStatsResponse,
            synctv_proto::admin::RuntimeSettings,
            synctv_proto::admin::UpdateSettingsRequest,
            synctv_proto::admin::SendTestEmailRequest,
            synctv_proto::admin::SendTestEmailResponse,
            synctv_proto::admin::UserRegistrationReview,
            synctv_proto::admin::ListUserRegistrationReviewsRequest,
            synctv_proto::admin::ListUserRegistrationReviewsResponse,
            synctv_proto::admin::ApproveUserRegistrationReviewRequest,
            synctv_proto::admin::ApproveUserRegistrationReviewResponse,
            synctv_proto::admin::RejectUserRegistrationReviewRequest,
            synctv_proto::admin::RejectUserRegistrationReviewResponse,
            synctv_proto::admin::RoomCreationReview,
            synctv_proto::admin::ListRoomCreationReviewsRequest,
            synctv_proto::admin::ListRoomCreationReviewsResponse,
            synctv_proto::admin::ApproveRoomCreationReviewRequest,
            synctv_proto::admin::ApproveRoomCreationReviewResponse,
            synctv_proto::admin::RejectRoomCreationReviewRequest,
            synctv_proto::admin::RejectRoomCreationReviewResponse,
            synctv_proto::admin::RoomJoinReview,
            synctv_proto::admin::ListRoomJoinReviewsRequest,
            synctv_proto::admin::ListRoomJoinReviewsResponse,
            synctv_proto::admin::ApproveRoomJoinReviewRequest,
            synctv_proto::admin::ApproveRoomJoinReviewResponse,
            synctv_proto::admin::RejectRoomJoinReviewRequest,
            synctv_proto::admin::RejectRoomJoinReviewResponse,
            synctv_proto::admin::BanRecord,
            synctv_proto::admin::ListBanRecordsRequest,
            synctv_proto::admin::ListBanRecordsResponse,
            synctv_proto::admin::ContentReport,
            synctv_proto::admin::ListContentReportsRequest,
            synctv_proto::admin::ListContentReportsResponse,
            synctv_proto::admin::GetContentReportResponse,
            synctv_proto::admin::UpdateContentReportStatusRequest,
            synctv_proto::admin::UpdateContentReportStatusResponse,
            synctv_proto::admin::ListUsersResponse,
            synctv_proto::admin::GetUserResponse,
            synctv_proto::admin::GetUserPreferencesResponse,
            synctv_proto::admin::UpdateUserPreferencesRequest,
            synctv_proto::admin::UpdateUserPreferencesResponse,
            synctv_proto::admin::CreateUserRequest,
            synctv_proto::admin::CreateUserResponse,
            synctv_proto::admin::DeleteUserResponse,
            synctv_proto::admin::UpdateUserRoleRequest,
            synctv_proto::admin::UpdateUserRoleResponse,
            synctv_proto::admin::SetUserPasswordRequest,
            synctv_proto::admin::SetUserPasswordResponse,
            synctv_proto::admin::UpdateUserUsernameRequest,
            synctv_proto::admin::UpdateUserUsernameResponse,
            synctv_proto::admin::BanUserRequest,
            synctv_proto::admin::BanUserResponse,
            synctv_proto::admin::UnbanUserResponse,
            synctv_proto::admin::GetUserRoomsResponse,
            synctv_proto::admin::BatchBanUsersRequest,
            synctv_proto::admin::BatchBanUsersResponse,
            synctv_proto::admin::BatchDeleteUsersRequest,
            synctv_proto::admin::BatchDeleteUsersResponse,
            synctv_proto::admin::ListRoomsResponse,
            synctv_proto::admin::ListRoomCategoriesRequest,
            synctv_proto::admin::ListRoomCategoriesResponse,
            synctv_proto::admin::UpsertRoomCategoryRequest,
            synctv_proto::admin::UpsertRoomCategoryResponse,
            synctv_proto::admin::DeleteRoomCategoryResponse,
            synctv_proto::admin::ListRoomLabelsRequest,
            synctv_proto::admin::ListRoomLabelsResponse,
            synctv_proto::admin::UpsertRoomLabelRequest,
            synctv_proto::admin::UpsertRoomLabelResponse,
            synctv_proto::admin::DeleteRoomLabelResponse,
            synctv_proto::admin::UpdateRoomTaxonomyRequest,
            synctv_proto::admin::UpdateRoomTaxonomyResponse,
            synctv_proto::admin::GetRoomResponse,
            synctv_proto::admin::DeleteRoomResponse,
            synctv_proto::admin::UpdateRoomPasswordRequest,
            synctv_proto::admin::UpdateRoomPasswordResponse,
            synctv_proto::admin::GetRoomMembersResponse,
            synctv_proto::admin::BanRoomRequest,
            synctv_proto::admin::BanRoomResponse,
            synctv_proto::admin::UnbanRoomResponse,
            synctv_proto::admin::GetRoomSettingsResponse,
            synctv_proto::admin::UpdateRoomSettingsRequest,
            synctv_proto::admin::ResetRoomSettingsResponse,
            synctv_proto::admin::BatchBanRoomsRequest,
            synctv_proto::admin::BatchBanRoomsResponse,
            synctv_proto::admin::BatchDeleteRoomsRequest,
            synctv_proto::admin::BatchDeleteRoomsResponse,
            synctv_proto::providers::common::ListProviderInstancesRequest,
            synctv_proto::providers::common::ListProviderInstancesResponse,
            synctv_proto::providers::common::AddProviderInstanceRequest,
            synctv_proto::providers::common::AddProviderInstanceResponse,
            synctv_proto::providers::common::UpdateProviderInstanceRequest,
            synctv_proto::providers::common::UpdateProviderInstanceResponse,
            synctv_proto::providers::common::DeleteProviderInstanceResponse,
            synctv_proto::providers::common::ReconnectProviderInstanceResponse,
            synctv_proto::providers::common::EnableProviderInstanceResponse,
            synctv_proto::providers::common::DisableProviderInstanceResponse,
            synctv_proto::admin::ListActiveStreamsResponse,
            synctv_proto::admin::KickStreamRequest,
            synctv_proto::admin::KickStreamResponse,
            synctv_proto::admin::ListAdminsResponse,
            synctv_proto::admin::AddAdminResponse,
            synctv_proto::admin::RemoveAdminResponse,
            synctv_proto::admin::AdminUser,
            synctv_proto::admin::Room,
            synctv_proto::providers::common::ProviderInstance
        )
    ),
    modifiers(&SecurityAddon),
    tags(
        (name = "Health", description = "Health and readiness endpoints"),
        (name = "Public", description = "Unauthenticated public endpoints"),
        (name = "Auth", description = "Authentication and session lifecycle"),
        (name = "WebSocket", description = "WebSocket handshake and ticket bootstrap endpoints"),
        (name = "WebRTC", description = "WebRTC transport bootstrap endpoints"),
        (name = "Streaming", description = "Publish-key and live-stream bootstrap endpoints"),
        (name = "Email", description = "Email bind, login, and password reset endpoints"),
        (name = "Notification", description = "Authenticated user notification endpoints"),
        (name = "OAuth2", description = "OAuth2 login and account-link endpoints"),
        (name = "Provider", description = "Provider discovery and backend selection endpoints"),
        (name = "DirectUrl Playback Provider", description = "DirectUrl playback transport endpoints"),
        (name = "Alist Playback Provider", description = "Alist playback transport endpoints"),
        (name = "Emby Playback Provider", description = "Emby playback transport endpoints"),
        (name = "Bilibili Playback Provider", description = "Bilibili playback and danmaku transport endpoints"),
        (name = "RTMP Playback Provider", description = "RTMP live playback transport endpoints"),
        (name = "LiveProxy Playback Provider", description = "LiveProxy playback transport endpoints"),
        (name = "User", description = "Current-user profile and ownership endpoints"),
        (name = "Room", description = "Core room lifecycle, membership, media, playback and playlist endpoints"),
        (name = "Room Member", description = "Room-scoped member moderation and permission endpoints"),
        (name = "Admin", description = "Administrative and moderation HTTP endpoints")
    )
)]
pub struct ApiDoc;

struct SecurityAddon;

impl utoipa::Modify for SecurityAddon {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityScheme};

        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "bearer_auth",
            SecurityScheme::Http(
                HttpBuilder::new()
                    .scheme(HttpAuthScheme::Bearer)
                    .bearer_format("JWT")
                    .build(),
            ),
        );
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
}

#[cfg(test)]
mod tests {
    use serde_json::Value;
    use utoipa::OpenApi;

    type TestResult<T = ()> = anyhow::Result<T>;

    fn test_error(message: impl Into<String>) -> anyhow::Error {
        anyhow::anyhow!(message.into())
    }

    fn openapi_json() -> TestResult<Value> {
        Ok(serde_json::to_value(super::ApiDoc::openapi())?)
    }

    #[test]
    fn openapi_keeps_bearer_auth_off_public_routes() -> TestResult {
        let doc = openapi_json()?;

        assert!(
            doc.get("security").is_none(),
            "document-level security should not force bearer auth onto public routes"
        );

        for (path, method) in [
            ("/health/live", "get"),
            ("/health/ready", "get"),
            ("/api/public/settings", "get"),
            ("/api/auth/email/confirm", "post"),
            ("/api/auth/opaque/registration/start", "post"),
            ("/api/auth/opaque/registration/finish", "post"),
            ("/api/rooms", "get"),
        ] {
            assert!(
                doc["paths"][path][method].get("security").is_none(),
                "{method} {path} should be documented as public"
            );
        }

        let notifications_security = doc["paths"]["/api/notifications"]["get"]["security"]
            .as_array()
            .ok_or_else(|| test_error("authenticated endpoints should declare security"))?;
        assert!(
            !notifications_security.is_empty(),
            "authenticated endpoints should keep bearer auth requirements"
        );
        Ok(())
    }

    #[test]
    fn openapi_matches_public_room_error_contracts() -> TestResult {
        let doc = openapi_json()?;

        assert!(
            doc["paths"]["/api/rooms/hot"]["get"]["responses"]["400"].is_object(),
            "hot rooms should document validation errors"
        );
        assert!(
            doc["paths"]["/api/rooms/{roomId}/check"]["get"]["responses"]["400"].is_object(),
            "check room should document invalid room IDs"
        );
        assert!(
            doc["paths"]["/api/rooms/{roomId}/check"]["get"]["responses"]
                .get("404")
                .is_none(),
            "check room returns 200 with exists=false for missing rooms"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_oauth2_unconfigured_as_service_unavailable() -> TestResult {
        let doc = openapi_json()?;
        let responses = &doc["paths"]["/api/oauth2/providers"]["get"]["responses"];

        assert!(
            responses["503"].is_object(),
            "OAuth2 provider listing should document the unconfigured case as 503"
        );
        assert!(
            responses.get("400").is_none(),
            "OAuth2 provider listing should not document missing service as a bad request"
        );
        Ok(())
    }

    #[test]
    fn openapi_marks_notifications_read_all_body_optional() -> TestResult {
        let doc = openapi_json()?;

        let request_body = &doc["paths"]["/api/notifications/read-all"]["post"]["requestBody"];
        assert!(
            request_body.is_object(),
            "mark-all-as-read should still document its request body schema"
        );

        let required = request_body["required"].as_bool();

        assert_ne!(
            required,
            Some(true),
            "mark-all-as-read should not document its request body as required"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_admin_user_preferences_routes() -> TestResult {
        let doc = openapi_json()?;

        let path = &doc["paths"]["/api/admin/users/{userId}/preferences"];
        assert!(
            path["get"].is_object(),
            "admin get-user-preferences route should be documented"
        );
        assert!(
            path["patch"].is_object(),
            "admin update-user-preferences route should be documented"
        );

        let request_body = &path["patch"]["requestBody"];
        assert!(
            request_body.is_object(),
            "admin update-user-preferences should document its request body"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_chat_routes() -> TestResult {
        let doc = openapi_json()?;

        for (path, method, responses) in [
            (
                "/api/rooms/{roomId}/chat/messages",
                "post",
                &["200", "400", "401", "403", "429"][..],
            ),
            (
                "/api/rooms/{roomId}/chat/attachments/upload-session",
                "post",
                &["200", "400", "401", "403", "429"][..],
            ),
            (
                "/api/rooms/{roomId}/chat/messages/{messageId}",
                "get",
                &["200", "400", "401", "403", "404"][..],
            ),
            (
                "/api/rooms/{roomId}/chat/messages/{messageId}",
                "patch",
                &["200", "400", "401", "403", "404", "409"][..],
            ),
            (
                "/api/rooms/{roomId}/chat/messages/{messageId}",
                "delete",
                &["200", "400", "401", "403", "404", "409"][..],
            ),
            (
                "/api/rooms/{roomId}/chat/messages/{messageId}/context",
                "get",
                &["200", "400", "401", "403", "404"][..],
            ),
            (
                "/api/rooms/{roomId}/chat/read-state",
                "post",
                &["200", "400", "401", "403", "404"][..],
            ),
            (
                "/api/rooms/{roomId}/chat/read-state",
                "get",
                &["200", "401", "403"][..],
            ),
            (
                "/api/rooms/{roomId}/watch/chat-events",
                "get",
                &["200", "400", "401", "403", "503"][..],
            ),
        ] {
            assert_response_codes(&doc, path, method, responses)?;
        }

        assert_parameter_location(
            &doc,
            "/api/rooms/{roomId}/chat/messages/{messageId}",
            "get",
            "includeDeleted",
            "query",
        )?;
        assert_parameter_location(
            &doc,
            "/api/rooms/{roomId}/watch/chat-events",
            "get",
            "afterEventSequence",
            "query",
        )?;
        Ok(())
    }

    fn assert_response_codes(
        doc: &Value,
        path: &str,
        method: &str,
        expected: &[&str],
    ) -> TestResult {
        let responses = doc["paths"][path][method]["responses"]
            .as_object()
            .ok_or_else(|| test_error(format!("{method} {path} should document responses")))?;

        for code in expected {
            assert!(
                responses.get(*code).is_some_and(Value::is_object),
                "{method} {path} should document HTTP {code}; responses were {:?}",
                responses.keys().collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    fn assert_parameter_location(
        doc: &Value,
        path: &str,
        method: &str,
        name: &str,
        expected_location: &str,
    ) -> TestResult {
        let params = doc["paths"][path][method]["parameters"]
            .as_array()
            .ok_or_else(|| test_error(format!("{method} {path} should document parameters")))?;
        let locations = params
            .iter()
            .filter(|param| param["name"] == name)
            .map(|param| param["in"].as_str().unwrap_or("<missing>"))
            .collect::<Vec<_>>();

        assert_eq!(
            locations,
            vec![expected_location],
            "{method} {path} should document {name} only as {expected_location}; got {locations:?}"
        );
        Ok(())
    }

    fn assert_parameter_absent(doc: &Value, path: &str, method: &str, name: &str) -> TestResult {
        let params = doc["paths"][path][method]["parameters"]
            .as_array()
            .map_or(&[][..], Vec::as_slice);
        let locations = params
            .iter()
            .filter(|param| param["name"] == name)
            .map(|param| param["in"].as_str().unwrap_or("<missing>"))
            .collect::<Vec<_>>();

        assert!(
            locations.is_empty(),
            "{method} {path} should document {name} in the request body; got parameter locations {locations:?}"
        );
        Ok(())
    }

    fn assert_request_body_schema_ref(
        doc: &Value,
        path: &str,
        method: &str,
        expected_schema: &str,
    ) -> TestResult {
        let schema_ref = doc["paths"][path][method]["requestBody"]["content"]["application/json"]
            ["schema"]["$ref"]
            .as_str()
            .ok_or_else(|| {
                test_error(format!(
                    "{method} {path} should document a JSON request body schema ref"
                ))
            })?;

        assert!(
            schema_ref.ends_with(expected_schema),
            "{method} {path} should use {expected_schema} request body schema; got {schema_ref}"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_query_struct_params_as_query() -> TestResult {
        let doc = openapi_json()?;

        for name in [
            "page",
            "pageSize",
            "search",
            "role",
            "sortBy",
            "sortDirection",
        ] {
            assert_parameter_location(&doc, "/api/rooms/{roomId}/members", "get", name, "query")?;
        }
        assert_parameter_location(&doc, "/api/rooms/{roomId}/members", "get", "roomId", "path")?;

        for name in [
            "page",
            "pageSize",
            "status",
            "search",
            "isBanned",
            "sortBy",
            "sortDirection",
        ] {
            assert_parameter_location(
                &doc,
                "/api/admin/users/{userId}/rooms",
                "get",
                name,
                "query",
            )?;
        }
        assert_parameter_location(
            &doc,
            "/api/admin/users/{userId}/rooms",
            "get",
            "userId",
            "path",
        )?;

        for name in [
            "page",
            "pageSize",
            "search",
            "role",
            "sortBy",
            "sortDirection",
        ] {
            assert_parameter_location(
                &doc,
                "/api/admin/rooms/{roomId}/members",
                "get",
                name,
                "query",
            )?;
        }
        assert_parameter_location(
            &doc,
            "/api/admin/rooms/{roomId}/members",
            "get",
            "roomId",
            "path",
        )?;

        assert_parameter_absent(&doc, "/api/providers/alist/list", "post", "instanceName")?;
        assert_request_body_schema_ref(
            &doc,
            "/api/providers/alist/list",
            "post",
            "synctv_provider_alist_ListRequest",
        )?;
        assert_parameter_location(
            &doc,
            "/api/rooms/{roomId}/playback",
            "get",
            "streamPreference",
            "query",
        )?;
        Ok(())
    }

    #[test]
    fn openapi_documents_provider_endpoint_error_contracts() -> TestResult {
        let doc = openapi_json()?;

        let provider_operation_errors = ["400", "401", "403", "404", "408", "409", "429", "503"];
        for (path, method) in [
            ("/api/providers/alist/login", "post"),
            ("/api/providers/alist/list", "post"),
            ("/api/providers/bilibili/parse", "post"),
            ("/api/providers/bilibili/login/qr/generate", "post"),
            ("/api/providers/emby/login", "post"),
            ("/api/providers/emby/list", "post"),
        ] {
            assert_response_codes(&doc, path, method, &provider_operation_errors)?;
        }

        let provider_binds_errors = ["400", "401", "403", "408", "429", "503"];
        for path in [
            "/api/providers/alist/binds",
            "/api/providers/bilibili/binds",
            "/api/providers/emby/binds",
        ] {
            assert_response_codes(&doc, path, "get", &provider_binds_errors)?;
        }

        let admin_provider_errors = ["400", "401", "403", "408", "429", "503"];
        for (path, method) in [
            ("/api/providers/instances", "get"),
            ("/api/providers/instances", "post"),
        ] {
            assert_response_codes(&doc, path, method, &admin_provider_errors)?;
        }

        let admin_provider_mutation_errors =
            ["400", "401", "403", "404", "408", "409", "429", "503"];
        for (path, method) in [
            ("/api/providers/instances/{name}", "put"),
            ("/api/providers/instances/{name}", "delete"),
            ("/api/providers/instances/{name}/reconnect", "post"),
            ("/api/providers/instances/{name}/enable", "post"),
            ("/api/providers/instances/{name}/disable", "post"),
        ] {
            assert_response_codes(&doc, path, method, &admin_provider_mutation_errors)?;
        }

        assert_response_codes(
            &doc,
            "/api/providers/emby/thumbnail/{itemId}",
            "get",
            &["400", "401", "403", "404", "408", "429", "503"],
        )?;
        Ok(())
    }

    #[test]
    fn openapi_documents_playlist_source_fields() -> TestResult {
        let doc = openapi_json()?;
        let playlist = doc
            .pointer("/components/schemas/synctv_client_Playlist/properties")
            .ok_or_else(|| test_error("Playlist schema properties should exist"))?;

        for field in ["sourceConfig", "sourceProvider", "providerInstanceName"] {
            assert!(
                playlist.get(field).is_some(),
                "Playlist schema should document {field}: {playlist:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn openapi_documents_websocket_handshake_endpoint() -> TestResult {
        let doc = openapi_json()?;
        let operation = &doc["paths"]["/ws/rooms/{roomId}"]["get"];

        assert!(
            operation.is_object(),
            "WebSocket route should be present in OpenAPI"
        );
        assert_eq!(operation["operationId"], "connectRoomWebSocket");

        let params = operation["parameters"]
            .as_array()
            .ok_or_else(|| test_error("WebSocket route should describe handshake parameters"))?;
        assert!(
            params.iter().any(|param| {
                param["name"] == "roomId" && param["in"] == "path" && param["required"] == true
            }),
            "room_id path parameter should be documented"
        );
        assert!(
            params
                .iter()
                .any(|param| param["name"] == "ticket" && param["in"] == "query"),
            "ticket query parameter should be documented"
        );
        assert!(
            params
                .iter()
                .any(|param| param["name"] == "Authorization" && param["in"] == "header"),
            "Authorization header should be documented"
        );
        assert!(
            params
                .iter()
                .any(|param| param["name"] == "Origin" && param["in"] == "header"),
            "Origin header should be documented"
        );

        assert!(
            operation["responses"]["101"].is_object(),
            "successful WebSocket upgrade should be documented"
        );
        assert!(
            operation["responses"]["401"].is_object(),
            "authentication failures should be documented"
        );
        assert!(
            operation["responses"]["503"].is_object(),
            "runtime dependency failures should be documented"
        );
        Ok(())
    }
}
