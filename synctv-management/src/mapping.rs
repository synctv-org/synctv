use tonic::Status;

use synctv_core::models::UserStatus as CoreUserStatus;
use synctv_proto::{admin as admin_proto, client as client_proto, common as common_proto};

pub(crate) fn invalid_enum_value(field: &'static str, value: i32) -> Status {
    Status::invalid_argument(format!("Invalid {field}: unknown enum value {value}"))
}

pub(crate) fn map_user_role(role: i32) -> Result<i32, Status> {
    common_proto::UserRole::try_from(role).map_err(|_| invalid_enum_value("role", role))?;
    Ok(role)
}

pub(crate) fn map_user_status(status: i32) -> Result<i32, Status> {
    common_proto::UserStatus::try_from(status).map_err(|_| invalid_enum_value("status", status))?;
    Ok(status)
}

pub(crate) fn map_room_status(status: i32) -> Result<i32, Status> {
    common_proto::RoomStatus::try_from(status).map_err(|_| invalid_enum_value("status", status))?;
    Ok(status)
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
        crate::proto::RoomMemberListSortBy::JoinedAt
        | crate::proto::RoomMemberListSortBy::Unspecified => {
            admin_proto::RoomMemberListSortBy::JoinedAt as i32
        }
    })
}

pub(crate) fn map_room_stream_list_sort_by(sort_by: i32) -> Result<i32, Status> {
    let sort_by = crate::proto::RoomStreamListSortBy::try_from(sort_by)
        .map_err(|_| invalid_enum_value("sort_by", sort_by))?;
    Ok(match sort_by {
        crate::proto::RoomStreamListSortBy::MediaId => {
            client_proto::RoomStreamListSortBy::MediaId as i32
        }
        crate::proto::RoomStreamListSortBy::Unspecified => {
            client_proto::RoomStreamListSortBy::Unspecified as i32
        }
    })
}

pub(crate) fn map_client_sort_direction(
    sort_direction: i32,
    default: client_proto::SortDirection,
) -> Result<i32, Status> {
    let sort_direction = crate::proto::SortDirection::try_from(sort_direction)
        .map_err(|_| invalid_enum_value("sort_direction", sort_direction))?;
    Ok(match sort_direction {
        crate::proto::SortDirection::Asc => client_proto::SortDirection::Asc as i32,
        crate::proto::SortDirection::Desc => client_proto::SortDirection::Desc as i32,
        crate::proto::SortDirection::Unspecified => default as i32,
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
        other => {
            tracing::error!("Management user lookup failed: {other}");
            Status::internal("Internal error")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        map_client_sort_direction, map_room_stream_list_sort_by, map_sort_direction,
        map_user_list_sort_by, validate_client_actor_user,
    };
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
        let status =
            map_user_list_sort_by(99).expect_err("unknown management user sort enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_by"));
    }

    #[test]
    fn enum_mapping_preserves_status_user_sort() -> Result<(), tonic::Status> {
        assert_eq!(
            map_user_list_sort_by(crate::proto::UserListSortBy::Status as i32)?,
            synctv_proto::admin::UserListSortBy::Status as i32
        );
        Ok(())
    }

    #[test]
    fn enum_mapping_rejects_unknown_sort_direction_values() {
        let status = map_sort_direction(99, synctv_proto::admin::SortDirection::Desc)
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
    fn enum_mapping_rejects_unknown_client_sort_direction_values() {
        let status = map_client_sort_direction(99, synctv_proto::client::SortDirection::Desc)
            .expect_err("unknown client sort direction enum should fail");

        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("sort_direction"));
    }
}
