use tonic::Status;

use crate::admin_runtime::{
    AdminSortDirection, RoomMemberListSortBy, RoomStreamListSortBy,
    UserListSortBy as RuntimeUserListSortBy,
};
use synctv_core::models::UserStatus as CoreUserStatus;
use synctv_core::models::{
    ProviderInstanceListSortBy, ReviewStatus, RoomListSortBy, RoomStatus, SortDirection, UserRole,
    UserStatus,
};
use synctv_core::service::BanRecordTargetType;
use synctv_proto::{
    admin as admin_proto, client as client_proto, common as common_proto,
    providers::common as provider_common_proto,
};

pub(crate) fn invalid_enum_value(field: &'static str, value: i32) -> Status {
    Status::invalid_argument(format!("Invalid {field}: unknown enum value {value}"))
}

pub(crate) fn map_required_user_role(role: i32) -> Result<UserRole, Status> {
    Ok(
        match common_proto::UserRole::try_from(role)
            .map_err(|_| invalid_enum_value("role", role))?
        {
            common_proto::UserRole::Root => UserRole::Root,
            common_proto::UserRole::Admin => UserRole::Admin,
            common_proto::UserRole::User => UserRole::User,
            common_proto::UserRole::Unspecified => {
                return Err(Status::invalid_argument("role is required"));
            }
        },
    )
}

pub(crate) fn map_required_user_status(status: i32) -> Result<UserStatus, Status> {
    Ok(
        match common_proto::UserStatus::try_from(status)
            .map_err(|_| invalid_enum_value("status", status))?
        {
            common_proto::UserStatus::Active => UserStatus::Active,
            common_proto::UserStatus::Banned => UserStatus::Banned,
            common_proto::UserStatus::Unspecified => {
                return Err(Status::invalid_argument("status is required"));
            }
        },
    )
}

pub(crate) fn map_review_status_filter(status: i32) -> Result<Option<ReviewStatus>, Status> {
    Ok(
        match common_proto::ReviewStatus::try_from(status)
            .map_err(|_| invalid_enum_value("status", status))?
        {
            common_proto::ReviewStatus::Unspecified => None,
            common_proto::ReviewStatus::Pending => Some(ReviewStatus::Pending),
            common_proto::ReviewStatus::Approved => Some(ReviewStatus::Approved),
            common_proto::ReviewStatus::Rejected => Some(ReviewStatus::Rejected),
        },
    )
}

pub(crate) fn map_ban_record_target_type_filter(
    target_type: i32,
) -> Result<Option<BanRecordTargetType>, Status> {
    Ok(
        match admin_proto::BanTargetType::try_from(target_type)
            .map_err(|_| invalid_enum_value("target_type", target_type))?
        {
            admin_proto::BanTargetType::Unspecified => None,
            admin_proto::BanTargetType::User => Some(BanRecordTargetType::User),
            admin_proto::BanTargetType::Room => Some(BanRecordTargetType::Room),
        },
    )
}

pub(crate) fn map_user_status_filter(status: i32) -> Result<Option<UserStatus>, Status> {
    Ok(
        match common_proto::UserStatus::try_from(status)
            .map_err(|_| invalid_enum_value("status", status))?
        {
            common_proto::UserStatus::Unspecified => None,
            common_proto::UserStatus::Active => Some(UserStatus::Active),
            common_proto::UserStatus::Banned => Some(UserStatus::Banned),
        },
    )
}

pub(crate) fn map_user_role_filter(role: i32) -> Result<Option<UserRole>, Status> {
    Ok(
        match common_proto::UserRole::try_from(role)
            .map_err(|_| invalid_enum_value("role", role))?
        {
            common_proto::UserRole::Unspecified => None,
            common_proto::UserRole::Root => Some(UserRole::Root),
            common_proto::UserRole::Admin => Some(UserRole::Admin),
            common_proto::UserRole::User => Some(UserRole::User),
        },
    )
}

pub(crate) fn user_notification_preferences_from_client_proto(
    value: client_proto::UserNotificationPreferences,
) -> synctv_core::models::UserNotificationPreferences {
    synctv_core::models::UserNotificationPreferences {
        room_invitation_in_app: value.room_invitation_in_app,
        room_event_in_app: value.room_event_in_app,
        system_announcement_in_app: value.system_announcement_in_app,
        room_invitation_email: value.room_invitation_email,
        room_event_email: value.room_event_email,
        system_announcement_email: value.system_announcement_email,
    }
}

pub(crate) fn map_room_status_filter(status: i32) -> Result<Option<RoomStatus>, Status> {
    Ok(
        match common_proto::RoomStatus::try_from(status)
            .map_err(|_| invalid_enum_value("status", status))?
        {
            common_proto::RoomStatus::Unspecified => None,
            common_proto::RoomStatus::Active => Some(RoomStatus::Active),
            common_proto::RoomStatus::Closed => Some(RoomStatus::Closed),
        },
    )
}

pub(crate) fn map_management_user_list_sort_by(
    sort_by: i32,
) -> Result<RuntimeUserListSortBy, Status> {
    let sort_by = crate::proto::UserListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::UserListSortBy::Username => RuntimeUserListSortBy::Username,
        crate::proto::UserListSortBy::Email => RuntimeUserListSortBy::Email,
        crate::proto::UserListSortBy::Status => RuntimeUserListSortBy::Status,
        crate::proto::UserListSortBy::Role => RuntimeUserListSortBy::Role,
        crate::proto::UserListSortBy::UpdatedAt => RuntimeUserListSortBy::UpdatedAt,
        crate::proto::UserListSortBy::CreatedAt | crate::proto::UserListSortBy::Unspecified => {
            RuntimeUserListSortBy::CreatedAt
        }
    })
}

pub(crate) fn map_management_room_list_sort_by(sort_by: i32) -> Result<RoomListSortBy, Status> {
    let sort_by = crate::proto::RoomListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::RoomListSortBy::Name => RoomListSortBy::Name,
        crate::proto::RoomListSortBy::UpdatedAt => RoomListSortBy::UpdatedAt,
        crate::proto::RoomListSortBy::LastActivityAt => RoomListSortBy::LastActivityAt,
        crate::proto::RoomListSortBy::CreatedAt | crate::proto::RoomListSortBy::Unspecified => {
            RoomListSortBy::CreatedAt
        }
    })
}

pub(crate) fn map_room_member_list_sort_by(sort_by: i32) -> Result<RoomMemberListSortBy, Status> {
    let sort_by = crate::proto::RoomMemberListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::RoomMemberListSortBy::Username => RoomMemberListSortBy::Username,
        crate::proto::RoomMemberListSortBy::Role => RoomMemberListSortBy::Role,
        crate::proto::RoomMemberListSortBy::JoinedAt
        | crate::proto::RoomMemberListSortBy::Unspecified => RoomMemberListSortBy::JoinedAt,
    })
}

pub(crate) fn map_room_stream_list_sort_by(sort_by: i32) -> Result<RoomStreamListSortBy, Status> {
    let sort_by = crate::proto::RoomStreamListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::RoomStreamListSortBy::MediaId => RoomStreamListSortBy::MediaId,
        crate::proto::RoomStreamListSortBy::Unspecified => RoomStreamListSortBy::Unspecified,
    })
}

pub(crate) fn map_management_sort_direction(
    sort_direction: i32,
    default: AdminSortDirection,
) -> Result<AdminSortDirection, Status> {
    let sort_direction = crate::proto::SortDirection::try_from(sort_direction)
        .map_err(|_| invalid_enum_value("sort_direction", sort_direction))?;
    Ok(match sort_direction {
        crate::proto::SortDirection::Asc => AdminSortDirection::Asc,
        crate::proto::SortDirection::Desc => AdminSortDirection::Desc,
        crate::proto::SortDirection::Unspecified => default,
    })
}

pub(crate) fn map_optional_management_sort_direction(
    sort_direction: i32,
) -> Result<Option<AdminSortDirection>, Status> {
    let sort_direction = crate::proto::SortDirection::try_from(sort_direction)
        .map_err(|_| invalid_enum_value("sort_direction", sort_direction))?;
    Ok(match sort_direction {
        crate::proto::SortDirection::Asc => Some(AdminSortDirection::Asc),
        crate::proto::SortDirection::Desc => Some(AdminSortDirection::Desc),
        crate::proto::SortDirection::Unspecified => None,
    })
}

pub(crate) fn map_management_core_sort_direction(
    sort_direction: i32,
    default: SortDirection,
) -> Result<SortDirection, Status> {
    let sort_direction = crate::proto::SortDirection::try_from(sort_direction)
        .map_err(|_| invalid_enum_value("sort_direction", sort_direction))?;
    Ok(match sort_direction {
        crate::proto::SortDirection::Asc => SortDirection::Asc,
        crate::proto::SortDirection::Desc => SortDirection::Desc,
        crate::proto::SortDirection::Unspecified => default,
    })
}

pub(crate) fn map_provider_instance_list_sort_by(
    sort_by: i32,
) -> Result<ProviderInstanceListSortBy, Status> {
    let sort_by = provider_common_proto::ProviderInstanceListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        provider_common_proto::ProviderInstanceListSortBy::Name => ProviderInstanceListSortBy::Name,
        provider_common_proto::ProviderInstanceListSortBy::Endpoint => {
            ProviderInstanceListSortBy::Endpoint
        }
        provider_common_proto::ProviderInstanceListSortBy::UpdatedAt => {
            ProviderInstanceListSortBy::UpdatedAt
        }
        provider_common_proto::ProviderInstanceListSortBy::CreatedAt
        | provider_common_proto::ProviderInstanceListSortBy::Unspecified => {
            ProviderInstanceListSortBy::CreatedAt
        }
    })
}

pub(crate) fn map_provider_instance_sort_direction(
    sort_direction: i32,
) -> Result<SortDirection, Status> {
    let sort_direction = provider_common_proto::SortDirection::try_from(sort_direction)
        .map_err(|_| invalid_enum_value("sort_direction", sort_direction))?;
    Ok(match sort_direction {
        provider_common_proto::SortDirection::Asc => SortDirection::Asc,
        provider_common_proto::SortDirection::Desc
        | provider_common_proto::SortDirection::Unspecified => SortDirection::Desc,
    })
}

pub(crate) fn validate_client_actor_user(user: &synctv_core::models::User) -> Result<(), Status> {
    if user.is_deleted() {
        return Err(Status::permission_denied(format!(
            "actor user '{}' is deleted",
            user.username
        )));
    }
    match user.status {
        CoreUserStatus::Active => {}
        CoreUserStatus::Banned => {
            return Err(Status::permission_denied(format!(
                "actor user '{}' is banned",
                user.username
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        map_management_sort_direction, map_management_user_list_sort_by,
        map_optional_management_sort_direction, map_room_stream_list_sort_by,
        validate_client_actor_user,
    };
    use crate::admin_runtime::{AdminSortDirection, UserListSortBy};
    use synctv_core::models::{SignupMethod, User, UserStatus};

    fn make_actor_user(username: &str, status: UserStatus) -> User {
        User::new_with_status(username.to_string(), SignupMethod::Email, status)
    }

    #[test]
    fn validate_client_actor_user_accepts_active_user() -> Result<(), tonic::Status> {
        let user = make_actor_user("root", UserStatus::Active);
        validate_client_actor_user(&user)?;
        Ok(())
    }

    #[test]
    fn validate_client_actor_user_rejects_banned_user_with_explicit_message() {
        let user = make_actor_user("root", UserStatus::Banned);
        let error = validate_client_actor_user(&user).expect_err("banned actor should be rejected");

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert_eq!(error.message(), "actor user 'root' is banned");
    }

    #[test]
    fn validate_client_actor_user_rejects_deleted_user_with_explicit_message() {
        let mut user = make_actor_user("root", UserStatus::Active);
        user.deleted_at = Some(user.created_at);
        let error =
            validate_client_actor_user(&user).expect_err("deleted actor should be rejected");

        assert_eq!(error.code(), tonic::Code::PermissionDenied);
        assert_eq!(error.message(), "actor user 'root' is deleted");
    }

    #[test]
    fn enum_mapping_rejects_unknown_user_sort_values() {
        let status = map_management_user_list_sort_by(99)
            .expect_err("unknown management user sort enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_by"));
    }

    #[test]
    fn enum_mapping_preserves_status_user_sort() -> Result<(), tonic::Status> {
        assert_eq!(
            map_management_user_list_sort_by(crate::proto::UserListSortBy::Status as i32)?,
            UserListSortBy::Status
        );
        Ok(())
    }

    #[test]
    fn enum_mapping_rejects_unknown_sort_direction_values() {
        let status = map_management_sort_direction(99, AdminSortDirection::Desc)
            .expect_err("unknown management sort direction enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_direction"));
    }

    #[test]
    fn enum_mapping_rejects_unknown_room_stream_sort_values() {
        let status = map_room_stream_list_sort_by(99)
            .expect_err("unknown room-stream sort enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_by"));
    }

    #[test]
    fn enum_mapping_rejects_unknown_optional_sort_direction_values() {
        let status = map_optional_management_sort_direction(99)
            .expect_err("unknown optional sort direction enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_direction"));
    }
}
