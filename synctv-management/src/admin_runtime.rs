use crate::request_context::RequestContext;
use crate::runtime_error::RuntimeError;
use synctv_core::models::{
    ReviewStatus, RoomListSortBy, RoomStatus, SortDirection, UserId, UserNotificationPreferences,
    UserRole, UserStatus,
};
use synctv_core::service::BanRecordTargetType;
use synctv_proto::{
    admin as admin_proto, client as client_proto, common as common_proto,
    providers::rtmp as rtmp_proto,
};

#[derive(Debug, Clone)]
pub struct ListUsersQuery {
    pub page: i32,
    pub page_size: i32,
    pub status: Option<UserStatus>,
    pub role: Option<UserRole>,
    pub search: String,
    pub sort_by: UserListSortBy,
    pub sort_direction: AdminSortDirection,
    pub is_banned: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserListSortBy {
    CreatedAt,
    UpdatedAt,
    Username,
    Email,
    Status,
    Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdminSortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
pub struct GetUserQuery {
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct GetUserPreferencesQuery {
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct UpdateUserPreferencesCommand {
    pub user_id: String,
    pub two_factor_enabled: Option<bool>,
    pub notifications: Option<UserNotificationPreferences>,
}

#[derive(Debug, Clone)]
pub struct AddAdminCommand {
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct RemoveAdminCommand {
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct ListAdminsQuery {
    pub page: i32,
    pub page_size: i32,
    pub search: String,
    pub sort_by: UserListSortBy,
    pub sort_direction: AdminSortDirection,
}

#[derive(Debug, Clone)]
pub struct GetUserRoomsQuery {
    pub user_id: String,
    pub page: i32,
    pub page_size: i32,
    pub status: Option<RoomStatus>,
    pub search: String,
    pub is_banned: Option<bool>,
    pub sort_by: RoomListSortBy,
    pub sort_direction: SortDirection,
}

#[derive(Debug, Clone)]
pub struct ListRoomsQuery {
    pub page: i32,
    pub page_size: i32,
    pub status: Option<RoomStatus>,
    pub search: String,
    pub creator_id: String,
    pub is_banned: Option<bool>,
    pub sort_by: RoomListSortBy,
    pub sort_direction: SortDirection,
    pub category_id: String,
    pub label_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ListRoomCategoriesQuery {
    pub include_disabled: bool,
}

#[derive(Debug, Clone)]
pub struct UpsertRoomCategoryCommand {
    pub key: String,
    pub name: String,
    pub description: String,
    pub sort_order: i32,
    pub is_enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct DeleteRoomCategoryCommand {
    pub category_id: String,
}

#[derive(Debug, Clone)]
pub struct ListRoomLabelsQuery {
    pub include_disabled: bool,
    pub category_id: String,
}

#[derive(Debug, Clone)]
pub struct UpsertRoomLabelCommand {
    pub key: String,
    pub name: String,
    pub description: String,
    pub color: String,
    pub category_id: String,
    pub sort_order: i32,
    pub is_enabled: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct DeleteRoomLabelCommand {
    pub label_id: String,
}

#[derive(Debug, Clone)]
pub struct UpdateRoomTaxonomyCommand {
    pub room_id: String,
    pub category_id: Option<String>,
    pub label_ids: Vec<String>,
    pub clear_category: bool,
}

#[derive(Debug, Clone)]
pub struct GetRoomQuery {
    pub room_id: String,
}

#[derive(Debug, Clone)]
pub struct GetRoomMembersQuery {
    pub room_id: String,
    pub page: i32,
    pub page_size: i32,
    pub search: String,
    pub role: i32,
    pub sort_by: RoomMemberListSortBy,
    pub sort_direction: AdminSortDirection,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomMemberListSortBy {
    JoinedAt,
    Username,
    Role,
}

#[derive(Debug, Clone)]
pub struct AddMemberCommand {
    pub room_id: String,
    pub user_id: String,
    pub role: i32,
    pub notify: bool,
    pub remark_name: String,
    pub display_tag: String,
}

#[derive(Debug, Clone)]
pub struct UpdateMemberRemarkNameCommand {
    pub room_id: String,
    pub user_id: String,
    pub remark_name: String,
}

#[derive(Debug, Clone)]
pub struct UpdateMemberDisplayTagCommand {
    pub room_id: String,
    pub user_id: String,
    pub display_tag: String,
}

#[derive(Debug, Clone)]
pub struct UpdateMemberPermissionsCommand {
    pub room_id: String,
    pub user_id: String,
    pub role: i32,
    pub added_permissions: u64,
    pub removed_permissions: u64,
    pub admin_added_permissions: u64,
    pub admin_removed_permissions: u64,
}

#[derive(Debug, Clone)]
pub struct KickMemberCommand {
    pub room_id: String,
    pub user_id: String,
    pub kick_cooldown_seconds: i64,
}

#[derive(Debug, Clone)]
pub struct GetRoomSettingsQuery {
    pub room_id: String,
}

#[derive(Debug, Clone)]
pub struct UpdateRoomSettingsCommand {
    pub room_id: String,
    pub settings: client_proto::RoomSettingsPatch,
    pub update_mask: synctv_proto::FieldMask,
}

#[derive(Debug, Clone)]
pub struct ResetRoomSettingsCommand {
    pub room_id: String,
}

#[derive(Debug, Clone)]
pub struct UpdateRoomPasswordCommand {
    pub room_id: String,
    pub new_password: String,
}

#[derive(Debug, Clone)]
pub struct BanRoomCommand {
    pub room_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct UnbanRoomCommand {
    pub room_id: String,
}

#[derive(Debug, Clone)]
pub struct DeleteRoomCommand {
    pub room_id: String,
}

#[derive(Debug, Clone)]
pub struct BatchBanRoomsCommand {
    pub room_ids: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct BatchDeleteRoomsCommand {
    pub room_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct StartPlaybackCommand {
    pub room_id: String,
    pub media_id: String,
    pub playlist_id: String,
    pub target: Option<client_proto::ProviderTarget>,
}

#[derive(Debug, Clone)]
pub struct UpdatePlaybackStateCommand {
    pub room_id: String,
    pub update_type: i32,
    pub playing: Option<bool>,
    pub position: Option<f64>,
    pub speed: Option<f64>,
    pub version: Option<i64>,
    pub expected_media_id: Option<String>,
    pub expected_playlist_id: Option<String>,
    pub expected_target_hash: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ListRoomStreamsQuery {
    pub room_id: String,
    pub page: i32,
    pub page_size: i32,
    pub search: String,
    pub sort_by: RoomStreamListSortBy,
    pub sort_direction: Option<AdminSortDirection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomStreamListSortBy {
    Unspecified,
    MediaId,
}

#[derive(Debug, Clone)]
pub struct ListPlaylistsQuery {
    pub room_id: String,
    pub parent_id: String,
    pub page: i32,
    pub page_size: i32,
    pub search: String,
    pub source_provider: i32,
    pub provider_instance_name: String,
    pub dynamic_only: Option<bool>,
    pub sort_by: i32,
    pub sort_direction: i32,
    pub availability: i32,
}

#[derive(Debug, Clone)]
pub struct UpdatePlaylistCommand {
    pub room_id: String,
    pub playlist_id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct MovePlaylistCommand {
    pub room_id: String,
    pub playlist_id: String,
    pub before_playlist_id: Option<String>,
    pub after_playlist_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DeletePlaylistCommand {
    pub room_id: String,
    pub playlist_id: String,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct ListMediaQuery {
    pub room_id: String,
    pub playlist_id: String,
    pub target: Option<client_proto::ProviderTarget>,
    pub page: i32,
    pub page_size: i32,
    pub search: String,
    pub source_provider: i32,
    pub provider_instance_name: String,
    pub sort_by: i32,
    pub sort_direction: i32,
    pub availability: i32,
    pub refresh: bool,
}

#[derive(Debug, Clone)]
pub struct EditMediaCommand {
    pub room_id: String,
    pub media_id: String,
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct DeleteMediaCommand {
    pub room_id: String,
    pub media_id: String,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct MoveMediaCommand {
    pub room_id: String,
    pub media_ids: Vec<String>,
    pub source_playlist_id: Option<String>,
    pub target_playlist_id: Option<String>,
    pub all_from_scope: bool,
    pub before_media_id: Option<String>,
    pub after_media_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct KickStreamCommand {
    pub room_id: String,
    pub media_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct GetSettingsQuery;

#[derive(Debug, Clone)]
pub struct UpdateSettingsCommand {
    pub settings: admin_proto::RuntimeSettingsPatch,
    pub update_mask: synctv_proto::FieldMask,
}

#[derive(Debug, Clone)]
pub struct SendTestEmailCommand {
    pub to: String,
}

#[derive(Debug, Clone)]
pub struct GetServiceStateQuery;

#[derive(Debug, Clone)]
pub struct ListActiveStreamsQuery {
    pub page: i32,
    pub page_size: i32,
    pub room_id: String,
    pub user_id: String,
    pub node_id: String,
    pub search: String,
    pub sort_by: i32,
    pub sort_direction: i32,
}

#[derive(Debug, Clone)]
pub struct BatchBanUsersCommand {
    pub user_ids: Vec<String>,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct BatchDeleteUsersCommand {
    pub user_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DeleteUserCommand {
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct CreateUserCommand {
    pub username: String,
    pub email: String,
    pub role: UserRole,
    pub status: UserStatus,
    pub password: String,
}

#[derive(Debug, Clone)]
pub struct BanUserCommand {
    pub user_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct UnbanUserCommand {
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct UpdateUserRoleCommand {
    pub user_id: String,
    pub role: UserRole,
}

#[derive(Debug, Clone)]
pub struct SetUserPasswordCommand {
    pub user_id: String,
    pub password: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct UpdateUserUsernameCommand {
    pub user_id: String,
    pub new_username: String,
}

#[derive(Debug, Clone)]
pub struct ListUserRegistrationReviewsQuery {
    pub page: i32,
    pub page_size: i32,
    pub status: Option<ReviewStatus>,
    pub search: String,
}

#[derive(Debug, Clone)]
pub struct ApproveUserRegistrationReviewCommand {
    pub request_id: String,
}

#[derive(Debug, Clone)]
pub struct RejectUserRegistrationReviewCommand {
    pub request_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ListRoomCreationReviewsQuery {
    pub page: i32,
    pub page_size: i32,
    pub status: Option<ReviewStatus>,
    pub requested_by: String,
    pub search: String,
}

#[derive(Debug, Clone)]
pub struct ApproveRoomCreationReviewCommand {
    pub request_id: String,
}

#[derive(Debug, Clone)]
pub struct RejectRoomCreationReviewCommand {
    pub request_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ListRoomJoinReviewsQuery {
    pub page: i32,
    pub page_size: i32,
    pub status: Option<ReviewStatus>,
    pub room_id: String,
    pub user_id: String,
}

#[derive(Debug, Clone)]
pub struct ApproveRoomJoinReviewCommand {
    pub request_id: String,
}

#[derive(Debug, Clone)]
pub struct RejectRoomJoinReviewCommand {
    pub request_id: String,
    pub reason: String,
}

#[derive(Debug, Clone)]
pub struct ListBanRecordsQuery {
    pub page: i32,
    pub page_size: i32,
    pub target_type: Option<BanRecordTargetType>,
    pub active: Option<bool>,
    pub user_id: String,
    pub room_id: String,
}

#[tonic::async_trait]
pub trait AdminRuntime: Send + Sync {
    async fn list_users(
        &self,
        query: ListUsersQuery,
    ) -> Result<admin_proto::ListUsersResponse, RuntimeError>;

    async fn get_user(&self, query: GetUserQuery) -> Result<admin_proto::AdminUser, RuntimeError>;

    async fn get_user_preferences(
        &self,
        query: GetUserPreferencesQuery,
    ) -> Result<admin_proto::GetUserPreferencesResponse, RuntimeError>;

    async fn update_user_preferences(
        &self,
        command: UpdateUserPreferencesCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::UpdateUserPreferencesResponse, RuntimeError>;

    async fn add_admin(
        &self,
        command: AddAdminCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError>;

    async fn remove_admin(
        &self,
        command: RemoveAdminCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RemoveAdminResponse, RuntimeError>;

    async fn list_admins(
        &self,
        query: ListAdminsQuery,
    ) -> Result<admin_proto::ListAdminsResponse, RuntimeError>;

    async fn create_user(
        &self,
        command: CreateUserCommand,
        caller_role: UserRole,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError>;

    async fn delete_user(
        &self,
        command: DeleteUserCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::DeleteUserResponse, RuntimeError>;

    async fn ban_user(
        &self,
        command: BanUserCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError>;

    async fn unban_user(
        &self,
        command: UnbanUserCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError>;

    async fn list_user_registration_reviews(
        &self,
        query: ListUserRegistrationReviewsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListUserRegistrationReviewsResponse, RuntimeError>;

    async fn approve_user_registration_review(
        &self,
        command: ApproveUserRegistrationReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::ApproveUserRegistrationReviewResponse, RuntimeError>;

    async fn reject_user_registration_review(
        &self,
        command: RejectUserRegistrationReviewCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::UserRegistrationReview, RuntimeError>;

    async fn list_room_creation_reviews(
        &self,
        query: ListRoomCreationReviewsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListRoomCreationReviewsResponse, RuntimeError>;

    async fn approve_room_creation_review(
        &self,
        command: ApproveRoomCreationReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::ApproveRoomCreationReviewResponse, RuntimeError>;

    async fn reject_room_creation_review(
        &self,
        command: RejectRoomCreationReviewCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::RoomCreationReview, RuntimeError>;

    async fn list_room_join_reviews(
        &self,
        query: ListRoomJoinReviewsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListRoomJoinReviewsResponse, RuntimeError>;

    async fn approve_room_join_review(
        &self,
        command: ApproveRoomJoinReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::ApproveRoomJoinReviewResponse, RuntimeError>;

    async fn reject_room_join_review(
        &self,
        command: RejectRoomJoinReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RoomJoinReview, RuntimeError>;

    async fn list_ban_records(
        &self,
        query: ListBanRecordsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListBanRecordsResponse, RuntimeError>;

    async fn update_user_role(
        &self,
        command: UpdateUserRoleCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError>;

    async fn set_user_password(
        &self,
        command: SetUserPasswordCommand,
        caller_user_id: UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::SetUserPasswordResponse, RuntimeError>;

    async fn update_user_username(
        &self,
        command: UpdateUserUsernameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError>;

    async fn get_user_rooms(
        &self,
        query: GetUserRoomsQuery,
    ) -> Result<admin_proto::GetUserRoomsResponse, RuntimeError>;

    async fn list_rooms(
        &self,
        query: ListRoomsQuery,
    ) -> Result<admin_proto::ListRoomsResponse, RuntimeError>;

    async fn list_room_categories(
        &self,
        query: ListRoomCategoriesQuery,
    ) -> Result<admin_proto::ListRoomCategoriesResponse, RuntimeError>;

    async fn upsert_room_category(
        &self,
        command: UpsertRoomCategoryCommand,
    ) -> Result<client_proto::RoomCategory, RuntimeError>;

    async fn delete_room_category(
        &self,
        command: DeleteRoomCategoryCommand,
    ) -> Result<admin_proto::DeleteRoomCategoryResponse, RuntimeError>;

    async fn list_room_labels(
        &self,
        query: ListRoomLabelsQuery,
    ) -> Result<admin_proto::ListRoomLabelsResponse, RuntimeError>;

    async fn upsert_room_label(
        &self,
        command: UpsertRoomLabelCommand,
    ) -> Result<client_proto::RoomLabel, RuntimeError>;

    async fn delete_room_label(
        &self,
        command: DeleteRoomLabelCommand,
    ) -> Result<admin_proto::DeleteRoomLabelResponse, RuntimeError>;

    async fn update_room_taxonomy(
        &self,
        command: UpdateRoomTaxonomyCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::Room, RuntimeError>;

    async fn get_room(&self, query: GetRoomQuery) -> Result<admin_proto::Room, RuntimeError>;

    async fn get_room_members(
        &self,
        query: GetRoomMembersQuery,
    ) -> Result<admin_proto::GetRoomMembersResponse, RuntimeError>;

    async fn add_member(
        &self,
        command: AddMemberCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError>;

    async fn update_member_remark_name(
        &self,
        command: UpdateMemberRemarkNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError>;

    async fn update_member_display_tag(
        &self,
        command: UpdateMemberDisplayTagCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError>;

    async fn update_member_permissions(
        &self,
        command: UpdateMemberPermissionsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError>;

    async fn kick_member(
        &self,
        command: KickMemberCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::KickMemberResponse, RuntimeError>;

    async fn get_room_settings(
        &self,
        query: GetRoomSettingsQuery,
    ) -> Result<admin_proto::GetRoomSettingsResponse, RuntimeError>;

    async fn update_room_settings(
        &self,
        command: UpdateRoomSettingsCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::Room, RuntimeError>;

    async fn reset_room_settings(
        &self,
        command: ResetRoomSettingsCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::Room, RuntimeError>;

    async fn update_room_password(
        &self,
        command: UpdateRoomPasswordCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::UpdateRoomPasswordResponse, RuntimeError>;

    async fn ban_room(
        &self,
        command: BanRoomCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::Room, RuntimeError>;

    async fn unban_room(
        &self,
        command: UnbanRoomCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::Room, RuntimeError>;

    async fn delete_room(
        &self,
        command: DeleteRoomCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::DeleteRoomResponse, RuntimeError>;

    async fn batch_ban_rooms(
        &self,
        command: BatchBanRoomsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchBanRoomsResponse, RuntimeError>;

    async fn batch_delete_rooms(
        &self,
        command: BatchDeleteRoomsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchDeleteRoomsResponse, RuntimeError>;

    async fn start_playback(
        &self,
        command: StartPlaybackCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<client_proto::StartPlaybackResponse, RuntimeError>;

    async fn stop_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<client_proto::StopPlaybackResponse, RuntimeError>;

    async fn get_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        playback_client_profile: Option<client_proto::PlaybackClientProfile>,
    ) -> Result<client_proto::GetPlaybackResponse, RuntimeError>;

    async fn update_playback_state(
        &self,
        command: UpdatePlaybackStateCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<client_proto::PlaybackState, RuntimeError>;

    async fn create_publish_key_for_actor(
        &self,
        room_id: &str,
        media_id: &str,
        actor_user_id: &UserId,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<rtmp_proto::CreatePublishKeyResponse, RuntimeError>;

    async fn get_stream_info(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<rtmp_proto::GetStreamInfoResponse, RuntimeError>;

    async fn list_room_streams(
        &self,
        query: ListRoomStreamsQuery,
    ) -> Result<client_proto::ListRoomStreamsResponse, RuntimeError>;

    async fn kick_stream(
        &self,
        command: KickStreamCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<(), RuntimeError>;

    async fn list_playlists(
        &self,
        query: ListPlaylistsQuery,
        admin_user_id: &UserId,
    ) -> Result<client_proto::ListPlaylistsResponse, RuntimeError>;

    async fn get_playlist(
        &self,
        room_id: &str,
        playlist_id: &str,
        admin_user_id: &UserId,
    ) -> Result<client_proto::GetPlaylistResponse, RuntimeError>;

    async fn update_playlist(
        &self,
        command: UpdatePlaylistCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::Playlist, RuntimeError>;

    async fn move_playlist(
        &self,
        command: MovePlaylistCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::Playlist, RuntimeError>;

    async fn delete_playlist(
        &self,
        command: DeletePlaylistCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::DeletePlaylistResponse, RuntimeError>;

    async fn list_media(
        &self,
        query: ListMediaQuery,
        admin_user_id: &UserId,
    ) -> Result<client_proto::ListPlaylistItemsResponse, RuntimeError>;

    async fn edit_media(
        &self,
        command: EditMediaCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::Media, RuntimeError>;

    async fn delete_media(
        &self,
        command: DeleteMediaCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::DeleteMediaResponse, RuntimeError>;

    async fn move_media(
        &self,
        command: MoveMediaCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::MoveMediaResponse, RuntimeError>;

    async fn get_settings(
        &self,
        query: GetSettingsQuery,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RuntimeSettings, RuntimeError>;

    async fn update_settings(
        &self,
        command: UpdateSettingsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RuntimeSettings, RuntimeError>;

    async fn send_test_email(
        &self,
        command: SendTestEmailCommand,
    ) -> Result<admin_proto::SendTestEmailResponse, RuntimeError>;

    async fn get_service_state(
        &self,
        query: GetServiceStateQuery,
    ) -> Result<admin_proto::GetServiceStateResponse, RuntimeError>;

    async fn list_active_streams(
        &self,
        query: ListActiveStreamsQuery,
    ) -> Result<admin_proto::ListActiveStreamsResponse, RuntimeError>;

    async fn batch_ban_users(
        &self,
        command: BatchBanUsersCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchBanUsersResponse, RuntimeError>;

    async fn batch_delete_users(
        &self,
        command: BatchDeleteUsersCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchDeleteUsersResponse, RuntimeError>;
}
