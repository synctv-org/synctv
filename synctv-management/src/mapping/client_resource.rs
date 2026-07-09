use tonic::Status;

use synctv_adapter::{PublicIdCodec, PublicIdType};
use synctv_core::models::{ChatHistoryCursor, ChatSearchMessagesQuery, RoomId, UserId};
use synctv_proto::{client as client_proto, source_config as source_config_proto};

pub(crate) fn room_settings_from_client_proto(
    settings: client_proto::RoomSettings,
) -> Result<synctv_core::models::RoomSettings, Status> {
    let auto_play = settings.auto_play.unwrap_or_default();
    let play_mode = match client_proto::PlayMode::try_from(auto_play.mode)
        .map_err(|_| Status::invalid_argument("Unsupported auto_play.mode"))?
    {
        client_proto::PlayMode::Unspecified | client_proto::PlayMode::Sequential => {
            synctv_core::models::PlayMode::Sequential
        }
        client_proto::PlayMode::RepeatOne => synctv_core::models::PlayMode::RepeatOne,
        client_proto::PlayMode::RepeatAll => synctv_core::models::PlayMode::RepeatAll,
        client_proto::PlayMode::Shuffle => synctv_core::models::PlayMode::Shuffle,
    };

    Ok(synctv_core::models::RoomSettings {
        allow_guest_join: synctv_core::models::room_settings::AllowGuestJoin::new(
            settings.allow_guest_join,
        ),
        max_members: synctv_core::models::room_settings::MaxMembers::new(settings.max_members),
        require_approval: synctv_core::models::room_settings::RequireApproval::new(
            settings.require_approval,
        ),
        allow_auto_join: synctv_core::models::room_settings::AllowAutoJoin::new(
            settings.allow_auto_join,
        ),
        chat_enabled: synctv_core::models::room_settings::ChatEnabled::new(settings.chat_enabled),
        auto_play: synctv_core::models::room_settings::AutoPlay::new(
            synctv_core::models::AutoPlaySettings {
                enabled: auto_play.enabled,
                mode: play_mode,
                delay: auto_play.delay,
            },
        ),
        admin_added_permissions: synctv_core::models::room_settings::AdminAddedPermissions::new(
            settings.admin_added_permissions,
        ),
        admin_removed_permissions: synctv_core::models::room_settings::AdminRemovedPermissions::new(
            settings.admin_removed_permissions,
        ),
        member_added_permissions: synctv_core::models::room_settings::MemberAddedPermissions::new(
            settings.member_added_permissions,
        ),
        member_removed_permissions:
            synctv_core::models::room_settings::MemberRemovedPermissions::new(
                settings.member_removed_permissions,
            ),
        guest_added_permissions: synctv_core::models::room_settings::GuestAddedPermissions::new(
            settings.guest_added_permissions,
        ),
        guest_removed_permissions: synctv_core::models::room_settings::GuestRemovedPermissions::new(
            settings.guest_removed_permissions,
        ),
    })
}

pub(crate) fn optional_room_category_id_from_public(
    value: &str,
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Option<synctv_core::models::RoomCategoryId>, Status> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    public_id_codec
        .decode_room_category_id(value)
        .map(Some)
        .map_err(Status::invalid_argument)
}

pub(crate) fn room_label_ids_from_public(
    values: &[String],
    public_id_codec: &synctv_adapter::PublicIdCodec,
) -> Result<Vec<synctv_core::models::RoomLabelId>, Status> {
    values
        .iter()
        .map(|value| {
            public_id_codec
                .decode_room_label_id(value)
                .map_err(Status::invalid_argument)
        })
        .collect()
}

pub(crate) fn optional_playlist_id_from_public(
    value: impl AsRef<str>,
    public_id_codec: &PublicIdCodec,
) -> Result<Option<synctv_core::models::PlaylistId>, Status> {
    let value = value.as_ref();
    if value.trim().is_empty() {
        return Ok(None);
    }
    public_id_codec
        .decode_playlist_id(value)
        .map(Some)
        .map_err(Status::invalid_argument)
}

fn public_id_from_proto<T>(
    value: impl AsRef<str>,
    public_id_codec: &PublicIdCodec,
) -> Result<T, Status>
where
    T: PublicIdType,
{
    public_id_codec
        .decode::<T>(value.as_ref().trim())
        .map_err(|error| Status::invalid_argument(format!("Invalid {}: {error}", T::TYPE_NAME)))
}

pub(crate) fn room_id_from_public(
    value: impl AsRef<str>,
    public_id_codec: &PublicIdCodec,
) -> Result<RoomId, Status> {
    public_id_from_proto(value, public_id_codec)
}

pub(crate) fn user_id_from_public(
    value: impl AsRef<str>,
    public_id_codec: &PublicIdCodec,
) -> Result<UserId, Status> {
    public_id_from_proto(value, public_id_codec)
}

fn chat_history_cursor_from_proto(cursor: &str) -> Result<Option<ChatHistoryCursor>, Status> {
    if cursor.is_empty() {
        return Ok(None);
    }
    let Some((created_at, id)) = cursor.split_once('|') else {
        return Err(Status::invalid_argument("Invalid cursor format"));
    };
    let created_at = synctv_common::time::parse_datetime_to_utc(created_at)
        .map_err(|_| Status::invalid_argument("Invalid cursor format"))?;
    let id = id
        .parse::<i64>()
        .map_err(|_| Status::invalid_argument("Invalid cursor format"))?;
    Ok(Some(ChatHistoryCursor { created_at, id }))
}

pub(crate) fn chat_history_cursor_to_client_proto(cursor: ChatHistoryCursor) -> String {
    format!(
        "{}|{}",
        synctv_common::time::format_datetime_rfc3339(cursor.created_at),
        cursor.id
    )
}

pub(crate) fn search_chat_messages_query_from_client_proto(
    room_id: RoomId,
    request: &client_proto::SearchChatMessagesRequest,
    public_id_codec: &PublicIdCodec,
) -> Result<ChatSearchMessagesQuery, Status> {
    synctv_proto::validate(request).map_err(|error| Status::invalid_argument(error.to_string()))?;
    let limit = if request.limit > 0 { request.limit } else { 50 };
    let user_id = if request.user_id.trim().is_empty() {
        None
    } else {
        Some(user_id_from_public(&request.user_id, public_id_codec)?)
    };

    Ok(ChatSearchMessagesQuery {
        room_id,
        query: request.query.clone(),
        cursor: chat_history_cursor_from_proto(&request.cursor)?,
        limit,
        include_deleted: request.include_deleted,
        user_id,
    })
}

pub(crate) fn source_provider_from_proto_filter(
    value: i32,
) -> Result<Option<synctv_core::models::SourceProvider>, Status> {
    if value == source_config_proto::SourceProvider::Unspecified as i32 {
        return Ok(None);
    }

    match source_config_proto::SourceProvider::try_from(value)
        .map_err(|_| Status::invalid_argument("Unsupported source_provider"))?
    {
        source_config_proto::SourceProvider::Unspecified => Ok(None),
        source_config_proto::SourceProvider::DirectUrl => {
            Ok(Some(synctv_core::models::SourceProvider::DirectUrl))
        }
        source_config_proto::SourceProvider::Bilibili => {
            Ok(Some(synctv_core::models::SourceProvider::Bilibili))
        }
        source_config_proto::SourceProvider::Alist => {
            Ok(Some(synctv_core::models::SourceProvider::Alist))
        }
        source_config_proto::SourceProvider::Emby => {
            Ok(Some(synctv_core::models::SourceProvider::Emby))
        }
        source_config_proto::SourceProvider::Rtmp => {
            Ok(Some(synctv_core::models::SourceProvider::Rtmp))
        }
        source_config_proto::SourceProvider::LiveProxy => {
            Ok(Some(synctv_core::models::SourceProvider::LiveProxy))
        }
    }
}
