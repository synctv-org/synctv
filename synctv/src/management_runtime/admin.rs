use std::sync::Arc;

use synctv_core::models::{
    ReviewStatus, RoomListSortBy, RoomStatus, SortDirection, UserId, UserRole, UserStatus,
};
use synctv_management::admin_runtime::{
    AddAdminCommand, AddMemberCommand, AdminRuntime, AdminSortDirection,
    ApproveRoomCreationReviewCommand, ApproveRoomJoinReviewCommand,
    ApproveUserRegistrationReviewCommand, BanRoomCommand, BanUserCommand, BatchBanRoomsCommand,
    BatchBanUsersCommand, BatchDeleteRoomsCommand, BatchDeleteUsersCommand, CreateUserCommand,
    DeleteMediaCommand, DeletePlaylistCommand, DeleteRoomCategoryCommand, DeleteRoomCommand,
    DeleteRoomLabelCommand, DeleteUserCommand, EditMediaCommand, GetRoomMembersQuery, GetRoomQuery,
    GetRoomSettingsQuery, GetServiceStateQuery, GetSettingsQuery, GetUserPreferencesQuery,
    GetUserQuery, GetUserRoomsQuery, KickMemberCommand, KickStreamCommand, ListActiveStreamsQuery,
    ListAdminsQuery, ListBanRecordsQuery, ListMediaQuery, ListPlaylistsQuery,
    ListRoomCategoriesQuery, ListRoomCreationReviewsQuery, ListRoomJoinReviewsQuery,
    ListRoomLabelsQuery, ListRoomStreamsQuery, ListRoomsQuery, ListUserRegistrationReviewsQuery,
    ListUsersQuery, MoveMediaCommand, MovePlaylistCommand, RejectRoomCreationReviewCommand,
    RejectRoomJoinReviewCommand, RejectUserRegistrationReviewCommand, RemoveAdminCommand,
    ResetRoomSettingsCommand, RoomMemberListSortBy, RoomStreamListSortBy, SendTestEmailCommand,
    SetUserPasswordCommand, StartPlaybackCommand, UnbanRoomCommand, UnbanUserCommand,
    UpdateMemberDisplayTagCommand, UpdateMemberPermissionsCommand, UpdateMemberRemarkNameCommand,
    UpdatePlaybackStateCommand, UpdatePlaylistCommand, UpdateRoomPasswordCommand,
    UpdateRoomSettingsCommand, UpdateRoomTaxonomyCommand, UpdateSettingsCommand,
    UpdateUserPreferencesCommand, UpdateUserRoleCommand, UpdateUserUsernameCommand,
    UpsertRoomCategoryCommand, UpsertRoomLabelCommand, UserListSortBy,
};
use synctv_management::request_context::RequestContext;
use synctv_management::runtime_error::RuntimeError;
use synctv_proto::{
    admin as admin_proto, client as client_proto, common as common_proto,
    providers::rtmp as rtmp_proto,
};

use super::map_runtime_error;

pub(crate) struct ManagementAdminRuntime {
    inner: Arc<synctv_api::AdminApiImpl>,
}

impl ManagementAdminRuntime {
    pub(crate) fn new(inner: Arc<synctv_api::AdminApiImpl>) -> Self {
        Self { inner }
    }
}

fn user_list_sort_by_to_proto(sort_by: UserListSortBy) -> i32 {
    match sort_by {
        UserListSortBy::CreatedAt => admin_proto::UserListSortBy::CreatedAt as i32,
        UserListSortBy::UpdatedAt => admin_proto::UserListSortBy::UpdatedAt as i32,
        UserListSortBy::Username => admin_proto::UserListSortBy::Username as i32,
        UserListSortBy::Email => admin_proto::UserListSortBy::Email as i32,
        UserListSortBy::Status => admin_proto::UserListSortBy::Status as i32,
        UserListSortBy::Role => admin_proto::UserListSortBy::Role as i32,
    }
}

fn admin_sort_direction_to_proto(sort_direction: AdminSortDirection) -> i32 {
    match sort_direction {
        AdminSortDirection::Asc => admin_proto::SortDirection::Asc as i32,
        AdminSortDirection::Desc => admin_proto::SortDirection::Desc as i32,
    }
}

fn user_role_to_proto(role: UserRole) -> i32 {
    match role {
        UserRole::User => common_proto::UserRole::User as i32,
        UserRole::Admin => common_proto::UserRole::Admin as i32,
        UserRole::Root => common_proto::UserRole::Root as i32,
    }
}

fn optional_user_role_to_proto(role: Option<UserRole>) -> i32 {
    role.map_or(
        common_proto::UserRole::Unspecified as i32,
        user_role_to_proto,
    )
}

fn user_status_to_proto(status: Option<UserStatus>) -> i32 {
    match status {
        Some(UserStatus::Active) => common_proto::UserStatus::Active as i32,
        Some(UserStatus::Banned) => common_proto::UserStatus::Banned as i32,
        None => common_proto::UserStatus::Unspecified as i32,
    }
}

fn user_notifications_to_proto(
    value: &synctv_core::models::UserNotificationPreferences,
) -> client_proto::UserNotificationPreferences {
    client_proto::UserNotificationPreferences {
        room_invitation_in_app: value.room_invitation_in_app,
        room_event_in_app: value.room_event_in_app,
        system_announcement_in_app: value.system_announcement_in_app,
        room_invitation_email: value.room_invitation_email,
        room_event_email: value.room_event_email,
        system_announcement_email: value.system_announcement_email,
    }
}

fn review_status_to_proto(status: Option<ReviewStatus>) -> i32 {
    match status {
        Some(ReviewStatus::Pending) => common_proto::ReviewStatus::Pending as i32,
        Some(ReviewStatus::Approved) => common_proto::ReviewStatus::Approved as i32,
        Some(ReviewStatus::Rejected) => common_proto::ReviewStatus::Rejected as i32,
        None => common_proto::ReviewStatus::Unspecified as i32,
    }
}

fn room_status_to_proto(status: Option<RoomStatus>) -> i32 {
    match status {
        Some(RoomStatus::Active) => common_proto::RoomStatus::Active as i32,
        Some(RoomStatus::Closed) => common_proto::RoomStatus::Closed as i32,
        None => common_proto::RoomStatus::Unspecified as i32,
    }
}

fn room_list_sort_by_to_proto(sort_by: RoomListSortBy) -> i32 {
    match sort_by {
        RoomListSortBy::CreatedAt => admin_proto::RoomListSortBy::CreatedAt as i32,
        RoomListSortBy::UpdatedAt => admin_proto::RoomListSortBy::UpdatedAt as i32,
        RoomListSortBy::LastActivityAt => admin_proto::RoomListSortBy::LastActivityAt as i32,
        RoomListSortBy::Name => admin_proto::RoomListSortBy::Name as i32,
    }
}

fn room_member_list_sort_by_to_proto(sort_by: RoomMemberListSortBy) -> i32 {
    match sort_by {
        RoomMemberListSortBy::JoinedAt => admin_proto::RoomMemberListSortBy::JoinedAt as i32,
        RoomMemberListSortBy::Username => admin_proto::RoomMemberListSortBy::Username as i32,
        RoomMemberListSortBy::Role => admin_proto::RoomMemberListSortBy::Role as i32,
    }
}

fn room_stream_list_sort_by_to_proto(sort_by: RoomStreamListSortBy) -> i32 {
    match sort_by {
        RoomStreamListSortBy::Unspecified => client_proto::RoomStreamListSortBy::Unspecified as i32,
        RoomStreamListSortBy::MediaId => client_proto::RoomStreamListSortBy::MediaId as i32,
    }
}

fn optional_admin_sort_direction_to_client_proto(
    sort_direction: Option<AdminSortDirection>,
) -> i32 {
    match sort_direction {
        Some(AdminSortDirection::Asc) => client_proto::SortDirection::Asc as i32,
        Some(AdminSortDirection::Desc) => client_proto::SortDirection::Desc as i32,
        None => client_proto::SortDirection::Unspecified as i32,
    }
}

fn api_request_context(ctx: &RequestContext) -> synctv_api::AdminRequestContext {
    // Management owns the public runtime contract; API context conversion
    // stays in this startup bridge while synctv-api has its own context type.
    synctv_api::AdminRequestContext {
        ip_address: ctx.ip_address.clone(),
        user_agent: ctx.user_agent.clone(),
    }
}

fn optional_trimmed(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn ban_record_target_type_to_proto(
    target_type: Option<synctv_core::service::BanRecordTargetType>,
) -> i32 {
    match target_type {
        None => admin_proto::BanTargetType::Unspecified as i32,
        Some(synctv_core::service::BanRecordTargetType::User) => {
            admin_proto::BanTargetType::User as i32
        }
        Some(synctv_core::service::BanRecordTargetType::Room) => {
            admin_proto::BanTargetType::Room as i32
        }
    }
}

fn core_sort_direction_to_admin_proto(sort_direction: SortDirection) -> i32 {
    match sort_direction {
        SortDirection::Asc => admin_proto::SortDirection::Asc as i32,
        SortDirection::Desc => admin_proto::SortDirection::Desc as i32,
    }
}

#[tonic::async_trait]
impl AdminRuntime for ManagementAdminRuntime {
    async fn list_users(
        &self,
        query: ListUsersQuery,
    ) -> Result<admin_proto::ListUsersResponse, RuntimeError> {
        let req = admin_proto::ListUsersRequest {
            page: query.page,
            page_size: query.page_size,
            search: query.search,
            status: user_status_to_proto(query.status),
            role: optional_user_role_to_proto(query.role),
            is_banned: query.is_banned,
            sort_by: user_list_sort_by_to_proto(query.sort_by),
            sort_direction: admin_sort_direction_to_proto(query.sort_direction),
        };
        self.inner
            .list_users(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_user(&self, query: GetUserQuery) -> Result<admin_proto::AdminUser, RuntimeError> {
        self.inner
            .get_user(admin_proto::GetUserRequest {
                user_id: query.user_id,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_user_preferences(
        &self,
        query: GetUserPreferencesQuery,
    ) -> Result<admin_proto::GetUserPreferencesResponse, RuntimeError> {
        self.inner
            .get_user_preferences(admin_proto::GetUserPreferencesRequest {
                user_id: query.user_id,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_user_preferences(
        &self,
        command: UpdateUserPreferencesCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::UpdateUserPreferencesResponse, RuntimeError> {
        self.inner
            .update_user_preferences(
                admin_proto::UpdateUserPreferencesRequest {
                    user_id: command.user_id,
                    two_factor_enabled: command.two_factor_enabled,
                    notifications: command
                        .notifications
                        .as_ref()
                        .map(user_notifications_to_proto),
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn add_admin(
        &self,
        command: AddAdminCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError> {
        self.inner
            .add_admin(
                admin_proto::AddAdminRequest {
                    user_id: command.user_id,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn remove_admin(
        &self,
        command: RemoveAdminCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RemoveAdminResponse, RuntimeError> {
        self.inner
            .remove_admin(
                admin_proto::RemoveAdminRequest {
                    user_id: command.user_id,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_admins(
        &self,
        query: ListAdminsQuery,
    ) -> Result<admin_proto::ListAdminsResponse, RuntimeError> {
        let req = admin_proto::ListAdminsRequest {
            page: query.page,
            page_size: query.page_size,
            search: query.search,
            sort_by: user_list_sort_by_to_proto(query.sort_by),
            sort_direction: admin_sort_direction_to_proto(query.sort_direction),
        };
        self.inner
            .list_admins(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn create_user(
        &self,
        command: CreateUserCommand,
        caller_role: UserRole,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError> {
        self.inner
            .create_user(
                admin_proto::CreateUserRequest {
                    username: command.username,
                    email: command.email,
                    role: user_role_to_proto(command.role),
                    status: user_status_to_proto(Some(command.status)),
                    password: command.password,
                },
                caller_role,
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_user(
        &self,
        command: DeleteUserCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::DeleteUserResponse, RuntimeError> {
        self.inner
            .delete_user(
                admin_proto::DeleteUserRequest {
                    user_id: command.user_id,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn ban_user(
        &self,
        command: BanUserCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError> {
        self.inner
            .ban_user(
                admin_proto::BanUserRequest {
                    user_id: command.user_id,
                    reason: command.reason,
                },
                admin_user_id,
                caller_role,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn unban_user(
        &self,
        command: UnbanUserCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError> {
        self.inner
            .unban_user(
                admin_proto::UnbanUserRequest {
                    user_id: command.user_id,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_user_registration_reviews(
        &self,
        query: ListUserRegistrationReviewsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListUserRegistrationReviewsResponse, RuntimeError> {
        self.inner
            .list_user_registration_reviews(
                admin_proto::ListUserRegistrationReviewsRequest {
                    page: query.page,
                    page_size: query.page_size,
                    status: review_status_to_proto(query.status),
                    search: query.search,
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn approve_user_registration_review(
        &self,
        command: ApproveUserRegistrationReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::ApproveUserRegistrationReviewResponse, RuntimeError> {
        self.inner
            .approve_user_registration_review(
                admin_proto::ApproveUserRegistrationReviewRequest {
                    request_id: command.request_id,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn reject_user_registration_review(
        &self,
        command: RejectUserRegistrationReviewCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::UserRegistrationReview, RuntimeError> {
        self.inner
            .reject_user_registration_review(
                admin_proto::RejectUserRegistrationReviewRequest {
                    request_id: command.request_id,
                    reason: command.reason,
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_room_creation_reviews(
        &self,
        query: ListRoomCreationReviewsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListRoomCreationReviewsResponse, RuntimeError> {
        self.inner
            .list_room_creation_reviews(
                admin_proto::ListRoomCreationReviewsRequest {
                    page: query.page,
                    page_size: query.page_size,
                    status: review_status_to_proto(query.status),
                    requested_by: query.requested_by,
                    search: query.search,
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn approve_room_creation_review(
        &self,
        command: ApproveRoomCreationReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::ApproveRoomCreationReviewResponse, RuntimeError> {
        self.inner
            .approve_room_creation_review(
                admin_proto::ApproveRoomCreationReviewRequest {
                    request_id: command.request_id,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn reject_room_creation_review(
        &self,
        command: RejectRoomCreationReviewCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::RoomCreationReview, RuntimeError> {
        self.inner
            .reject_room_creation_review(
                admin_proto::RejectRoomCreationReviewRequest {
                    request_id: command.request_id,
                    reason: command.reason,
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_room_join_reviews(
        &self,
        query: ListRoomJoinReviewsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListRoomJoinReviewsResponse, RuntimeError> {
        self.inner
            .list_room_join_reviews(
                admin_proto::ListRoomJoinReviewsRequest {
                    page: query.page,
                    page_size: query.page_size,
                    status: review_status_to_proto(query.status),
                    room_id: query.room_id,
                    user_id: query.user_id,
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn approve_room_join_review(
        &self,
        command: ApproveRoomJoinReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::ApproveRoomJoinReviewResponse, RuntimeError> {
        self.inner
            .approve_room_join_review(
                admin_proto::ApproveRoomJoinReviewRequest {
                    request_id: command.request_id,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn reject_room_join_review(
        &self,
        command: RejectRoomJoinReviewCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RoomJoinReview, RuntimeError> {
        self.inner
            .reject_room_join_review(
                admin_proto::RejectRoomJoinReviewRequest {
                    request_id: command.request_id,
                    reason: command.reason,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_ban_records(
        &self,
        query: ListBanRecordsQuery,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::ListBanRecordsResponse, RuntimeError> {
        self.inner
            .list_ban_records(
                admin_proto::ListBanRecordsRequest {
                    page: query.page,
                    page_size: query.page_size,
                    target_type: ban_record_target_type_to_proto(query.target_type),
                    active: query.active,
                    user_id: optional_trimmed(&query.user_id).unwrap_or_default(),
                    room_id: optional_trimmed(&query.room_id).unwrap_or_default(),
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_user_role(
        &self,
        command: UpdateUserRoleCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError> {
        self.inner
            .update_user_role(
                admin_proto::UpdateUserRoleRequest {
                    user_id: command.user_id,
                    role: user_role_to_proto(command.role),
                },
                admin_user_id,
                caller_role,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn set_user_password(
        &self,
        command: SetUserPasswordCommand,
        caller_user_id: UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::SetUserPasswordResponse, RuntimeError> {
        self.inner
            .set_user_password(
                admin_proto::SetUserPasswordRequest {
                    user_id: command.user_id,
                    password: command.password,
                    reason: command.reason,
                },
                caller_user_id,
                caller_role,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_user_username(
        &self,
        command: UpdateUserUsernameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::AdminUser, RuntimeError> {
        self.inner
            .update_user_username(
                admin_proto::UpdateUserUsernameRequest {
                    user_id: command.user_id,
                    new_username: command.new_username,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_user_rooms(
        &self,
        query: GetUserRoomsQuery,
    ) -> Result<admin_proto::GetUserRoomsResponse, RuntimeError> {
        let req = admin_proto::GetUserRoomsRequest {
            user_id: query.user_id,
            page: query.page,
            page_size: query.page_size,
            status: room_status_to_proto(query.status),
            search: query.search,
            is_banned: query.is_banned,
            sort_by: room_list_sort_by_to_proto(query.sort_by),
            sort_direction: core_sort_direction_to_admin_proto(query.sort_direction),
        };
        self.inner
            .get_user_rooms(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_rooms(
        &self,
        query: ListRoomsQuery,
    ) -> Result<admin_proto::ListRoomsResponse, RuntimeError> {
        let req = admin_proto::ListRoomsRequest {
            page: query.page,
            page_size: query.page_size,
            status: room_status_to_proto(query.status),
            search: query.search,
            creator_id: query.creator_id,
            is_banned: query.is_banned,
            sort_by: room_list_sort_by_to_proto(query.sort_by),
            sort_direction: core_sort_direction_to_admin_proto(query.sort_direction),
            category_id: query.category_id,
            label_ids: query.label_ids,
        };
        self.inner
            .list_rooms(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_room_categories(
        &self,
        query: ListRoomCategoriesQuery,
    ) -> Result<admin_proto::ListRoomCategoriesResponse, RuntimeError> {
        self.inner
            .list_room_categories(admin_proto::ListRoomCategoriesRequest {
                include_disabled: query.include_disabled,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn upsert_room_category(
        &self,
        command: UpsertRoomCategoryCommand,
    ) -> Result<client_proto::RoomCategory, RuntimeError> {
        self.inner
            .upsert_room_category(admin_proto::UpsertRoomCategoryRequest {
                key: command.key,
                name: command.name,
                description: command.description,
                sort_order: command.sort_order,
                is_enabled: command.is_enabled,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_room_category(
        &self,
        command: DeleteRoomCategoryCommand,
    ) -> Result<admin_proto::DeleteRoomCategoryResponse, RuntimeError> {
        self.inner
            .delete_room_category(admin_proto::DeleteRoomCategoryRequest {
                category_id: command.category_id,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_room_labels(
        &self,
        query: ListRoomLabelsQuery,
    ) -> Result<admin_proto::ListRoomLabelsResponse, RuntimeError> {
        self.inner
            .list_room_labels(admin_proto::ListRoomLabelsRequest {
                include_disabled: query.include_disabled,
                category_id: query.category_id,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn upsert_room_label(
        &self,
        command: UpsertRoomLabelCommand,
    ) -> Result<client_proto::RoomLabel, RuntimeError> {
        self.inner
            .upsert_room_label(admin_proto::UpsertRoomLabelRequest {
                key: command.key,
                name: command.name,
                description: command.description,
                color: command.color,
                category_id: command.category_id,
                sort_order: command.sort_order,
                is_enabled: command.is_enabled,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_room_label(
        &self,
        command: DeleteRoomLabelCommand,
    ) -> Result<admin_proto::DeleteRoomLabelResponse, RuntimeError> {
        self.inner
            .delete_room_label(admin_proto::DeleteRoomLabelRequest {
                label_id: command.label_id,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_room_taxonomy(
        &self,
        command: UpdateRoomTaxonomyCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::Room, RuntimeError> {
        self.inner
            .update_room_taxonomy(
                admin_proto::UpdateRoomTaxonomyRequest {
                    room_id: command.room_id,
                    category_id: command.category_id,
                    clear_category: command.clear_category,
                    label_ids: command.label_ids,
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_room(&self, query: GetRoomQuery) -> Result<admin_proto::Room, RuntimeError> {
        let req = admin_proto::GetRoomRequest {
            room_id: query.room_id,
        };
        self.inner
            .get_room(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_room_members(
        &self,
        query: GetRoomMembersQuery,
    ) -> Result<admin_proto::GetRoomMembersResponse, RuntimeError> {
        let req = admin_proto::GetRoomMembersRequest {
            room_id: query.room_id,
            page: query.page,
            page_size: query.page_size,
            search: query.search,
            role: query.role,
            sort_by: room_member_list_sort_by_to_proto(query.sort_by),
            sort_direction: admin_sort_direction_to_proto(query.sort_direction),
        };
        self.inner
            .get_room_members(req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn add_member(
        &self,
        command: AddMemberCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError> {
        let req = admin_proto::AddMemberRequest {
            room_id: command.room_id,
            user_id: command.user_id,
            role: command.role,
            notify: command.notify,
            remark_name: command.remark_name,
            display_tag: command.display_tag,
        };
        self.inner
            .add_member(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_member_remark_name(
        &self,
        command: UpdateMemberRemarkNameCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError> {
        let req = admin_proto::UpdateMemberRemarkNameRequest {
            room_id: command.room_id,
            user_id: command.user_id,
            remark_name: command.remark_name,
        };
        self.inner
            .update_member_remark_name(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_member_display_tag(
        &self,
        command: UpdateMemberDisplayTagCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError> {
        let req = admin_proto::UpdateMemberDisplayTagRequest {
            room_id: command.room_id,
            user_id: command.user_id,
            display_tag: command.display_tag,
        };
        self.inner
            .update_member_display_tag(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_member_permissions(
        &self,
        command: UpdateMemberPermissionsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<common_proto::RoomMember, RuntimeError> {
        let req = admin_proto::UpdateMemberPermissionsRequest {
            room_id: command.room_id,
            user_id: command.user_id,
            role: command.role,
            added_permissions: command.added_permissions,
            removed_permissions: command.removed_permissions,
            admin_added_permissions: command.admin_added_permissions,
            admin_removed_permissions: command.admin_removed_permissions,
        };
        self.inner
            .update_member_permissions(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn kick_member(
        &self,
        command: KickMemberCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::KickMemberResponse, RuntimeError> {
        let req = admin_proto::KickMemberRequest {
            room_id: command.room_id,
            user_id: command.user_id,
            kick_cooldown_seconds: command.kick_cooldown_seconds,
        };
        self.inner
            .kick_member(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_room_settings(
        &self,
        query: GetRoomSettingsQuery,
    ) -> Result<admin_proto::GetRoomSettingsResponse, RuntimeError> {
        self.inner
            .get_room_settings(admin_proto::GetRoomSettingsRequest {
                room_id: query.room_id,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_room_settings(
        &self,
        command: UpdateRoomSettingsCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::Room, RuntimeError> {
        self.inner
            .update_room_settings(
                admin_proto::UpdateRoomSettingsRequest {
                    room_id: command.room_id,
                    settings: Some(command.settings),
                    update_mask: Some(command.update_mask),
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn reset_room_settings(
        &self,
        command: ResetRoomSettingsCommand,
        admin_user_id: &UserId,
    ) -> Result<admin_proto::Room, RuntimeError> {
        self.inner
            .reset_room_settings(
                admin_proto::ResetRoomSettingsRequest {
                    room_id: command.room_id,
                },
                admin_user_id,
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_room_password(
        &self,
        command: UpdateRoomPasswordCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::UpdateRoomPasswordResponse, RuntimeError> {
        let req = admin_proto::UpdateRoomPasswordRequest {
            room_id: command.room_id,
            new_password: command.new_password,
        };
        self.inner
            .update_room_password(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn ban_room(
        &self,
        command: BanRoomCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::Room, RuntimeError> {
        let req = admin_proto::BanRoomRequest {
            room_id: command.room_id,
            reason: command.reason,
        };
        self.inner
            .ban_room(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn unban_room(
        &self,
        command: UnbanRoomCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::Room, RuntimeError> {
        let req = admin_proto::UnbanRoomRequest {
            room_id: command.room_id,
        };
        self.inner
            .unban_room(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_room(
        &self,
        command: DeleteRoomCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::DeleteRoomResponse, RuntimeError> {
        let req = admin_proto::DeleteRoomRequest {
            room_id: command.room_id,
        };
        self.inner
            .delete_room(req, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn batch_ban_rooms(
        &self,
        command: BatchBanRoomsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchBanRoomsResponse, RuntimeError> {
        self.inner
            .batch_ban_rooms(
                admin_proto::BatchBanRoomsRequest {
                    room_ids: command.room_ids,
                    reason: command.reason,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn batch_delete_rooms(
        &self,
        command: BatchDeleteRoomsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchDeleteRoomsResponse, RuntimeError> {
        self.inner
            .batch_delete_rooms(
                admin_proto::BatchDeleteRoomsRequest {
                    room_ids: command.room_ids,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn start_playback(
        &self,
        command: StartPlaybackCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<client_proto::StartPlaybackResponse, RuntimeError> {
        let req = client_proto::StartPlaybackRequest {
            media_id: command.media_id,
            playlist_id: command.playlist_id,
            target: command.target,
        };
        self.inner
            .start_playback(
                &command.room_id,
                req,
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn stop_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<client_proto::StopPlaybackResponse, RuntimeError> {
        self.inner
            .stop_playback(room_id, admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_playback(
        &self,
        room_id: &str,
        admin_user_id: &UserId,
        playback_client_profile: Option<client_proto::PlaybackClientProfile>,
    ) -> Result<client_proto::GetPlaybackResponse, RuntimeError> {
        self.inner
            .get_playback(room_id, admin_user_id, playback_client_profile)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_playback_state(
        &self,
        command: UpdatePlaybackStateCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<client_proto::PlaybackState, RuntimeError> {
        let req = client_proto::UpdatePlaybackStateRequest {
            r#type: command.update_type,
            playing: command.playing,
            position: command.position,
            speed: command.speed,
            version: command.version,
            expected_media_id: command.expected_media_id,
            expected_playlist_id: command.expected_playlist_id,
            expected_target_hash: command.expected_target_hash,
        };
        self.inner
            .update_playback_state(
                &command.room_id,
                req,
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn create_publish_key_for_actor(
        &self,
        room_id: &str,
        media_id: &str,
        actor_user_id: &UserId,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<rtmp_proto::CreatePublishKeyResponse, RuntimeError> {
        self.inner
            .create_publish_key_for_actor(
                room_id,
                media_id,
                actor_user_id,
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_stream_info(
        &self,
        room_id: &str,
        media_id: &str,
    ) -> Result<rtmp_proto::GetStreamInfoResponse, RuntimeError> {
        self.inner
            .get_stream_info(room_id, media_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_room_streams(
        &self,
        query: ListRoomStreamsQuery,
    ) -> Result<client_proto::ListRoomStreamsResponse, RuntimeError> {
        let req = client_proto::ListRoomStreamsRequest {
            page: query.page,
            page_size: query.page_size,
            search: query.search,
            sort_by: room_stream_list_sort_by_to_proto(query.sort_by),
            sort_direction: optional_admin_sort_direction_to_client_proto(query.sort_direction),
        };
        self.inner
            .list_room_streams(&query.room_id, req)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn kick_stream(
        &self,
        command: KickStreamCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<(), RuntimeError> {
        self.inner
            .kick_stream(
                admin_proto::KickStreamRequest {
                    room_id: command.room_id,
                    media_id: command.media_id,
                    reason: command.reason,
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_playlists(
        &self,
        query: ListPlaylistsQuery,
        admin_user_id: &UserId,
    ) -> Result<client_proto::ListPlaylistsResponse, RuntimeError> {
        let req = client_proto::ListPlaylistsRequest {
            parent_id: query.parent_id,
            page: query.page,
            page_size: query.page_size,
            search: query.search,
            source_provider: query.source_provider,
            provider_instance_name: query.provider_instance_name,
            dynamic_only: query.dynamic_only,
            sort_by: query.sort_by,
            sort_direction: query.sort_direction,
            availability: query.availability,
        };
        self.inner
            .list_playlists(&query.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_playlist(
        &self,
        room_id: &str,
        playlist_id: &str,
        admin_user_id: &UserId,
    ) -> Result<client_proto::GetPlaylistResponse, RuntimeError> {
        self.inner
            .get_playlist(room_id, playlist_id, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_playlist(
        &self,
        command: UpdatePlaylistCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::Playlist, RuntimeError> {
        let req = client_proto::UpdatePlaylistRequest {
            playlist_id: command.playlist_id,
            name: command.name,
            description: command.description,
        };
        self.inner
            .update_playlist(&command.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn move_playlist(
        &self,
        command: MovePlaylistCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::Playlist, RuntimeError> {
        let req = client_proto::MovePlaylistRequest {
            playlist_id: command.playlist_id,
            anchor: command
                .before_playlist_id
                .map(client_proto::move_playlist_request::Anchor::BeforePlaylistId)
                .or_else(|| {
                    command
                        .after_playlist_id
                        .map(client_proto::move_playlist_request::Anchor::AfterPlaylistId)
                }),
        };
        self.inner
            .move_playlist(&command.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_playlist(
        &self,
        command: DeletePlaylistCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::DeletePlaylistResponse, RuntimeError> {
        let req = client_proto::DeletePlaylistRequest {
            playlist_id: command.playlist_id,
            force: command.force,
        };
        self.inner
            .delete_playlist(&command.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_media(
        &self,
        query: ListMediaQuery,
        admin_user_id: &UserId,
    ) -> Result<client_proto::ListPlaylistItemsResponse, RuntimeError> {
        let req = client_proto::ListPlaylistItemsRequest {
            playlist_id: query.playlist_id,
            target: query.target,
            pagination: query.pagination,
            page_size: query.page_size,
            search: query.search,
            source_provider: query.source_provider,
            provider_instance_name: query.provider_instance_name,
            sort_by: query.sort_by,
            sort_direction: query.sort_direction,
            availability: query.availability,
            refresh: query.refresh,
            preview_source_config: None,
        };
        self.inner
            .list_media(&query.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn edit_media(
        &self,
        command: EditMediaCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::Media, RuntimeError> {
        let req = client_proto::EditMediaRequest {
            media_id: command.media_id,
            name: command.name,
            description: command.description,
        };
        self.inner
            .edit_media(&command.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn delete_media(
        &self,
        command: DeleteMediaCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::DeleteMediaResponse, RuntimeError> {
        let req = client_proto::DeleteMediaRequest {
            media_id: command.media_id,
            force: command.force,
        };
        self.inner
            .delete_media(&command.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn move_media(
        &self,
        command: MoveMediaCommand,
        admin_user_id: &UserId,
    ) -> Result<client_proto::MoveMediaResponse, RuntimeError> {
        let req = client_proto::MoveMediaRequest {
            media_ids: command.media_ids,
            source_playlist_id: command.source_playlist_id,
            target_playlist_id: command.target_playlist_id,
            all_from_scope: command.all_from_scope,
            before_media_id: command.before_media_id,
            after_media_id: command.after_media_id,
        };
        self.inner
            .move_media(&command.room_id, req, admin_user_id)
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_settings(
        &self,
        _query: GetSettingsQuery,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RuntimeSettings, RuntimeError> {
        self.inner
            .get_settings(admin_user_id, &api_request_context(ctx))
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn update_settings(
        &self,
        command: UpdateSettingsCommand,
        admin_user_id: &UserId,
        ctx: &RequestContext,
    ) -> Result<admin_proto::RuntimeSettings, RuntimeError> {
        self.inner
            .update_settings(
                admin_proto::UpdateSettingsRequest {
                    settings: Some(command.settings),
                    update_mask: Some(command.update_mask),
                },
                admin_user_id,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn send_test_email(
        &self,
        command: SendTestEmailCommand,
    ) -> Result<admin_proto::SendTestEmailResponse, RuntimeError> {
        self.inner
            .send_test_email(admin_proto::SendTestEmailRequest { to: command.to })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn get_service_state(
        &self,
        _query: GetServiceStateQuery,
    ) -> Result<admin_proto::GetServiceStateResponse, RuntimeError> {
        self.inner
            .get_service_state()
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn list_active_streams(
        &self,
        query: ListActiveStreamsQuery,
    ) -> Result<admin_proto::ListActiveStreamsResponse, RuntimeError> {
        self.inner
            .list_active_streams(admin_proto::ListActiveStreamsRequest {
                page: query.page,
                page_size: query.page_size,
                room_id: query.room_id,
                user_id: query.user_id,
                node_id: query.node_id,
                search: query.search,
                sort_by: query.sort_by,
                sort_direction: query.sort_direction,
            })
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn batch_ban_users(
        &self,
        command: BatchBanUsersCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchBanUsersResponse, RuntimeError> {
        self.inner
            .batch_ban_users(
                admin_proto::BatchBanUsersRequest {
                    user_ids: command.user_ids,
                    reason: command.reason,
                },
                admin_user_id,
                caller_role,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }

    async fn batch_delete_users(
        &self,
        command: BatchDeleteUsersCommand,
        admin_user_id: &UserId,
        caller_role: UserRole,
        ctx: &RequestContext,
    ) -> Result<admin_proto::BatchDeleteUsersResponse, RuntimeError> {
        self.inner
            .batch_delete_users(
                admin_proto::BatchDeleteUsersRequest {
                    user_ids: command.user_ids,
                },
                admin_user_id,
                caller_role,
                &api_request_context(ctx),
            )
            .await
            .map_err(|error| map_runtime_error(&error))
    }
}
