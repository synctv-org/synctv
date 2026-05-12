use axum::Router;
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

use crate::http::providers::{alist, bilibili, common, emby, rtmp};
use crate::http::{
    admin, auth, email_verification, health, notifications, oauth2, public, room, room_extra,
    ticket, user, webrtc, websocket, AppState,
};
use crate::proto::client;

pub type ErrorResponseDoc = client::ApiErrorResponse;

#[derive(OpenApi)]
#[openapi(
    paths(
        health::liveness_check,
        health::readiness_check,
        public::get_public_settings,
        auth::register,
        auth::login,
        auth::create_guest_token,
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
        auth::verify_mfa_password,
        auth::refresh_token,
        auth::logout,
        ticket::create_ticket,
        webrtc::get_ice_servers,
        user::get_me,
        user::update_user,
        user::get_user_preferences,
        user::update_user_preferences,
        user::start_opaque_password_update,
        user::finish_opaque_password_update,
        user::start_passkey_bind,
        user::finish_passkey_bind,
        user::list_passkeys,
        user::delete_passkey,
        user::list_my_rooms,
        user::delete_me,
        email_verification::send_verification_email,
        email_verification::confirm_email,
        email_verification::request_password_reset,
        email_verification::confirm_password_reset,
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
        bilibili::new_captcha,
        bilibili::sms_send,
        bilibili::sms_login,
        bilibili::user_info,
        bilibili::binds,
        bilibili::logout,
        emby::thumbnail,
        rtmp::generate_publish_key,
        rtmp::handle_stream_info,
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
        room::update_playback,
        room::set_room_password,
        room::get_chat_history,
        room::list_playlist_items,
        room::delete_playlist,
        room::update_playlist,
        room::move_playlist,
        room::reset_room_settings,
        room_extra::kick_member,
        room_extra::set_member_permissions,
        room_extra::ban_member,
        room_extra::unban_member,
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
        admin::get_settings,
        admin::set_settings,
        admin::get_settings_group,
        admin::send_test_email,
        admin::list_users,
        admin::create_user,
        admin::get_user,
        admin::delete_user,
        admin::set_user_role,
        admin::set_user_password,
        admin::set_user_username,
        admin::ban_user,
        admin::unban_user,
        admin::get_user_rooms,
        admin::batch_ban_users,
        admin::batch_delete_users,
        admin::list_rooms,
        admin::get_room,
        admin::delete_room,
        admin::set_room_password,
        admin::get_room_members,
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
            ErrorResponseDoc,
            health::HealthResponse,
            health::HealthDetails,
            health::MemoryHealth,
            client::CreateWebSocketTicketRequest,
            client::CreateWebSocketTicketResponse,
            client::UpdateUserRequest,
            client::UpdateUserResponse,
            client::RegisterRequest,
            client::RegisterResponse,
            client::LoginRequest,
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
            client::RequestMfaEmailCodeRequest,
            client::RequestMfaEmailCodeResponse,
            client::VerifyMfaEmailCodeRequest,
            client::StartMfaPasskeyRequest,
            client::StartMfaPasskeyResponse,
            client::FinishMfaPasskeyRequest,
            client::VerifyMfaPasswordRequest,
            client::RefreshTokenRequest,
            client::RefreshTokenResponse,
            client::LogoutResponse,
            crate::proto::providers::rtmp::CreatePublishKeyResponse,
            client::SendVerificationEmailRequest,
            client::SendVerificationEmailResponse,
            client::ConfirmEmailRequest,
            client::ConfirmEmailResponse,
            client::RequestPasswordResetRequest,
            client::RequestPasswordResetResponse,
            client::ConfirmPasswordResetRequest,
            client::ConfirmPasswordResetResponse,
            client::GetPublicSettingsResponse,
            client::GetProfileResponse,
            client::StartOpaquePasswordUpdateRequest,
            client::StartOpaquePasswordUpdateResponse,
            client::FinishOpaquePasswordUpdateRequest,
            client::FinishOpaquePasswordUpdateResponse,
            client::StartPasskeyBindRequest,
            client::StartPasskeyBindResponse,
            client::FinishPasskeyBindRequest,
            client::PasskeyCredentialResponse,
            client::ListPasskeysResponse,
            client::DeletePasskeyResponse,
            client::GetUserPreferencesResponse,
            client::UpdateUserPreferencesRequest,
            client::UpdateUserPreferencesResponse,
            client::GetIceServersResponse,
            client::ListMyRoomsResponse,
            client::DeleteRoomResponse,
            client::CreateRoomRequest,
            client::CreateRoomResponse,
            client::ListRoomsResponse,
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
            client::UpdateRoomSettingsResponse,
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
            client::UpdatePlayback,
            client::SetRoomPasswordRequest,
            client::SetRoomPasswordResponse,
            client::GetChatHistoryResponse,
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
            client::UpdateMemberPermissionsResponse,
            client::BanMemberRequest,
            client::BanMemberResponse,
            client::UnbanMemberResponse,
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
            crate::proto::providers::common::ProviderInstancesResponse,
            crate::proto::providers::common::ProviderBackendsResponse,
            crate::proto::providers::alist::LoginRequest,
            crate::proto::providers::alist::LoginResponse,
            crate::proto::providers::alist::GetBindsResponse,
            crate::proto::providers::alist::ListRequest,
            crate::proto::providers::alist::ListResponse,
            crate::proto::providers::alist::FileItem,
            crate::proto::providers::alist::SearchRequest,
            crate::proto::providers::alist::SearchResponse,
            crate::proto::providers::alist::SearchItem,
            crate::proto::providers::alist::GetMeRequest,
            crate::proto::providers::alist::GetMeResponse,
            crate::proto::providers::alist::LogoutRequest,
            crate::proto::providers::alist::LogoutResponse,
            crate::proto::providers::emby::LoginRequest,
            crate::proto::providers::emby::LoginResponse,
            crate::proto::providers::emby::GetBindsResponse,
            crate::proto::providers::emby::ListRequest,
            crate::proto::providers::emby::ListResponse,
            crate::proto::providers::emby::MediaItem,
            crate::proto::providers::emby::GetMeRequest,
            crate::proto::providers::emby::GetMeResponse,
            crate::proto::providers::emby::LogoutRequest,
            crate::proto::providers::emby::LogoutResponse,
            crate::proto::providers::bilibili::ParseRequest,
            crate::proto::providers::bilibili::ParseResponse,
            crate::proto::providers::bilibili::VideoInfo,
            crate::proto::providers::bilibili::QrCodeResponse,
            crate::proto::providers::bilibili::CheckQrRequest,
            crate::proto::providers::bilibili::QrStatusResponse,
            crate::proto::providers::bilibili::CaptchaResponse,
            crate::proto::providers::bilibili::SendSmsRequest,
            crate::proto::providers::bilibili::SendSmsResponse,
            crate::proto::providers::bilibili::LoginSmsRequest,
            crate::proto::providers::bilibili::LoginSmsResponse,
            crate::proto::providers::bilibili::UserInfoRequest,
            crate::proto::providers::bilibili::UserInfoResponse,
            crate::proto::providers::bilibili::GetBindsResponse,
            crate::proto::providers::bilibili::LogoutRequest,
            crate::proto::providers::bilibili::LogoutResponse,
            crate::proto::providers::bilibili::QrLoginStatus,
            crate::proto::providers::rtmp::GetStreamInfoResponse,
            crate::proto::providers::rtmp::StreamPublisherInfo,
            crate::proto::admin::GetSystemStatsResponse,
            crate::proto::admin::GetSettingsResponse,
            crate::proto::admin::GetSettingsGroupResponse,
            crate::proto::admin::UpdateSettingsRequest,
            crate::proto::admin::UpdateSettingsResponse,
            crate::proto::admin::SendTestEmailRequest,
            crate::proto::admin::SendTestEmailResponse,
            crate::proto::admin::UserRegistrationReview,
            crate::proto::admin::ListUserRegistrationReviewsRequest,
            crate::proto::admin::ListUserRegistrationReviewsResponse,
            crate::proto::admin::ApproveUserRegistrationReviewRequest,
            crate::proto::admin::ApproveUserRegistrationReviewResponse,
            crate::proto::admin::RejectUserRegistrationReviewRequest,
            crate::proto::admin::RejectUserRegistrationReviewResponse,
            crate::proto::admin::RoomCreationReview,
            crate::proto::admin::ListRoomCreationReviewsRequest,
            crate::proto::admin::ListRoomCreationReviewsResponse,
            crate::proto::admin::ApproveRoomCreationReviewRequest,
            crate::proto::admin::ApproveRoomCreationReviewResponse,
            crate::proto::admin::RejectRoomCreationReviewRequest,
            crate::proto::admin::RejectRoomCreationReviewResponse,
            crate::proto::admin::RoomJoinReview,
            crate::proto::admin::ListRoomJoinReviewsRequest,
            crate::proto::admin::ListRoomJoinReviewsResponse,
            crate::proto::admin::ApproveRoomJoinReviewRequest,
            crate::proto::admin::ApproveRoomJoinReviewResponse,
            crate::proto::admin::RejectRoomJoinReviewRequest,
            crate::proto::admin::RejectRoomJoinReviewResponse,
            crate::proto::admin::BanRecord,
            crate::proto::admin::ListBanRecordsRequest,
            crate::proto::admin::ListBanRecordsResponse,
            crate::proto::admin::ListUsersResponse,
            crate::proto::admin::GetUserResponse,
            crate::proto::admin::CreateUserRequest,
            crate::proto::admin::CreateUserResponse,
            crate::proto::admin::DeleteUserResponse,
            crate::proto::admin::UpdateUserRoleRequest,
            crate::proto::admin::UpdateUserRoleResponse,
            crate::proto::admin::UpdateUserPasswordRequest,
            crate::proto::admin::UpdateUserPasswordResponse,
            crate::proto::admin::UpdateUserUsernameRequest,
            crate::proto::admin::UpdateUserUsernameResponse,
            crate::proto::admin::BanUserRequest,
            crate::proto::admin::BanUserResponse,
            crate::proto::admin::UnbanUserResponse,
            crate::proto::admin::GetUserRoomsResponse,
            crate::proto::admin::BatchBanUsersRequest,
            crate::proto::admin::BatchBanUsersResponse,
            crate::proto::admin::BatchDeleteUsersRequest,
            crate::proto::admin::BatchDeleteUsersResponse,
            crate::proto::admin::ListRoomsResponse,
            crate::proto::admin::GetRoomResponse,
            crate::proto::admin::DeleteRoomResponse,
            crate::proto::admin::UpdateRoomPasswordRequest,
            crate::proto::admin::UpdateRoomPasswordResponse,
            crate::proto::admin::GetRoomMembersResponse,
            crate::proto::admin::BanRoomRequest,
            crate::proto::admin::BanRoomResponse,
            crate::proto::admin::UnbanRoomResponse,
            crate::proto::admin::GetRoomSettingsResponse,
            crate::proto::admin::UpdateRoomSettingsRequest,
            crate::proto::admin::UpdateRoomSettingsResponse,
            crate::proto::admin::ResetRoomSettingsResponse,
            crate::proto::admin::BatchBanRoomsRequest,
            crate::proto::admin::BatchBanRoomsResponse,
            crate::proto::admin::BatchDeleteRoomsRequest,
            crate::proto::admin::BatchDeleteRoomsResponse,
            crate::proto::providers::common::ListProviderInstancesRequest,
            crate::proto::providers::common::ListProviderInstancesResponse,
            crate::proto::providers::common::AddProviderInstanceRequest,
            crate::proto::providers::common::AddProviderInstanceResponse,
            crate::proto::providers::common::UpdateProviderInstanceRequest,
            crate::proto::providers::common::UpdateProviderInstanceResponse,
            crate::proto::providers::common::DeleteProviderInstanceResponse,
            crate::proto::providers::common::ReconnectProviderInstanceResponse,
            crate::proto::providers::common::EnableProviderInstanceResponse,
            crate::proto::providers::common::DisableProviderInstanceResponse,
            crate::proto::admin::ListActiveStreamsResponse,
            crate::proto::admin::KickStreamRequest,
            crate::proto::admin::KickStreamResponse,
            crate::proto::admin::ListAdminsResponse,
            crate::proto::admin::AddAdminResponse,
            crate::proto::admin::RemoveAdminResponse,
            crate::proto::admin::AdminUser,
            crate::proto::admin::AdminRoom,
            crate::proto::providers::common::ProviderInstance,
            crate::proto::admin::SettingsGroup
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
        (name = "Email", description = "Email verification and password reset endpoints"),
        (name = "Notification", description = "Authenticated user notification endpoints"),
        (name = "OAuth2", description = "OAuth2 login and account-link endpoints"),
        (name = "Provider", description = "Provider discovery and backend selection endpoints"),
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

    fn openapi_json() -> Value {
        serde_json::to_value(super::ApiDoc::openapi()).expect("serialize openapi")
    }

    #[test]
    fn openapi_keeps_bearer_auth_off_public_routes() {
        let doc = openapi_json();

        assert!(
            doc.get("security").is_none(),
            "document-level security should not force bearer auth onto public routes"
        );

        for (path, method) in [
            ("/health/live", "get"),
            ("/health/ready", "get"),
            ("/api/public/settings", "get"),
            ("/api/auth/login", "post"),
            ("/api/auth/register", "post"),
        ] {
            assert!(
                doc["paths"][path][method].get("security").is_none(),
                "{method} {path} should be documented as public"
            );
        }

        let notifications_security = doc["paths"]["/api/notifications"]["get"]["security"]
            .as_array()
            .expect("authenticated endpoints should declare security");
        assert!(
            !notifications_security.is_empty(),
            "authenticated endpoints should keep bearer auth requirements"
        );
    }

    #[test]
    fn openapi_marks_notifications_read_all_body_optional() {
        let doc = openapi_json();

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
    }

    #[test]
    fn openapi_documents_playlist_source_fields() {
        let doc = openapi_json();
        let playlist = doc
            .pointer("/components/schemas/synctv_client_Playlist/properties")
            .expect("Playlist schema properties should exist");

        for field in ["source_config", "source_provider", "provider_instance_name"] {
            assert!(
                playlist.get(field).is_some(),
                "Playlist schema should document {field}: {playlist:?}"
            );
        }
    }

    #[test]
    fn openapi_documents_websocket_handshake_endpoint() {
        let doc = openapi_json();
        let operation = &doc["paths"]["/ws/rooms/{room_id}"]["get"];

        assert!(
            operation.is_object(),
            "WebSocket route should be present in OpenAPI"
        );
        assert_eq!(operation["operationId"], "connectRoomWebSocket");

        let params = operation["parameters"]
            .as_array()
            .expect("WebSocket route should describe handshake parameters");
        assert!(
            params.iter().any(|param| {
                param["name"] == "room_id" && param["in"] == "path" && param["required"] == true
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
    }
}
