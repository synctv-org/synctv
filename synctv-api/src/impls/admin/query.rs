use std::cmp::Ordering;

use synctv_core::models::{
    MediaListSortBy as CoreMediaListSortBy, PlaylistListSortBy as CorePlaylistListSortBy,
    RoomStatus, SortDirection as CoreSortDirection, UserRole, UserStatus,
};
use synctv_core::service::BanRecordTargetType;

use crate::impls::ApiError;

use super::ActiveStreamListSortBy;

pub(in crate::impls::admin) fn page_i32_to_usize(value: i32) -> Result<usize, ApiError> {
    let normalized = if value > 0 { value.cast_unsigned() } else { 1 };
    usize::try_from(normalized).map_err(|_| ApiError::Internal("page exceeds usize::MAX".into()))
}

pub(in crate::impls::admin) fn page_size_i32_to_usize(
    value: i32,
    max: i32,
) -> Result<usize, ApiError> {
    let max = max.max(1).cast_unsigned();
    let normalized = if value > 0 {
        value.cast_unsigned().clamp(1, max)
    } else {
        1
    };
    usize::try_from(normalized)
        .map_err(|_| ApiError::Internal("page_size exceeds usize::MAX".into()))
}

pub(in crate::impls::admin) fn usize_to_i32_api(
    value: usize,
    field: &'static str,
) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub(in crate::impls::admin) fn i64_to_i32_api(
    value: i64,
    field: &'static str,
) -> Result<i32, ApiError> {
    i32::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i32::MAX")))
}

pub(in crate::impls::admin) fn usize_to_i64_api(
    value: usize,
    field: &'static str,
) -> Result<i64, ApiError> {
    i64::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i64::MAX")))
}

pub(in crate::impls::admin) fn i64_count_to_usize(
    value: i64,
    field: &'static str,
) -> Result<usize, ApiError> {
    if value < 0 {
        return Err(ApiError::Internal(format!(
            "{field} returned a negative count"
        )));
    }
    usize::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds usize::MAX")))
}

pub(in crate::impls::admin) fn u64_to_i64_api(
    value: u64,
    field: &'static str,
) -> Result<i64, ApiError> {
    i64::try_from(value).map_err(|_| ApiError::Internal(format!("{field} exceeds i64::MAX")))
}

pub(in crate::impls::admin) fn page_offset_usize(
    page: usize,
    page_size: usize,
    field: &'static str,
) -> Result<usize, ApiError> {
    page.saturating_sub(1)
        .checked_mul(page_size)
        .ok_or_else(|| ApiError::Internal(format!("{field} exceeds usize::MAX")))
}

pub(crate) fn pagination_limit_offset_i64(
    page: i32,
    page_size: i32,
    label: &'static str,
) -> Result<(i64, i64), ApiError> {
    let page = page_i32_to_usize(page)?;
    let page_size = crate::impls::proto_page_size_usize(page_size, 50, 100)?;
    let offset = page_offset_usize(page, page_size, label)?;
    let limit = usize_to_i64_api(page_size, label)?;
    let offset = usize_to_i64_api(offset, label)?;
    Ok((limit, offset))
}

pub(crate) fn proto_review_status_filter(
    value: i32,
) -> Result<synctv_core::models::ReviewStatus, ApiError> {
    synctv_core::models::ReviewStatus::try_from(value)
        .map_err(|_| ApiError::InvalidInput("Unsupported review status".to_string()))
}

pub(in crate::impls::admin) fn proto_room_status_filter(
    value: i32,
) -> Result<Option<RoomStatus>, ApiError> {
    if value == synctv_proto::common::RoomStatus::Unspecified as i32 {
        return Ok(None);
    }
    RoomStatus::try_from(value)
        .map(Some)
        .map_err(|_| ApiError::InvalidInput("Unsupported room status".to_string()))
}

pub(crate) fn proto_user_status_filter(value: i32) -> Result<Option<UserStatus>, ApiError> {
    if value == synctv_proto::common::UserStatus::Unspecified as i32 {
        return Ok(None);
    }
    UserStatus::try_from(value)
        .map(Some)
        .map_err(|_| ApiError::InvalidInput("Unsupported user status".to_string()))
}

pub(crate) fn proto_user_role_filter(value: i32) -> Result<Option<UserRole>, ApiError> {
    if value == synctv_proto::common::UserRole::Unspecified as i32 {
        return Ok(None);
    }
    UserRole::try_from(value)
        .map(Some)
        .map_err(|_| ApiError::InvalidInput("Unsupported user role".to_string()))
}

pub(crate) fn ban_record_target_type_from_proto(
    value: i32,
) -> Result<Option<BanRecordTargetType>, ApiError> {
    match synctv_proto::admin::BanTargetType::try_from(value) {
        Ok(synctv_proto::admin::BanTargetType::Unspecified) => Ok(None),
        Ok(synctv_proto::admin::BanTargetType::User) => Ok(Some(BanRecordTargetType::User)),
        Ok(synctv_proto::admin::BanTargetType::Room) => Ok(Some(BanRecordTargetType::Room)),
        Err(_) => Err(ApiError::InvalidInput(
            "Invalid ban record target type".to_string(),
        )),
    }
}

pub(crate) fn proto_admin_sort_direction(
    sort_direction: i32,
    default: CoreSortDirection,
) -> Result<CoreSortDirection, ApiError> {
    match synctv_proto::admin::SortDirection::try_from(sort_direction)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        synctv_proto::admin::SortDirection::Unspecified => Ok(default),
        synctv_proto::admin::SortDirection::Asc => Ok(CoreSortDirection::Asc),
        synctv_proto::admin::SortDirection::Desc => Ok(CoreSortDirection::Desc),
    }
}

pub(crate) fn proto_admin_user_list_sort_by(
    sort_by: i32,
) -> Result<synctv_core::models::UserListSortBy, ApiError> {
    match synctv_proto::admin::UserListSortBy::try_from(sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported user list sort field".to_string()))?
    {
        synctv_proto::admin::UserListSortBy::Unspecified
        | synctv_proto::admin::UserListSortBy::CreatedAt => {
            Ok(synctv_core::models::UserListSortBy::CreatedAt)
        }
        synctv_proto::admin::UserListSortBy::Username => {
            Ok(synctv_core::models::UserListSortBy::Username)
        }
        synctv_proto::admin::UserListSortBy::Email => {
            Ok(synctv_core::models::UserListSortBy::Email)
        }
        synctv_proto::admin::UserListSortBy::Status => {
            Ok(synctv_core::models::UserListSortBy::Status)
        }
        synctv_proto::admin::UserListSortBy::Role => Ok(synctv_core::models::UserListSortBy::Role),
        synctv_proto::admin::UserListSortBy::UpdatedAt => {
            Ok(synctv_core::models::UserListSortBy::UpdatedAt)
        }
    }
}

pub(in crate::impls::admin) fn proto_admin_room_list_sort_by(
    sort_by: i32,
) -> Result<synctv_core::models::RoomListSortBy, ApiError> {
    match synctv_proto::admin::RoomListSortBy::try_from(sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported room list sort field".to_string()))?
    {
        synctv_proto::admin::RoomListSortBy::Unspecified
        | synctv_proto::admin::RoomListSortBy::CreatedAt => {
            Ok(synctv_core::models::RoomListSortBy::CreatedAt)
        }
        synctv_proto::admin::RoomListSortBy::Name => Ok(synctv_core::models::RoomListSortBy::Name),
        synctv_proto::admin::RoomListSortBy::UpdatedAt => {
            Ok(synctv_core::models::RoomListSortBy::UpdatedAt)
        }
        synctv_proto::admin::RoomListSortBy::LastActivityAt => {
            Ok(synctv_core::models::RoomListSortBy::LastActivityAt)
        }
    }
}

pub(in crate::impls::admin) fn proto_admin_room_member_list_sort_by(
    sort_by: i32,
) -> Result<synctv_core::models::RoomMemberListSortBy, ApiError> {
    match synctv_proto::admin::RoomMemberListSortBy::try_from(sort_by).map_err(|_| {
        ApiError::InvalidInput("Unsupported room member list sort field".to_string())
    })? {
        synctv_proto::admin::RoomMemberListSortBy::Unspecified
        | synctv_proto::admin::RoomMemberListSortBy::JoinedAt => {
            Ok(synctv_core::models::RoomMemberListSortBy::JoinedAt)
        }
        synctv_proto::admin::RoomMemberListSortBy::Username => {
            Ok(synctv_core::models::RoomMemberListSortBy::Username)
        }
        synctv_proto::admin::RoomMemberListSortBy::Role => {
            Ok(synctv_core::models::RoomMemberListSortBy::Role)
        }
    }
}

pub(crate) fn proto_admin_active_stream_list_sort_by(
    sort_by: i32,
) -> Result<ActiveStreamListSortBy, ApiError> {
    match synctv_proto::admin::ActiveStreamListSortBy::try_from(sort_by).map_err(|_| {
        ApiError::InvalidInput("Unsupported active stream list sort field".to_string())
    })? {
        synctv_proto::admin::ActiveStreamListSortBy::Unspecified
        | synctv_proto::admin::ActiveStreamListSortBy::StartedAt => {
            Ok(ActiveStreamListSortBy::StartedAt)
        }
        synctv_proto::admin::ActiveStreamListSortBy::RoomId => Ok(ActiveStreamListSortBy::RoomId),
        synctv_proto::admin::ActiveStreamListSortBy::MediaId => Ok(ActiveStreamListSortBy::MediaId),
        synctv_proto::admin::ActiveStreamListSortBy::UserId => Ok(ActiveStreamListSortBy::UserId),
        synctv_proto::admin::ActiveStreamListSortBy::NodeId => Ok(ActiveStreamListSortBy::NodeId),
    }
}

pub(crate) fn proto_admin_active_stream_sort_direction(
    sort_direction: i32,
) -> Result<CoreSortDirection, ApiError> {
    match synctv_proto::admin::SortDirection::try_from(sort_direction)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        synctv_proto::admin::SortDirection::Unspecified
        | synctv_proto::admin::SortDirection::Desc => Ok(CoreSortDirection::Desc),
        synctv_proto::admin::SortDirection::Asc => Ok(CoreSortDirection::Asc),
    }
}

pub(in crate::impls::admin) fn map_client_sort_direction(
    sort_direction: i32,
) -> Result<CoreSortDirection, ApiError> {
    match synctv_proto::client::SortDirection::try_from(sort_direction)
        .map_err(|_| ApiError::InvalidInput("Unsupported sort direction".to_string()))?
    {
        synctv_proto::client::SortDirection::Unspecified
        | synctv_proto::client::SortDirection::Asc => Ok(CoreSortDirection::Asc),
        synctv_proto::client::SortDirection::Desc => Ok(CoreSortDirection::Desc),
    }
}

pub(in crate::impls::admin) fn map_admin_playlist_sort(
    sort_by: i32,
) -> Result<CorePlaylistListSortBy, ApiError> {
    match synctv_proto::client::PlaylistListSortBy::try_from(sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported playlist list sort field".to_string()))?
    {
        synctv_proto::client::PlaylistListSortBy::Unspecified
        | synctv_proto::client::PlaylistListSortBy::Position => {
            Ok(CorePlaylistListSortBy::Position)
        }
        synctv_proto::client::PlaylistListSortBy::Name => Ok(CorePlaylistListSortBy::Name),
        synctv_proto::client::PlaylistListSortBy::CreatedAt => {
            Ok(CorePlaylistListSortBy::CreatedAt)
        }
        synctv_proto::client::PlaylistListSortBy::UpdatedAt => {
            Ok(CorePlaylistListSortBy::UpdatedAt)
        }
    }
}

pub(in crate::impls::admin) fn map_admin_playlist_sort_from_media_sort(
    sort_by: i32,
) -> Result<CorePlaylistListSortBy, ApiError> {
    match synctv_proto::client::MediaListSortBy::try_from(sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported media list sort field".to_string()))?
    {
        synctv_proto::client::MediaListSortBy::Unspecified
        | synctv_proto::client::MediaListSortBy::Position => Ok(CorePlaylistListSortBy::Position),
        synctv_proto::client::MediaListSortBy::Name => Ok(CorePlaylistListSortBy::Name),
        synctv_proto::client::MediaListSortBy::AddedAt => Ok(CorePlaylistListSortBy::CreatedAt),
        synctv_proto::client::MediaListSortBy::UpdatedAt => Ok(CorePlaylistListSortBy::UpdatedAt),
        synctv_proto::client::MediaListSortBy::SourceProvider
        | synctv_proto::client::MediaListSortBy::ProviderInstanceName => {
            Ok(CorePlaylistListSortBy::Position)
        }
    }
}

pub(in crate::impls::admin) fn map_admin_media_sort(
    sort_by: i32,
) -> Result<CoreMediaListSortBy, ApiError> {
    match synctv_proto::client::MediaListSortBy::try_from(sort_by)
        .map_err(|_| ApiError::InvalidInput("Unsupported media list sort field".to_string()))?
    {
        synctv_proto::client::MediaListSortBy::Unspecified
        | synctv_proto::client::MediaListSortBy::Position => Ok(CoreMediaListSortBy::Position),
        synctv_proto::client::MediaListSortBy::Name => Ok(CoreMediaListSortBy::Name),
        synctv_proto::client::MediaListSortBy::AddedAt => Ok(CoreMediaListSortBy::AddedAt),
        synctv_proto::client::MediaListSortBy::UpdatedAt => Ok(CoreMediaListSortBy::UpdatedAt),
        synctv_proto::client::MediaListSortBy::SourceProvider => {
            Ok(CoreMediaListSortBy::SourceProvider)
        }
        synctv_proto::client::MediaListSortBy::ProviderInstanceName => {
            Ok(CoreMediaListSortBy::ProviderInstanceName)
        }
    }
}

pub(in crate::impls::admin) fn map_resource_availability_filter(
    filter: i32,
) -> Result<Option<bool>, ApiError> {
    match synctv_proto::client::ResourceAvailabilityFilter::try_from(filter)
        .map_err(|_| ApiError::InvalidInput("Unsupported availability filter".to_string()))?
    {
        synctv_proto::client::ResourceAvailabilityFilter::All => Ok(None),
        synctv_proto::client::ResourceAvailabilityFilter::Available => Ok(Some(true)),
        synctv_proto::client::ResourceAvailabilityFilter::Unavailable => Ok(Some(false)),
    }
}

pub(in crate::impls::admin) fn paginate_vec<T>(
    items: Vec<T>,
    page: i32,
    page_size: i32,
) -> Result<Vec<T>, ApiError> {
    let page = page_i32_to_usize(page)?;
    let page_size = page_size_i32_to_usize(page_size, 100)?;
    let offset = (page - 1) * page_size;
    Ok(items.into_iter().skip(offset).take(page_size).collect())
}

pub(in crate::impls::admin) fn compare_active_streams(
    left: &synctv_proto::admin::ActiveStreamInfo,
    right: &synctv_proto::admin::ActiveStreamInfo,
    sort_by: ActiveStreamListSortBy,
    sort_direction: CoreSortDirection,
) -> Ordering {
    let ordering = match sort_by {
        ActiveStreamListSortBy::RoomId => left
            .room_id
            .cmp(&right.room_id)
            .then_with(|| left.media_id.cmp(&right.media_id)),
        ActiveStreamListSortBy::MediaId => left
            .media_id
            .cmp(&right.media_id)
            .then_with(|| left.room_id.cmp(&right.room_id)),
        ActiveStreamListSortBy::UserId => left
            .user_id
            .cmp(&right.user_id)
            .then_with(|| left.started_at.cmp(&right.started_at)),
        ActiveStreamListSortBy::NodeId => left
            .node_id
            .cmp(&right.node_id)
            .then_with(|| left.started_at.cmp(&right.started_at)),
        ActiveStreamListSortBy::StartedAt => left
            .started_at
            .cmp(&right.started_at)
            .then_with(|| left.room_id.cmp(&right.room_id))
            .then_with(|| left.media_id.cmp(&right.media_id)),
    };

    match sort_direction {
        CoreSortDirection::Asc => ordering,
        CoreSortDirection::Desc => ordering.reverse(),
    }
}
