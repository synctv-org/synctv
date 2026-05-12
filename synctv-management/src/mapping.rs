use tonic::Status;

use synctv_core::models::UserStatus as CoreUserStatus;
use synctv_proto::{admin as admin_proto, common as common_proto};

pub(crate) fn invalid_enum_value(field: &'static str, value: i32) -> Status {
    Status::invalid_argument(format!("Invalid {field}: unknown enum value {value}"))
}

pub(crate) fn map_user_role(role: i32) -> Result<i32, Status> {
    let role =
        common_proto::UserRole::try_from(role).map_err(|_| invalid_enum_value("role", role))?;
    Ok(match role {
        common_proto::UserRole::User => common_proto::UserRole::User as i32,
        common_proto::UserRole::Admin => common_proto::UserRole::Admin as i32,
        common_proto::UserRole::Root => common_proto::UserRole::Root as i32,
        common_proto::UserRole::Unspecified => common_proto::UserRole::Unspecified as i32,
    })
}

pub(crate) fn map_user_status(status: i32) -> Result<i32, Status> {
    let status = common_proto::UserStatus::try_from(status)
        .map_err(|_| invalid_enum_value("status", status))?;
    Ok(match status {
        common_proto::UserStatus::Active => common_proto::UserStatus::Active as i32,
        common_proto::UserStatus::Banned => common_proto::UserStatus::Banned as i32,
        common_proto::UserStatus::Unspecified => common_proto::UserStatus::Unspecified as i32,
    })
}

pub(crate) fn map_room_status(status: i32) -> Result<i32, Status> {
    let status = common_proto::RoomStatus::try_from(status)
        .map_err(|_| invalid_enum_value("status", status))?;
    Ok(match status {
        common_proto::RoomStatus::Active => common_proto::RoomStatus::Active as i32,
        common_proto::RoomStatus::Closed => common_proto::RoomStatus::Closed as i32,
        common_proto::RoomStatus::Unspecified => common_proto::RoomStatus::Unspecified as i32,
    })
}

pub(crate) fn map_user_list_sort_by(sort_by: i32) -> Result<i32, Status> {
    let sort_by = crate::proto::UserListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::UserListSortBy::Username => admin_proto::UserListSortBy::Username as i32,
        crate::proto::UserListSortBy::Email => admin_proto::UserListSortBy::Email as i32,
        crate::proto::UserListSortBy::Status => admin_proto::UserListSortBy::Status as i32,
        crate::proto::UserListSortBy::Role => admin_proto::UserListSortBy::Role as i32,
        crate::proto::UserListSortBy::UpdatedAt => admin_proto::UserListSortBy::UpdatedAt as i32,
        crate::proto::UserListSortBy::CreatedAt | crate::proto::UserListSortBy::Unspecified => {
            admin_proto::UserListSortBy::CreatedAt as i32
        }
    })
}

pub(crate) fn map_room_list_sort_by(sort_by: i32) -> Result<i32, Status> {
    let sort_by = crate::proto::RoomListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::RoomListSortBy::Name => admin_proto::RoomListSortBy::Name as i32,
        crate::proto::RoomListSortBy::UpdatedAt => admin_proto::RoomListSortBy::UpdatedAt as i32,
        crate::proto::RoomListSortBy::LastActivityAt => {
            admin_proto::RoomListSortBy::LastActivityAt as i32
        }
        crate::proto::RoomListSortBy::CreatedAt | crate::proto::RoomListSortBy::Unspecified => {
            admin_proto::RoomListSortBy::CreatedAt as i32
        }
    })
}

pub(crate) fn map_room_member_list_sort_by(sort_by: i32) -> Result<i32, Status> {
    let sort_by = crate::proto::RoomMemberListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::RoomMemberListSortBy::Username => {
            admin_proto::RoomMemberListSortBy::Username as i32
        }
        crate::proto::RoomMemberListSortBy::Role => admin_proto::RoomMemberListSortBy::Role as i32,
        crate::proto::RoomMemberListSortBy::Status => {
            admin_proto::RoomMemberListSortBy::Status as i32
        }
        crate::proto::RoomMemberListSortBy::JoinedAt
        | crate::proto::RoomMemberListSortBy::Unspecified => {
            admin_proto::RoomMemberListSortBy::JoinedAt as i32
        }
    })
}

pub(crate) fn map_sort_direction(
    sort_direction: i32,
    default: admin_proto::SortDirection,
) -> Result<i32, Status> {
    let sort_direction = crate::proto::SortDirection::try_from(sort_direction)
        .map_err(|_| invalid_enum_value("sort_direction", sort_direction))?;
    Ok(match sort_direction {
        crate::proto::SortDirection::Asc => admin_proto::SortDirection::Asc as i32,
        crate::proto::SortDirection::Desc => admin_proto::SortDirection::Desc as i32,
        crate::proto::SortDirection::Unspecified => default as i32,
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

pub(crate) fn map_management_user_lookup_error(err: synctv_core::Error) -> Status {
    match err {
        synctv_core::Error::NotFound(_) => Status::not_found("User not found"),
        synctv_core::Error::InvalidInput(message) => Status::invalid_argument(message),
        synctv_core::Error::Authentication(message) => Status::unauthenticated(message),
        synctv_core::Error::Authorization(message) => Status::permission_denied(message),
        synctv_core::Error::AlreadyExists(message) => Status::already_exists(message),
        synctv_core::Error::RateLimited(message) => Status::resource_exhausted(message),
        synctv_core::Error::ServiceUnavailable(message) => Status::unavailable(message),
        synctv_core::Error::Timeout(message) => Status::deadline_exceeded(message),
        synctv_core::Error::OptimisticLockConflict => {
            Status::aborted("management actor user was modified concurrently")
        }
        synctv_core::Error::LockConflict(message) => Status::aborted(message),
        synctv_core::Error::EmailNotVerified => {
            Status::permission_denied("management actor user email is not verified")
        }
        other => {
            tracing::error!("Management user lookup failed: {other}");
            Status::internal("Internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{map_sort_direction, map_user_list_sort_by, validate_client_actor_user};
    use synctv_core::models::{SignupMethod, User, UserStatus};

    fn make_actor_user(username: &str, status: UserStatus) -> User {
        let mut user = User::new_with_status(
            username.to_string(),
            Some(format!("{username}@example.com")),
            "hash".to_string(),
            SignupMethod::Email,
            status,
        );
        user.email_verified = true;
        user
    }

    #[test]
    fn validate_client_actor_user_accepts_active_user() {
        let user = make_actor_user("root", UserStatus::Active);
        validate_client_actor_user(&user).expect("active actor should be accepted");
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
        let status =
            map_user_list_sort_by(99).expect_err("unknown management user sort enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_by"));
    }

    #[test]
    fn enum_mapping_preserves_status_user_sort() {
        assert_eq!(
            map_user_list_sort_by(crate::proto::UserListSortBy::Status as i32).unwrap(),
            synctv_proto::admin::UserListSortBy::Status as i32
        );
    }

    #[test]
    fn enum_mapping_rejects_unknown_sort_direction_values() {
        let status = map_sort_direction(99, synctv_proto::admin::SortDirection::Desc)
            .expect_err("unknown management sort direction enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_direction"));
    }
}
