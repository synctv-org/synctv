use axum::Router;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::http::{
    admin, auth, email, notifications, oauth2, public, room, room_extra, ticket, user, webrtc,
    websocket, AppState,
};
use crate::providers::{
    acfun, alist, bilibili, cctv, cloudreve, common, douyin, douyu, emby, fnos, huya, nextcloud,
    playback_provider::{
        acfun as playback_provider_acfun, alist as playback_provider_alist,
        bilibili as playback_provider_bilibili, cctv as playback_provider_cctv,
        direct_url as playback_provider_direct_url, douyin as playback_provider_douyin,
        douyu as playback_provider_douyu, emby as playback_provider_emby,
        fnos as playback_provider_fnos, huya as playback_provider_huya,
        live_proxy as playback_provider_live_proxy, nextcloud as playback_provider_nextcloud,
        qnap as playback_provider_qnap, rtmp as playback_provider_rtmp,
        seafile as playback_provider_seafile, synology as playback_provider_synology,
        tiktok as playback_provider_tiktok, truenas as playback_provider_truenas,
        twitch as playback_provider_twitch, youtube as playback_provider_youtube,
    },
    qnap, rtmp, seafile, synology, tiktok, truenas, twitch, youtube,
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
        public::get_public_settings,
        public::get_server_info,
        public::get_server_time,
        auth::confirm_email_login,
        auth::create_guest_token,
        auth::register_with_direct_password,
        auth::login_with_direct_password,
        auth::start_login,
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
        auth::verify_mfa_totp,
        auth::verify_mfa_recovery_code,
        auth::refresh_token,
        auth::logout,
        ticket::create_ticket,
        webrtc::get_ice_servers,
        user::get_me,
        user::update_user,
        user::get_user_preferences,
        user::update_user_preferences,
        user::set_two_factor_enabled,
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
        user::start_totp_setup,
        user::finish_totp_setup,
        user::regenerate_totp_recovery_codes,
        user::delete_totp,
        user::discover_rooms,
        user::get_room_discovery,
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
        cloudreve::login,
        cloudreve::list,
        cloudreve::search,
        cloudreve::me,
        cloudreve::logout,
        cloudreve::binds,
        twitch::bind,
        twitch::binds,
        twitch::unbind,
        twitch::resolve,
        twitch::list_channel_items,
        twitch::list_followed_live,
        twitch::list_category_streams,
        twitch::list_top_categories,
        twitch::search_live_channels,
        twitch::list_schedule,
        huya::resolve,
        douyu::resolve,
        acfun::resolve,
        cctv::resolve,
        youtube::bind,
        youtube::binds,
        youtube::unbind,
        youtube::resolve,
        douyin::bind,
        douyin::binds,
        douyin::unbind,
        douyin::resolve,
        douyin::list_user_posts,
        tiktok::bind,
        tiktok::binds,
        tiktok::unbind,
        tiktok::resolve,
        tiktok::get_user,
        tiktok::list_user_posts,
        fnos::login,
        fnos::list,
        fnos::media_libraries,
        fnos::media_items,
        fnos::set_favorite,
        fnos::set_watched,
        fnos::server_info,
        fnos::logout,
        fnos::binds,
        fnos::thumbnail,
        qnap::login,
        qnap::list,
        qnap::capabilities,
        qnap::logout,
        qnap::binds,
        qnap::thumbnail,
        synology::login,
        synology::list_files,
        synology::list_libraries,
        synology::list_movies,
        synology::list_tv_shows,
        synology::list_episodes,
        synology::list_home_videos,
        synology::list_tv_recordings,
        synology::logout,
        synology::binds,
        synology::image,
        nextcloud::login,
        nextcloud::start_login_flow,
        nextcloud::poll_login_flow,
        nextcloud::list,
        nextcloud::list_favorites,
        nextcloud::logout,
        nextcloud::binds,
        nextcloud::preview,
        seafile::login,
        seafile::unlock_library,
        seafile::list_repositories,
        seafile::list,
        seafile::list_starred,
        seafile::logout,
        seafile::binds,
        seafile::thumbnail,
        truenas::login,
        truenas::list,
        truenas::logout,
        truenas::binds,
        emby::login,
        emby::list,
        emby::me,
        emby::logout,
        emby::binds,
        bilibili::parse,
        bilibili::list_live_areas,
        bilibili::list_favorite_folders,
        bilibili::list_followed_pgc,
        bilibili::list_history,
        bilibili::list_pgc_timeline,
        bilibili::list_pgc_seasons,
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
        playback_provider_direct_url::get_direct_url_hls_resource,
        playback_provider_direct_url::head_direct_url_hls_resource,
        playback_provider_direct_url::get_direct_url_dash_manifest,
        playback_provider_direct_url::get_direct_url_dash_resource,
        playback_provider_direct_url::head_direct_url_dash_resource,
        playback_provider_direct_url::get_direct_url_subtitle,
        playback_provider_twitch::get_twitch_resource,
        playback_provider_twitch::get_twitch_segment,
        playback_provider_twitch::watch_twitch_chat,
        playback_provider_youtube::get_youtube_resource,
        playback_provider_youtube::get_youtube_segment,
        playback_provider_youtube::get_youtube_subtitle,
        playback_provider_huya::get_huya_resource,
        playback_provider_huya::get_huya_segment,
        playback_provider_huya::watch_huya_danmaku,
        playback_provider_douyu::get_douyu_resource,
        playback_provider_douyu::get_douyu_segment,
        playback_provider_douyu::watch_douyu_danmaku,
        playback_provider_douyin::get_resource,
        playback_provider_douyin::get_segment,
        playback_provider_douyin::watch_danmaku,
        playback_provider_tiktok::get_tiktok_resource,
        playback_provider_tiktok::get_tiktok_segment,
        playback_provider_tiktok::get_tiktok_subtitle,
        playback_provider_acfun::get_acfun_resource,
        playback_provider_acfun::get_acfun_segment,
        playback_provider_acfun::get_acfun_danmaku_file,
        playback_provider_acfun::watch_acfun_danmaku,
        playback_provider_cctv::get_cctv_resource,
        playback_provider_cctv::get_cctv_segment,
        playback_provider_fnos::get_fnos_resource,
        playback_provider_fnos::get_fnos_segment,
        playback_provider_fnos::get_fnos_subtitle,
        playback_provider_fnos::get_fnos_thumbnail,
        playback_provider_qnap::get_qnap_resource,
        playback_provider_qnap::get_qnap_subtitle,
        playback_provider_qnap::get_qnap_thumbnail,
        playback_provider_synology::get_synology_resource,
        playback_provider_synology::get_synology_segment,
        playback_provider_synology::get_synology_subtitle,
        playback_provider_nextcloud::get_nextcloud_resource,
        playback_provider_nextcloud::get_nextcloud_subtitle,
        playback_provider_seafile::get_seafile_resource,
        playback_provider_seafile::get_seafile_subtitle,
        playback_provider_truenas::get_truenas_resource,
        playback_provider_truenas::get_truenas_subtitle,
        playback_provider_alist::get_alist_file_stream,
        playback_provider_alist::head_alist_file_stream,
        playback_provider_alist::get_alist_transcoded_hls_manifest,
        playback_provider_alist::get_alist_transcoded_hls_resource,
        playback_provider_alist::head_alist_transcoded_hls_resource,
        playback_provider_alist::get_alist_subtitle,
        playback_provider_alist::get_alist_thumbnail,
        playback_provider_emby::get_emby_media_stream,
        playback_provider_emby::head_emby_media_stream,
        playback_provider_emby::get_emby_hls_manifest,
        playback_provider_emby::get_emby_hls_resource,
        playback_provider_emby::head_emby_hls_resource,
        playback_provider_emby::get_emby_subtitle,
        playback_provider_bilibili::get_bilibili_media_stream,
        playback_provider_bilibili::head_bilibili_media_stream,
        playback_provider_bilibili::get_bilibili_hls_manifest,
        playback_provider_bilibili::get_bilibili_hls_resource,
        playback_provider_bilibili::head_bilibili_hls_resource,
        playback_provider_bilibili::get_bilibili_dash_manifest,
        playback_provider_bilibili::get_bilibili_dash_resource,
        playback_provider_bilibili::head_bilibili_dash_resource,
        playback_provider_bilibili::get_bilibili_subtitle,
        playback_provider_bilibili::get_bilibili_danmaku_file,
        playback_provider_bilibili::watch_bilibili_live_danmaku,
        playback_provider_rtmp::get_rtmp_flv_stream,
        playback_provider_rtmp::head_rtmp_flv_stream,
        playback_provider_rtmp::get_rtmp_hls_master,
        playback_provider_rtmp::get_rtmp_hls_playlist,
        playback_provider_rtmp::get_rtmp_hls_segment,
        playback_provider_rtmp::head_rtmp_hls_segment,
        playback_provider_live_proxy::get_live_proxy_flv_stream,
        playback_provider_live_proxy::head_live_proxy_flv_stream,
        playback_provider_live_proxy::get_live_proxy_hls_master,
        playback_provider_live_proxy::get_live_proxy_hls_playlist,
        playback_provider_live_proxy::get_live_proxy_hls_segment,
        playback_provider_live_proxy::head_live_proxy_hls_segment,
        websocket::websocket_room_connect_doc,
        room::create_room,
        room::discover_rooms,
        room::get_room_discovery,
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
        room::play_next,
        room::play_previous,
        room::list_playback_history,
        room::play_history_entry,
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
        admin::get_service_state,
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
            client::CctvPlaybackMetadata,
            client::CctvChapterMetadata,
            client::CreateWebSocketTicketRequest,
            client::CreateWebSocketTicketResponse,
            client::SetUsernameRequest,
            client::User,
            client::RegisterResponse,
            client::RegisterWithDirectPasswordRequest,
            client::LoginWithDirectPasswordRequest,
            client::LoginMethod,
            client::StartLoginRequest,
            client::StartLoginResponse,
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
            client::VerifyMfaTotpRequest,
            client::VerifyMfaRecoveryCodeRequest,
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
            client::GetServerTimeRequest,
            client::GetServerTimeResponse,
            client::User,
            client::SensitiveOperationVerificationChallenge,
            client::StartSensitiveOperationVerificationRequest,
            client::SensitiveOperationVerificationOutcome,
            client::StartSensitiveOperationPasskeyRequest,
            client::StartSensitiveOperationPasskeyResponse,
            client::RequestSensitiveOperationEmailCodeRequest,
            client::RequestSensitiveOperationEmailCodeResponse,
            client::FinishSensitiveOperationVerificationRequest,
            client::StartOpaquePasswordUpdateRequest,
            client::StartOpaquePasswordUpdateResponse,
            client::FinishOpaquePasswordUpdateRequest,
            client::User,
            client::StartPasskeyBindRequest,
            client::StartPasskeyBindResponse,
            client::FinishPasskeyBindRequest,
            client::PasskeyCredential,
            client::ListPasskeysResponse,
            client::DeletePasskeyRequest,
            client::DeletePasskeyResponse,
            client::StartTotpSetupRequest,
            client::StartTotpSetupResponse,
            client::FinishTotpSetupRequest,
            client::TotpRecoveryCodesResponse,
            client::RegenerateTotpRecoveryCodesRequest,
            client::DeleteTotpRequest,
            client::DeleteTotpResponse,
            client::CloseAccountRequest,
            client::CloseAccountResponse,
            client::GetUserPreferencesResponse,
            client::UpdateUserPreferencesRequest,
            client::UpdateUserPreferencesResponse,
            client::SetTwoFactorEnabledRequest,
            client::GetIceServersResponse,
            client::ListMyRoomsResponse,
            client::DeleteRoomResponse,
            client::CreateRoomRequest,
            client::Room,
            client::DiscoverRoomsResponse,
            client::RoomDiscoveryItem,
            client::RoomCategory,
            client::RoomLabel,
            client::ListRoomCategoriesResponse,
            client::ListRoomLabelsResponse,
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
            client::Playlist,
            client::GetPlaylistResponse,
            client::Playlist,
            client::Media,
            client::AddMediaRequest,
            client::Media,
            client::ClearPlaylistResponse,
            client::DeleteMediaResponse,
            client::EditMediaRequest,
            client::Media,
            client::DeleteEntriesRequest,
            client::DeleteEntriesResponse,
            client::AddMediaBatchRequest,
            client::AddMediaBatchResponse,
            client::MoveMediaRequest,
            client::MoveMediaResponse,
            client::StartPlaybackRequest,
            client::StopPlaybackRequest,
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
            client::ChatMessageEvent,
            client::ReportContentRequest,
            client::ReportContentResponse,
            client::ReportRoomTarget,
            client::ReportUserTarget,
            client::ReportRoomMemberTarget,
            client::ReportChatMessageTarget,
            client::ListPlaylistItemsRequest,
            client::ListPlaylistItemsResponse,
            client::PagePagination,
            client::CursorPagination,
            client::DeletePlaylistResponse,
            client::UpdatePlaylistRequest,
            client::Playlist,
            client::MovePlaylistRequest,
            client::Playlist,
            client::RoomSettings,
            client::KickMemberResponse,
            client::UpdateMemberPermissionsRequest,
            client::RoomJoinReview,
            client::ListRoomJoinReviewsRequest,
            client::ListRoomJoinReviewsResponse,
            client::ApproveRoomJoinReviewRequest,
            client::ApproveRoomJoinReviewResponse,
            client::RejectRoomJoinReviewRequest,
            client::NotificationProto,
            client::ListNotificationsResponse,
            client::NotificationProto,
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
            synctv_proto::providers::cloudreve::LoginRequest,
            synctv_proto::providers::cloudreve::LoginResponse,
            synctv_proto::providers::cloudreve::ListRequest,
            synctv_proto::providers::cloudreve::ListResponse,
            synctv_proto::providers::cloudreve::PagePagination,
            synctv_proto::providers::cloudreve::CursorPagination,
            synctv_proto::providers::cloudreve::SearchRequest,
            synctv_proto::providers::cloudreve::SearchResponse,
            synctv_proto::providers::cloudreve::FileItem,
            synctv_proto::providers::cloudreve::GetMeRequest,
            synctv_proto::providers::cloudreve::GetMeResponse,
            synctv_proto::providers::cloudreve::LogoutRequest,
            synctv_proto::providers::cloudreve::LogoutResponse,
            synctv_proto::providers::cloudreve::GetBindsResponse,
            synctv_proto::providers::cloudreve::BindInfo,
            synctv_proto::providers::twitch::BindRequest,
            synctv_proto::providers::twitch::BindResponse,
            synctv_proto::providers::twitch::GetBindsResponse,
            synctv_proto::providers::twitch::BindInfo,
            synctv_proto::providers::twitch::UnbindRequest,
            synctv_proto::providers::twitch::UnbindResponse,
            synctv_proto::providers::twitch::ResolveRequest,
            synctv_proto::providers::twitch::ResolveResponse,
            synctv_proto::providers::twitch::Metadata,
            synctv_proto::providers::twitch::Quality,
            synctv_proto::providers::twitch::Chapter,
            synctv_proto::providers::twitch::ListChannelItemsRequest,
            synctv_proto::providers::twitch::ListChannelItemsResponse,
            synctv_proto::providers::twitch::ListItem,
            synctv_proto::providers::twitch::ListFollowedLiveRequest,
            synctv_proto::providers::twitch::ListFollowedLiveResponse,
            synctv_proto::providers::twitch::ListCategoryStreamsRequest,
            synctv_proto::providers::twitch::ListCategoryStreamsResponse,
            synctv_proto::providers::twitch::StreamItem,
            synctv_proto::providers::twitch::ListTopCategoriesRequest,
            synctv_proto::providers::twitch::ListTopCategoriesResponse,
            synctv_proto::providers::twitch::CategoryItem,
            synctv_proto::providers::twitch::SearchLiveChannelsRequest,
            synctv_proto::providers::twitch::SearchLiveChannelsResponse,
            synctv_proto::providers::twitch::SearchChannelItem,
            synctv_proto::providers::twitch::ListScheduleRequest,
            synctv_proto::providers::twitch::ListScheduleResponse,
            synctv_proto::providers::twitch::ScheduleSegment,
            synctv_proto::providers::huya::ResolveRequest,
            synctv_proto::providers::huya::ResolveResponse,
            synctv_proto::providers::huya::Metadata,
            synctv_proto::providers::huya::Quality,
            synctv_proto::providers::douyu::ResolveRequest,
            synctv_proto::providers::douyu::ResolveResponse,
            synctv_proto::providers::douyu::Metadata,
            synctv_proto::providers::douyu::Quality,
            synctv_proto::providers::acfun::ResolveRequest,
            synctv_proto::providers::acfun::ResolveResponse,
            synctv_proto::providers::acfun::Metadata,
            synctv_proto::providers::acfun::Quality,
            synctv_proto::providers::cctv::ResolveRequest,
            synctv_proto::providers::cctv::ResolveResponse,
            synctv_proto::providers::cctv::Metadata,
            synctv_proto::providers::cctv::Chapter,
            synctv_proto::providers::cctv::Stream,
            synctv_proto::providers::youtube::BindRequest,
            synctv_proto::providers::youtube::BindResponse,
            synctv_proto::providers::youtube::GetBindsResponse,
            synctv_proto::providers::youtube::BindInfo,
            synctv_proto::providers::youtube::UnbindRequest,
            synctv_proto::providers::youtube::UnbindResponse,
            synctv_proto::providers::youtube::ResolveRequest,
            synctv_proto::providers::youtube::ResolveResponse,
            synctv_proto::providers::youtube::Metadata,
            synctv_proto::providers::youtube::Format,
            synctv_proto::providers::youtube::Subtitle,
            synctv_proto::providers::douyin::BindRequest,
            synctv_proto::providers::douyin::BindResponse,
            synctv_proto::providers::douyin::GetBindsResponse,
            synctv_proto::providers::douyin::BindInfo,
            synctv_proto::providers::douyin::UnbindRequest,
            synctv_proto::providers::douyin::UnbindResponse,
            synctv_proto::providers::douyin::ResolveRequest,
            synctv_proto::providers::douyin::ResolveResponse,
            synctv_proto::providers::douyin::ListUserPostsRequest,
            synctv_proto::providers::douyin::ListUserPostsResponse,
            synctv_proto::providers::tiktok::BindRequest,
            synctv_proto::providers::tiktok::BindResponse,
            synctv_proto::providers::tiktok::GetBindsResponse,
            synctv_proto::providers::tiktok::BindInfo,
            synctv_proto::providers::tiktok::UnbindRequest,
            synctv_proto::providers::tiktok::UnbindResponse,
            synctv_proto::providers::tiktok::ResolveRequest,
            synctv_proto::providers::tiktok::ResolveResponse,
            synctv_proto::providers::tiktok::GetUserRequest,
            synctv_proto::providers::tiktok::GetUserResponse,
            synctv_proto::providers::tiktok::ListUserPostsRequest,
            synctv_proto::providers::tiktok::ListUserPostsResponse,
            synctv_proto::providers::fnos::LoginRequest,
            synctv_proto::providers::fnos::LoginResponse,
            synctv_proto::providers::fnos::Authenticated,
            synctv_proto::providers::fnos::TwoFactorRequired,
            synctv_proto::providers::fnos::ListRequest,
            synctv_proto::providers::fnos::ListResponse,
            synctv_proto::providers::fnos::FileItem,
            synctv_proto::providers::fnos::ListMediaLibrariesRequest,
            synctv_proto::providers::fnos::ListMediaLibrariesResponse,
            synctv_proto::providers::fnos::MediaLibrary,
            synctv_proto::providers::fnos::ListMediaItemsRequest,
            synctv_proto::providers::fnos::ListMediaItemsResponse,
            synctv_proto::providers::fnos::MediaItem,
            synctv_proto::providers::fnos::SetFavoriteRequest,
            synctv_proto::providers::fnos::SetFavoriteResponse,
            synctv_proto::providers::fnos::SetWatchedRequest,
            synctv_proto::providers::fnos::SetWatchedResponse,
            synctv_proto::providers::fnos::GetServerInfoRequest,
            synctv_proto::providers::fnos::GetServerInfoResponse,
            synctv_proto::providers::fnos::LogoutRequest,
            synctv_proto::providers::fnos::LogoutResponse,
            synctv_proto::providers::fnos::GetBindsResponse,
            synctv_proto::providers::fnos::BindInfo,
            synctv_proto::providers::qnap::LoginRequest,
            synctv_proto::providers::qnap::LoginResponse,
            synctv_proto::providers::qnap::ListRequest,
            synctv_proto::providers::qnap::ListResponse,
            synctv_proto::providers::qnap::FileItem,
            synctv_proto::providers::qnap::GetCapabilitiesRequest,
            synctv_proto::providers::qnap::GetCapabilitiesResponse,
            synctv_proto::providers::qnap::LogoutRequest,
            synctv_proto::providers::qnap::LogoutResponse,
            synctv_proto::providers::qnap::GetBindsResponse,
            synctv_proto::providers::qnap::BindInfo,
            synctv_proto::providers::synology::LoginRequest,
            synctv_proto::providers::synology::LoginResponse,
            synctv_proto::providers::synology::ListFilesRequest,
            synctv_proto::providers::synology::ListFilesResponse,
            synctv_proto::providers::synology::FileItem,
            synctv_proto::providers::synology::ListLibrariesRequest,
            synctv_proto::providers::synology::ListLibrariesResponse,
            synctv_proto::providers::synology::VideoLibrary,
            synctv_proto::providers::synology::ListMoviesRequest,
            synctv_proto::providers::synology::ListTvShowsRequest,
            synctv_proto::providers::synology::ListEpisodesRequest,
            synctv_proto::providers::synology::ListHomeVideosRequest,
            synctv_proto::providers::synology::ListTvRecordingsRequest,
            synctv_proto::providers::synology::ListVideoItemsResponse,
            synctv_proto::providers::synology::VideoItem,
            synctv_proto::providers::synology::VideoFile,
            synctv_proto::providers::synology::SynologyVideoEntryKind,
            synctv_proto::providers::synology::LogoutRequest,
            synctv_proto::providers::synology::LogoutResponse,
            synctv_proto::providers::synology::GetBindsResponse,
            synctv_proto::providers::synology::BindInfo,
            synctv_proto::providers::nextcloud::LoginRequest,
            synctv_proto::providers::nextcloud::LoginResponse,
            synctv_proto::providers::nextcloud::StartLoginFlowRequest,
            synctv_proto::providers::nextcloud::StartLoginFlowResponse,
            synctv_proto::providers::nextcloud::PollLoginFlowRequest,
            synctv_proto::providers::nextcloud::ListRequest,
            synctv_proto::providers::nextcloud::ListFavoritesRequest,
            synctv_proto::providers::nextcloud::ListResponse,
            synctv_proto::providers::nextcloud::FileItem,
            synctv_proto::providers::nextcloud::LogoutRequest,
            synctv_proto::providers::nextcloud::LogoutResponse,
            synctv_proto::providers::nextcloud::GetBindsResponse,
            synctv_proto::providers::nextcloud::BindInfo,
            synctv_proto::providers::seafile::LoginRequest,
            synctv_proto::providers::seafile::LoginResponse,
            synctv_proto::providers::seafile::UnlockLibraryRequest,
            synctv_proto::providers::seafile::UnlockLibraryResponse,
            synctv_proto::providers::seafile::ListRepositoriesRequest,
            synctv_proto::providers::seafile::ListRequest,
            synctv_proto::providers::seafile::ListStarredRequest,
            synctv_proto::providers::seafile::ListResponse,
            synctv_proto::providers::seafile::FileItem,
            synctv_proto::providers::seafile::LogoutRequest,
            synctv_proto::providers::seafile::LogoutResponse,
            synctv_proto::providers::seafile::GetBindsResponse,
            synctv_proto::providers::seafile::BindInfo,
            synctv_proto::providers::truenas::LoginRequest,
            synctv_proto::providers::truenas::LoginResponse,
            synctv_proto::providers::truenas::ListRequest,
            synctv_proto::providers::truenas::ListResponse,
            synctv_proto::providers::truenas::FileItem,
            synctv_proto::providers::truenas::LogoutRequest,
            synctv_proto::providers::truenas::LogoutResponse,
            synctv_proto::providers::truenas::GetBindsResponse,
            synctv_proto::providers::truenas::BindInfo,
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
            synctv_proto::providers::bilibili::ParseCandidate,
            synctv_proto::providers::bilibili::ListLiveAreasRequest,
            synctv_proto::providers::bilibili::ListLiveAreasResponse,
            synctv_proto::providers::bilibili::LiveArea,
            synctv_proto::providers::bilibili::ListFavoriteFoldersRequest,
            synctv_proto::providers::bilibili::ListFavoriteFoldersResponse,
            synctv_proto::providers::bilibili::FavoriteFolder,
            synctv_proto::providers::bilibili::ListFollowedPgcRequest,
            synctv_proto::providers::bilibili::ListFollowedPgcResponse,
            synctv_proto::providers::bilibili::FollowedPgcSeason,
            synctv_proto::providers::bilibili::PgcFollowType,
            synctv_proto::providers::bilibili::ListHistoryRequest,
            synctv_proto::providers::bilibili::ListHistoryResponse,
            synctv_proto::providers::bilibili::HistoryItem,
            synctv_proto::providers::bilibili::ListPgcTimelineRequest,
            synctv_proto::providers::bilibili::ListPgcTimelineResponse,
            synctv_proto::providers::bilibili::PgcTimelineItem,
            synctv_proto::providers::bilibili::ListPgcSeasonsRequest,
            synctv_proto::providers::bilibili::ListPgcSeasonsResponse,
            synctv_proto::providers::bilibili::PgcSeason,
            synctv_proto::providers::bilibili::PgcSeasonType,
            synctv_proto::providers::bilibili::PgcSeasonOrder,
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
            synctv_proto::admin::GetServiceStateResponse,
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
            synctv_proto::admin::RoomCreationReview,
            synctv_proto::admin::ListRoomCreationReviewsRequest,
            synctv_proto::admin::ListRoomCreationReviewsResponse,
            synctv_proto::admin::ApproveRoomCreationReviewRequest,
            synctv_proto::admin::ApproveRoomCreationReviewResponse,
            synctv_proto::admin::RejectRoomCreationReviewRequest,
            synctv_proto::admin::RoomJoinReview,
            synctv_proto::admin::ListRoomJoinReviewsRequest,
            synctv_proto::admin::ListRoomJoinReviewsResponse,
            synctv_proto::admin::ApproveRoomJoinReviewRequest,
            synctv_proto::admin::ApproveRoomJoinReviewResponse,
            synctv_proto::admin::RejectRoomJoinReviewRequest,
            synctv_proto::admin::BanRecord,
            synctv_proto::admin::ListBanRecordsRequest,
            synctv_proto::admin::ListBanRecordsResponse,
            synctv_proto::admin::ContentReport,
            synctv_proto::admin::ListContentReportsRequest,
            synctv_proto::admin::ListContentReportsResponse,
            synctv_proto::admin::ContentReport,
            synctv_proto::admin::UpdateContentReportStatusRequest,
            synctv_proto::admin::UpdateContentReportStatusResponse,
            synctv_proto::admin::ListUsersResponse,
            synctv_proto::admin::AdminUser,
            synctv_proto::admin::GetUserPreferencesResponse,
            synctv_proto::admin::UpdateUserPreferencesRequest,
            synctv_proto::admin::UpdateUserPreferencesResponse,
            synctv_proto::admin::CreateUserRequest,
            synctv_proto::admin::AdminUser,
            synctv_proto::admin::DeleteUserResponse,
            synctv_proto::admin::UpdateUserRoleRequest,
            synctv_proto::admin::AdminUser,
            synctv_proto::admin::SetUserPasswordRequest,
            synctv_proto::admin::SetUserPasswordResponse,
            synctv_proto::admin::UpdateUserUsernameRequest,
            synctv_proto::admin::AdminUser,
            synctv_proto::admin::BanUserRequest,
            synctv_proto::admin::AdminUser,
            synctv_proto::admin::AdminUser,
            synctv_proto::admin::GetUserRoomsResponse,
            synctv_proto::admin::BatchBanUsersRequest,
            synctv_proto::admin::BatchBanUsersResponse,
            synctv_proto::admin::BatchDeleteUsersRequest,
            synctv_proto::admin::BatchDeleteUsersResponse,
            synctv_proto::admin::ListRoomsResponse,
            synctv_proto::admin::ListRoomCategoriesRequest,
            synctv_proto::admin::ListRoomCategoriesResponse,
            synctv_proto::admin::UpsertRoomCategoryRequest,
            synctv_proto::client::RoomCategory,
            synctv_proto::admin::DeleteRoomCategoryResponse,
            synctv_proto::admin::ListRoomLabelsRequest,
            synctv_proto::admin::ListRoomLabelsResponse,
            synctv_proto::admin::UpsertRoomLabelRequest,
            synctv_proto::client::RoomLabel,
            synctv_proto::admin::DeleteRoomLabelResponse,
            synctv_proto::admin::UpdateRoomTaxonomyRequest,
            synctv_proto::admin::Room,
            synctv_proto::admin::Room,
            synctv_proto::admin::DeleteRoomResponse,
            synctv_proto::admin::UpdateRoomPasswordRequest,
            synctv_proto::admin::UpdateRoomPasswordResponse,
            synctv_proto::admin::GetRoomMembersResponse,
            synctv_proto::admin::BanRoomRequest,
            synctv_proto::admin::Room,
            synctv_proto::admin::Room,
            synctv_proto::admin::GetRoomSettingsResponse,
            synctv_proto::admin::UpdateRoomSettingsRequest,
            synctv_proto::admin::Room,
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
            synctv_proto::admin::AdminUser,
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
    fn openapi_preserves_authored_path_order() {
        let doc = super::ApiDoc::openapi();
        let paths = doc
            .paths
            .paths
            .keys()
            .take(5)
            .map(String::as_str)
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            [
                "/api/public/settings",
                "/api/public/server-info",
                "/api/public/time",
                "/api/auth/email/confirm",
                "/api/auth/guest-token",
            ]
        );
    }

    #[test]
    fn openapi_preserves_component_field_order() -> TestResult {
        let doc = super::ApiDoc::openapi();
        let schema = doc
            .components
            .as_ref()
            .and_then(|components| components.schemas.get("GoogleRpcStatusSchema"))
            .ok_or_else(|| test_error("GoogleRpcStatusSchema should be registered"))?;
        let schema_json = serde_json::to_string(schema)?;
        let code = schema_json
            .find("\"code\"")
            .ok_or_else(|| test_error("code property should be documented"))?;
        let message = schema_json
            .find("\"message\"")
            .ok_or_else(|| test_error("message property should be documented"))?;
        let details = schema_json
            .find("\"details\"")
            .ok_or_else(|| test_error("details property should be documented"))?;

        assert!(code < message && message < details);
        Ok(())
    }

    #[test]
    fn openapi_keeps_bearer_auth_off_public_routes() -> TestResult {
        let doc = openapi_json()?;

        assert!(
            doc.get("security").is_none(),
            "document-level security should not force bearer auth onto public routes"
        );

        for (path, method) in [
            ("/api/public/settings", "get"),
            ("/api/auth/email/confirm", "post"),
            ("/api/auth/opaque/registration/start", "post"),
            ("/api/auth/opaque/registration/finish", "post"),
            ("/api/rooms/discover", "get"),
            ("/api/rooms/{roomId}/discovery", "get"),
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
        for path in [
            "/api/user/rooms/discover",
            "/api/user/rooms/{roomId}/discovery",
        ] {
            let security = doc["paths"][path]["get"]["security"]
                .as_array()
                .ok_or_else(|| test_error("user discovery should declare security"))?;
            assert!(
                !security.is_empty(),
                "GET {path} should require bearer auth"
            );
        }
        Ok(())
    }

    #[test]
    fn openapi_documents_cloudreve_provider_contract() -> TestResult {
        let doc = openapi_json()?;

        for (path, method) in [
            ("/api/providers/cloudreve/login", "post"),
            ("/api/providers/cloudreve/list", "post"),
            ("/api/providers/cloudreve/search", "post"),
            ("/api/providers/cloudreve/me", "post"),
            ("/api/providers/cloudreve/logout", "post"),
            ("/api/providers/cloudreve/binds", "get"),
        ] {
            let operation = &doc["paths"][path][method];
            assert!(
                operation.is_object(),
                "{method} {path} should be documented"
            );
            assert!(
                operation["responses"]["200"].is_object(),
                "{method} {path} should document its success response"
            );
            assert!(
                operation["security"].is_array(),
                "{method} {path} should require bearer authentication"
            );
        }

        for path in [
            "/api/providers/cloudreve/login",
            "/api/providers/cloudreve/list",
            "/api/providers/cloudreve/search",
            "/api/providers/cloudreve/me",
            "/api/providers/cloudreve/logout",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "POST {path} should document its protobuf request body"
            );
        }

        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("cloudreve")),
            "Cloudreve protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_youtube_provider_contract() -> TestResult {
        let doc = openapi_json()?;

        for (path, method) in [
            ("/api/providers/youtube/bind", "post"),
            ("/api/providers/youtube/binds", "get"),
            ("/api/providers/youtube/unbind", "post"),
            (
                "/api/playback-providers/youtube/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            ("/api/playback-providers/youtube/{version}/segments", "get"),
            (
                "/api/playback-providers/youtube/{version}/subtitles/{modeName}/{subtitleIndex}",
                "get",
            ),
        ] {
            let operation = &doc["paths"][path][method];
            assert!(
                operation.is_object(),
                "{method} {path} should be documented"
            );
            assert!(
                operation["responses"]["200"].is_object(),
                "{method} {path} should document its success response"
            );
        }

        for path in [
            "/api/providers/youtube/bind",
            "/api/providers/youtube/unbind",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "POST {path} should document its protobuf request body"
            );
        }

        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("youtube")),
            "YouTube protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_generation_scoped_live_hls_contract() -> TestResult {
        let doc = openapi_json()?;

        for provider in ["rtmp", "live-proxy"] {
            for path in [
                format!("/api/playback-providers/{provider}/{{version}}/hls-master"),
                format!(
                    "/api/playback-providers/{provider}/{{version}}/hls/{{generationId}}/index.m3u8"
                ),
                format!(
                    "/api/playback-providers/{provider}/{{version}}/hls/{{generationId}}/{{segmentName}}"
                ),
            ] {
                let operation = &doc["paths"][&path]["get"];
                assert!(operation.is_object(), "GET {path} should be documented");
                assert!(
                    operation["responses"]["200"].is_object(),
                    "GET {path} should document its success response"
                );
            }
            for removed_path in [
                format!("/api/playback-providers/{provider}/{{version}}/hls-playlist"),
                format!(
                    "/api/playback-providers/{provider}/{{version}}/hls-segments/{{segmentName}}"
                ),
            ] {
                assert!(
                    doc["paths"].get(&removed_path).is_none(),
                    "removed live HLS path should be absent: {removed_path}"
                );
            }
        }

        Ok(())
    }

    #[test]
    fn openapi_documents_douyin_provider_contract() -> TestResult {
        let doc = openapi_json()?;

        for (path, method) in [
            ("/api/providers/douyin/bind", "post"),
            ("/api/providers/douyin/binds", "get"),
            ("/api/providers/douyin/unbind", "post"),
            ("/api/providers/douyin/resolve", "post"),
            ("/api/providers/douyin/user-posts", "post"),
            (
                "/api/playback-providers/douyin/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            ("/api/playback-providers/douyin/{version}/segments", "get"),
            (
                "/api/playback-providers/douyin/{version}/danmakus/{modeName}/{mediaIndex}",
                "get",
            ),
        ] {
            let operation = &doc["paths"][path][method];
            assert!(
                operation.is_object(),
                "{method} {path} should be documented"
            );
            assert!(
                operation["responses"]["200"].is_object(),
                "{method} {path} should document its success response"
            );
        }

        for path in [
            "/api/providers/douyin/bind",
            "/api/providers/douyin/unbind",
            "/api/providers/douyin/resolve",
            "/api/providers/douyin/user-posts",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "POST {path} should document its protobuf request body"
            );
        }

        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("douyin")),
            "Douyin protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_tiktok_provider_contract() -> TestResult {
        let doc = openapi_json()?;

        for (path, method) in [
            ("/api/providers/tiktok/bind", "post"),
            ("/api/providers/tiktok/binds", "get"),
            ("/api/providers/tiktok/unbind", "post"),
            ("/api/providers/tiktok/resolve", "post"),
            ("/api/providers/tiktok/user", "post"),
            ("/api/providers/tiktok/user-posts", "post"),
            (
                "/api/playback-providers/tiktok/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            ("/api/playback-providers/tiktok/{version}/segments", "get"),
            (
                "/api/playback-providers/tiktok/{version}/subtitles/{modeName}/{subtitleIndex}",
                "get",
            ),
        ] {
            let operation = &doc["paths"][path][method];
            assert!(
                operation.is_object(),
                "{method} {path} should be documented"
            );
            assert!(
                operation["responses"]["200"].is_object(),
                "{method} {path} should document its success response"
            );
        }

        for path in [
            "/api/providers/tiktok/bind",
            "/api/providers/tiktok/unbind",
            "/api/providers/tiktok/resolve",
            "/api/providers/tiktok/user",
            "/api/providers/tiktok/user-posts",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "POST {path} should document its protobuf request body"
            );
        }

        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("tiktok")),
            "TikTok protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_twitch_provider_contract() -> TestResult {
        let doc = openapi_json()?;

        for (path, method) in [
            ("/api/providers/twitch/bind", "post"),
            ("/api/providers/twitch/binds", "get"),
            ("/api/providers/twitch/unbind", "post"),
            ("/api/providers/twitch/resolve", "post"),
            ("/api/providers/twitch/channel-items", "post"),
            (
                "/api/playback-providers/twitch/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            ("/api/playback-providers/twitch/{version}/segments", "get"),
            (
                "/api/playback-providers/twitch/{version}/chats/{modeName}/{mediaIndex}",
                "get",
            ),
        ] {
            let operation = &doc["paths"][path][method];
            assert!(
                operation.is_object(),
                "{method} {path} should be documented"
            );
            assert!(
                operation["responses"]["200"].is_object(),
                "{method} {path} should document its success response"
            );
        }

        for path in [
            "/api/providers/twitch/bind",
            "/api/providers/twitch/unbind",
            "/api/providers/twitch/resolve",
            "/api/providers/twitch/channel-items",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "POST {path} should document its protobuf request body"
            );
        }

        let chat_response = &doc["paths"]
            ["/api/playback-providers/twitch/{version}/chats/{modeName}/{mediaIndex}"]["get"]
            ["responses"]["200"]["content"]["text/event-stream"];
        assert!(
            chat_response["schema"].is_object(),
            "Twitch chat SSE should document its event schema"
        );

        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("twitch")),
            "Twitch protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_huya_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        let operation = &doc["paths"]["/api/providers/huya/resolve"]["post"];
        assert!(operation.is_object(), "Huya resolve should be documented");
        assert!(
            operation["requestBody"].is_object(),
            "Huya resolve should document its protobuf request body"
        );
        assert!(
            operation["responses"]["200"].is_object(),
            "Huya resolve should document its response"
        );
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("huya")),
            "Huya provider schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_douyu_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        let operation = &doc["paths"]["/api/providers/douyu/resolve"]["post"];
        assert!(operation.is_object(), "Douyu resolve should be documented");
        assert!(operation["requestBody"].is_object());
        assert!(operation["responses"]["200"].is_object());
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(schemas.keys().any(|name| name.contains("douyu")));
        Ok(())
    }

    #[test]
    fn openapi_documents_acfun_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        let operation = &doc["paths"]["/api/providers/acfun/resolve"]["post"];
        assert!(operation.is_object(), "AcFun resolve should be documented");
        assert!(operation["requestBody"].is_object());
        assert!(operation["responses"]["200"].is_object());
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(schemas.keys().any(|name| name.contains("acfun")));
        Ok(())
    }

    #[test]
    fn openapi_documents_cctv_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        let operation = &doc["paths"]["/api/providers/cctv/resolve"]["post"];
        assert!(operation.is_object(), "CCTV resolve should be documented");
        assert!(operation["requestBody"].is_object());
        assert!(operation["responses"]["200"].is_object());
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(schemas.keys().any(|name| name.contains("cctv")));
        Ok(())
    }

    #[test]
    fn openapi_documents_fnos_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for (path, method) in [
            ("/api/providers/fnos/login", "post"),
            ("/api/providers/fnos/list", "post"),
            ("/api/providers/fnos/server-info", "post"),
            ("/api/providers/fnos/logout", "post"),
            ("/api/providers/fnos/binds", "get"),
            (
                "/api/playback-providers/fnos/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            ("/api/playback-providers/fnos/{version}/segments", "get"),
        ] {
            assert!(
                doc["paths"][path][method].is_object(),
                "{method} {path} should be documented"
            );
        }
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("fnos")),
            "FNOS protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_qnap_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for (path, method) in [
            ("/api/providers/qnap/login", "post"),
            ("/api/providers/qnap/list", "post"),
            ("/api/providers/qnap/capabilities", "post"),
            ("/api/providers/qnap/logout", "post"),
            ("/api/providers/qnap/binds", "get"),
            ("/api/providers/qnap/thumbnail", "get"),
            (
                "/api/playback-providers/qnap/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            (
                "/api/playback-providers/qnap/{version}/subtitles/{modeName}/{subtitleIndex}",
                "get",
            ),
            ("/api/playback-providers/qnap/{version}/thumbnail", "get"),
        ] {
            let operation = &doc["paths"][path][method];
            assert!(
                operation.is_object(),
                "{method} {path} should be documented"
            );
            assert!(
                operation["responses"]["200"].is_object(),
                "{method} {path} should document its success response"
            );
        }

        for path in [
            "/api/providers/qnap/login",
            "/api/providers/qnap/list",
            "/api/providers/qnap/capabilities",
            "/api/providers/qnap/logout",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "POST {path} should document its protobuf request body"
            );
        }

        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("qnap")),
            "QNAP protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_synology_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for (path, method) in [
            ("/api/providers/synology/login", "post"),
            ("/api/providers/synology/files", "post"),
            ("/api/providers/synology/libraries", "post"),
            ("/api/providers/synology/movies", "post"),
            ("/api/providers/synology/tv-shows", "post"),
            ("/api/providers/synology/episodes", "post"),
            ("/api/providers/synology/home-videos", "post"),
            ("/api/providers/synology/tv-recordings", "post"),
            ("/api/providers/synology/logout", "post"),
            ("/api/providers/synology/binds", "get"),
            ("/api/providers/synology/image", "get"),
            (
                "/api/playback-providers/synology/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            ("/api/playback-providers/synology/{version}/segments", "get"),
            (
                "/api/playback-providers/synology/{version}/subtitles/{modeName}/{subtitleIndex}",
                "get",
            ),
        ] {
            assert!(
                doc["paths"][path][method].is_object(),
                "missing Synology OpenAPI operation {method} {path}"
            );
        }

        for path in [
            "/api/providers/synology/login",
            "/api/providers/synology/files",
            "/api/providers/synology/libraries",
            "/api/providers/synology/movies",
            "/api/providers/synology/tv-shows",
            "/api/providers/synology/episodes",
            "/api/providers/synology/home-videos",
            "/api/providers/synology/tv-recordings",
            "/api/providers/synology/logout",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "missing Synology request body for {path}"
            );
        }

        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("synology")),
            "Synology protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_nextcloud_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for (path, method) in [
            ("/api/providers/nextcloud/login", "post"),
            ("/api/providers/nextcloud/login-flow/start", "post"),
            ("/api/providers/nextcloud/login-flow/poll", "post"),
            ("/api/providers/nextcloud/list", "post"),
            ("/api/providers/nextcloud/favorites", "post"),
            ("/api/providers/nextcloud/logout", "post"),
            ("/api/providers/nextcloud/binds", "get"),
            ("/api/providers/nextcloud/preview", "get"),
            (
                "/api/playback-providers/nextcloud/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            (
                "/api/playback-providers/nextcloud/{version}/subtitles/{modeName}/{subtitleIndex}",
                "get",
            ),
        ] {
            assert!(
                doc["paths"][path][method].is_object(),
                "missing Nextcloud OpenAPI operation {method} {path}"
            );
        }
        for path in [
            "/api/providers/nextcloud/login",
            "/api/providers/nextcloud/login-flow/start",
            "/api/providers/nextcloud/login-flow/poll",
            "/api/providers/nextcloud/list",
            "/api/providers/nextcloud/favorites",
            "/api/providers/nextcloud/logout",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "missing Nextcloud request body for {path}"
            );
        }
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("nextcloud")),
            "Nextcloud protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_seafile_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for (path, method) in [
            ("/api/providers/seafile/login", "post"),
            ("/api/providers/seafile/unlock-library", "post"),
            ("/api/providers/seafile/repositories", "post"),
            ("/api/providers/seafile/list", "post"),
            ("/api/providers/seafile/starred", "post"),
            ("/api/providers/seafile/logout", "post"),
            ("/api/providers/seafile/binds", "get"),
            ("/api/providers/seafile/thumbnail", "get"),
            (
                "/api/playback-providers/seafile/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            (
                "/api/playback-providers/seafile/{version}/subtitles/{modeName}/{subtitleIndex}",
                "get",
            ),
        ] {
            assert!(
                doc["paths"][path][method].is_object(),
                "missing Seafile OpenAPI operation {method} {path}"
            );
        }
        for path in [
            "/api/providers/seafile/login",
            "/api/providers/seafile/unlock-library",
            "/api/providers/seafile/repositories",
            "/api/providers/seafile/list",
            "/api/providers/seafile/starred",
            "/api/providers/seafile/logout",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "missing Seafile request body for {path}"
            );
        }
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("seafile")),
            "Seafile protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_truenas_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for (path, method) in [
            ("/api/providers/truenas/login", "post"),
            ("/api/providers/truenas/list", "post"),
            ("/api/providers/truenas/logout", "post"),
            ("/api/providers/truenas/binds", "get"),
            (
                "/api/playback-providers/truenas/{version}/resources/{modeName}/{mediaIndex}",
                "get",
            ),
            (
                "/api/playback-providers/truenas/{version}/subtitles/{modeName}/{subtitleIndex}",
                "get",
            ),
        ] {
            assert!(
                doc["paths"][path][method].is_object(),
                "missing TrueNAS OpenAPI operation {method} {path}"
            );
        }
        for path in [
            "/api/providers/truenas/login",
            "/api/providers/truenas/list",
            "/api/providers/truenas/logout",
        ] {
            assert!(
                doc["paths"][path]["post"]["requestBody"].is_object(),
                "missing TrueNAS request body for {path}"
            );
        }
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("truenas")),
            "TrueNAS protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_huya_playback_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for path in [
            "/api/playback-providers/huya/{version}/resources/{modeName}/{mediaIndex}",
            "/api/playback-providers/huya/{version}/segments",
            "/api/playback-providers/huya/{version}/danmakus/{modeName}/{mediaIndex}",
        ] {
            assert!(
                doc["paths"][path]["get"].is_object(),
                "GET {path} should be documented"
            );
        }
        let danmaku = &doc["paths"]
            ["/api/playback-providers/huya/{version}/danmakus/{modeName}/{mediaIndex}"]["get"]
            ["responses"]["200"]["content"]["text/event-stream"];
        assert!(
            danmaku["schema"].is_object(),
            "Huya danmaku should document its event schema"
        );
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("huya")),
            "Huya protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_douyu_playback_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for path in [
            "/api/playback-providers/douyu/{version}/resources/{modeName}/{mediaIndex}",
            "/api/playback-providers/douyu/{version}/segments",
            "/api/playback-providers/douyu/{version}/danmakus/{modeName}/{mediaIndex}",
        ] {
            assert!(
                doc["paths"][path]["get"].is_object(),
                "GET {path} should be documented"
            );
        }
        let danmaku = &doc["paths"]
            ["/api/playback-providers/douyu/{version}/danmakus/{modeName}/{mediaIndex}"]["get"]
            ["responses"]["200"]["content"]["text/event-stream"];
        assert!(
            danmaku["schema"].is_object(),
            "Douyu danmaku should document its event schema"
        );
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("douyu")),
            "Douyu protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_acfun_playback_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for path in [
            "/api/playback-providers/acfun/{version}/resources/{modeName}/{mediaIndex}",
            "/api/playback-providers/acfun/{version}/segments.ts",
            "/api/playback-providers/acfun/{version}/danmaku-files/{modeName}/{mediaIndex}",
            "/api/playback-providers/acfun/{version}/danmakus/{modeName}/{mediaIndex}",
        ] {
            assert!(
                doc["paths"][path]["get"].is_object(),
                "GET {path} should be documented"
            );
        }
        let danmaku = &doc["paths"]
            ["/api/playback-providers/acfun/{version}/danmakus/{modeName}/{mediaIndex}"]["get"]
            ["responses"]["200"]["content"]["text/event-stream"];
        assert!(
            danmaku["schema"].is_object(),
            "AcFun live danmaku should document its event schema"
        );
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas.keys().any(|name| name.contains("acfun")),
            "AcFun protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_documents_cctv_playback_provider_contract() -> TestResult {
        let doc = openapi_json()?;
        for path in [
            "/api/playback-providers/cctv/{version}/resources/{modeName}/{mediaIndex}",
            "/api/playback-providers/cctv/{version}/segments",
        ] {
            assert!(
                doc["paths"][path]["get"].is_object(),
                "missing CCTV playback provider path {path}"
            );
        }
        let schemas = doc["components"]["schemas"]
            .as_object()
            .ok_or_else(|| test_error("OpenAPI components.schemas should be an object"))?;
        assert!(
            schemas
                .keys()
                .any(|name| name.to_ascii_lowercase().contains("cctv")),
            "CCTV protobuf schemas should be registered"
        );
        Ok(())
    }

    #[test]
    fn openapi_matches_public_room_error_contracts() -> TestResult {
        let doc = openapi_json()?;

        assert!(
            doc["paths"]["/api/rooms/discover"]["get"]["responses"]["400"].is_object(),
            "room discovery should document validation errors"
        );
        assert!(
            doc["paths"]["/api/rooms/{roomId}/discovery"]["get"]["responses"]["400"].is_object(),
            "room discovery item should document invalid room IDs"
        );
        assert!(
            doc["paths"]["/api/rooms/{roomId}/discovery"]["get"]["responses"]["404"].is_object(),
            "room discovery item should document missing rooms"
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
