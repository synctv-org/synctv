//! PostgreSQL representations for domain value types.

use crate::models::permission::Role;
use crate::models::{
    AuditDetails, BanRecordId, ChatAttachmentKind, ChatEventKind, ChatMessage, ChatMessageStatus,
    ChatMessageType, ChatMetadata, ContentReport, ContentReportAdminRow, ContentReportId,
    ContentReportMetadata, ContentReportStatus, ContentReportTargetType, EmailRegistrationTokenId,
    FileBlobCompression, FileCleanupMetadata, FileMetadata, FileObjectAccess,
    FileReferenceMetadata, FileUploadSessionKind, FileUploadSessionMetadata, FileVariantMetadata,
    MediaId, MediaSourceConfig, Notification, NotificationData, NotificationType, OAuth2Provider,
    PlaybackDurationSource, PlaybackDurationStatus, PlaybackSourceMetadata, Playlist, PlaylistId,
    PlaylistSourceConfig, ProviderPlaybackSession, ProviderPlaybackSessionState,
    ProviderPlaybackStopReason, ProviderTarget, ReviewRequestId, ReviewStatus, RoomCategoryId,
    RoomId, RoomLabelId, RoomPlaybackProgress, RoomPlaybackState, RoomSettings, RuntimeSetting,
    SignupMethod, SourceProvider, UserId, UserRole, UserStatus,
};

macro_rules! sqlx_numeric_id {
    ($name:ty) => {
        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <i64 as sqlx::Type<sqlx::Postgres>>::type_info()
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                <i64 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.get(), buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $name {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let id = <i64 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                <$name>::try_from(id).map_err(Into::into)
            }
        }
    };
}

sqlx_numeric_id!(UserId);
sqlx_numeric_id!(RoomId);
sqlx_numeric_id!(RoomCategoryId);
sqlx_numeric_id!(RoomLabelId);
sqlx_numeric_id!(MediaId);
sqlx_numeric_id!(PlaylistId);
sqlx_numeric_id!(ReviewRequestId);
sqlx_numeric_id!(BanRecordId);
sqlx_numeric_id!(EmailRegistrationTokenId);
sqlx_numeric_id!(ContentReportId);

macro_rules! sqlx_i16_enum {
    ($name:ty) => {
        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&i16::from(*self), buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $name {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let value = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                <$name>::try_from(value).map_err(Into::into)
            }
        }
    };
}

sqlx_i16_enum!(ChatMessageType);
sqlx_i16_enum!(ChatAttachmentKind);
sqlx_i16_enum!(ChatMessageStatus);
sqlx_i16_enum!(ChatEventKind);
sqlx_i16_enum!(ContentReportTargetType);
sqlx_i16_enum!(ContentReportStatus);
sqlx_i16_enum!(UserStatus);
sqlx_i16_enum!(UserRole);
sqlx_i16_enum!(SignupMethod);
sqlx_i16_enum!(PlaybackDurationStatus);
sqlx_i16_enum!(PlaybackDurationSource);
sqlx_i16_enum!(ProviderPlaybackSessionState);
sqlx_i16_enum!(ProviderPlaybackStopReason);
sqlx_i16_enum!(Role);
sqlx_i16_enum!(NotificationType);
sqlx_i16_enum!(ReviewStatus);
sqlx_i16_enum!(SourceProvider);

macro_rules! sqlx_i16_file_enum {
    ($name:ty, $description:literal) => {
        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&i16::from(*self), buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $name {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let value = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                <$name>::try_from(value)
                    .map_err(|()| format!("unknown {} value {value}", $description).into())
            }
        }
    };
}

sqlx_i16_file_enum!(FileBlobCompression, "file blob compression");
sqlx_i16_file_enum!(FileUploadSessionKind, "file upload session kind");

#[derive(Debug, Clone)]
pub(crate) struct ProviderTypeName(pub SourceProvider);

impl sqlx::Type<sqlx::Postgres> for ProviderTypeName {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <i16 as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for ProviderTypeName {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let code = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        SourceProvider::try_from(code).map(Self).map_err(Into::into)
    }
}

impl sqlx::Type<sqlx::Postgres> for OAuth2Provider {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <i16 as sqlx::Type<sqlx::Postgres>>::type_info()
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for OAuth2Provider {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <i16 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.as_i16(), buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for OAuth2Provider {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let value = <i16 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Self::try_from(value).map_err(Into::into)
    }
}

macro_rules! sqlx_json {
    ($name:ty) => {
        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <sqlx::types::Json<Self> as sqlx::Type<sqlx::Postgres>>::type_info()
            }

            fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
                <sqlx::types::Json<Self> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                sqlx::types::Json(self).encode_by_ref(buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $name {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let sqlx::types::Json(value) =
                    <sqlx::types::Json<Self> as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                Ok(value)
            }
        }
    };
}

sqlx_json!(ContentReportMetadata);
sqlx_json!(ProviderPlaybackSession);
sqlx_json!(ProviderTarget);
sqlx_json!(MediaSourceConfig);
sqlx_json!(PlaylistSourceConfig);
sqlx_json!(NotificationData);
sqlx_json!(AuditDetails);
sqlx_json!(RoomSettings);
sqlx_json!(ChatMetadata);
sqlx_json!(FileMetadata);
sqlx_json!(FileVariantMetadata);
sqlx_json!(FileReferenceMetadata);
sqlx_json!(FileCleanupMetadata);
sqlx_json!(FileUploadSessionMetadata);
sqlx_json!(FileObjectAccess);

macro_rules! sqlx_column {
    ($field:ident) => {
        stringify!($field)
    };
    ($field:ident, $column:literal) => {
        $column
    };
}

macro_rules! sqlx_from_row {
    ($name:ty, { $($field:ident $(=> $column:literal)?),+ $(,)? }) => {
        impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for $name {
            fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
                Ok(Self {
                    $($field: sqlx::Row::try_get(row, sqlx_column!($field $(, $column)?))?,)+
                })
            }
        }
    };
}

sqlx_from_row!(ContentReport, {
    id,
    reporter_user_id,
    room_id,
    target_type,
    target_room_id,
    target_user_id,
    target_member_room_id,
    target_member_user_id,
    target_chat_message_id,
    target_chat_message_created_at,
    reason_code,
    reason,
    metadata,
    status,
    reviewed_by,
    reviewed_at,
    resolution_note,
    created_at,
    updated_at,
});

sqlx_from_row!(ContentReportAdminRow, {
    id,
    reporter_user_id,
    reporter_username,
    room_id,
    room_name,
    target_type,
    target_room_id,
    target_room_name,
    target_user_id,
    target_username,
    target_member_room_id,
    target_member_room_name,
    target_member_user_id,
    target_member_username,
    target_chat_message_id,
    target_chat_message_created_at,
    target_chat_message_preview,
    reason_code,
    reason,
    metadata,
    status,
    reviewed_by,
    reviewed_by_username,
    reviewed_at,
    resolution_note,
    created_at,
    updated_at,
});

sqlx_from_row!(RuntimeSetting, {
    key,
    group_name,
    value,
    version,
    created_at,
    updated_at,
});

sqlx_from_row!(Playlist, {
    id,
    room_id,
    creator_id,
    name,
    description,
    cover_file_reference_id,
    parent_id,
    position,
    source_provider,
    source_config,
    provider_instance_name,
    created_at,
    updated_at,
    version,
});

sqlx_from_row!(RoomPlaybackState, {
    room_id,
    playing_media_id,
    playing_playlist_id,
    target,
    current_progress_id,
    history_cursor_id,
    position,
    speed,
    is_playing,
    playback_generation,
    updated_at,
    version,
});

sqlx_from_row!(RoomPlaybackProgress, {
    id,
    room_id,
    media_id,
    playlist_id,
    target,
    target_hash,
    position,
    created_at,
    updated_at,
    version,
});

sqlx_from_row!(PlaybackSourceMetadata, {
    room_id,
    media_id,
    playlist_id,
    target_hash,
    is_live,
    duration_seconds,
    duration_status,
    duration_source,
    duration_error,
    next_retry_at,
    created_at,
    updated_at,
    version,
});

sqlx_from_row!(Notification, {
    id,
    user_id,
    notification_type => "type",
    title,
    content,
    data,
    is_read,
    created_at,
    updated_at,
});

sqlx_from_row!(ChatMessage, {
    id,
    room_id,
    user_id,
    client_message_id,
    content,
    message_type,
    status,
    version,
    reply_to_message_id,
    reply_to_message_created_at,
    metadata,
    edited_at,
    deleted_at,
    deleted_by,
    delete_reason,
    created_at,
});
