macro_rules! sort_field_enum {
    (
        $(#[$enum_meta:meta])*
        pub enum $name:ident {
            $(
                $variant:ident => {
                    display: $display:literal,
                    sql: $sql:literal
                }
            ),+ $(,)?
        }
        default = $default:ident;
        error = $error:literal;
    ) => {
        $(#[$enum_meta])*
        pub enum $name {
            $($variant),+
        }

        impl Default for $name {
            fn default() -> Self {
                Self::$default
            }
        }

        impl $name {
            #[must_use]
            pub const fn as_sql(self) -> &'static str {
                match self {
                    $(Self::$variant => $sql),+
                }
            }

            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $display),+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                let normalized = raw.trim().to_ascii_lowercase();
                match normalized.as_str() {
                    $($display => Ok(Self::$variant),)+
                    other => Err(format!("{}: {other}", $error)),
                }
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str((*self).as_str())
            }
        }
    };
}

macro_rules! sqlx_i16_enum {
    ($name:ident, $error:literal, { $($variant:ident = $value:literal),+ $(,)? }) => {
        impl From<$name> for i16 {
            fn from(value: $name) -> Self {
                match value {
                    $($name::$variant => $value),+
                }
            }
        }

        impl std::convert::TryFrom<i16> for $name {
            type Error = String;

            fn try_from(value: i16) -> Result<Self, Self::Error> {
                match value {
                    $($value => Ok($name::$variant),)+
                    other => Err(format!("{}: {other}", $error)),
                }
            }
        }

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
                Self::try_from(value).map_err(Into::into)
            }
        }
    };
}

pub mod audit;
pub mod chat;
pub mod content_report;
pub mod email_token;
pub mod file_storage;
pub mod id;
pub mod media;
pub mod notification;
pub mod oauth2_client;
pub mod opaque_password;
pub mod pagination;
pub mod permission;
pub mod playback;
pub mod playlist;
pub mod provider_instance;
pub mod query;
pub mod review;
pub mod room;
pub mod room_member;
pub mod room_settings;
pub mod settings;
pub mod user;
pub mod user_preferences;

pub use audit::{AuditAction, AuditTargetType};
pub use chat::{
    ChatAttachment, ChatAttachmentKind, ChatAttachmentUploadSession, ChatEventKind,
    ChatHistoryCursor, ChatHistoryPage, ChatMention, ChatMentionInput, ChatMessage,
    ChatMessageContext, ChatMessageEvent, ChatMessageEventLog, ChatMessageReadReceiptMember,
    ChatMessageReadReceiptUser, ChatMessageReadReceiptsPage, ChatMessageStatus, ChatMessageType,
    ChatMessageWithAttachments, ChatPlaybackMessagesQuery, ChatReaction, ChatReactionSummary,
    ChatReactionUser, ChatReactionUsersCursor, ChatReactionUsersPage, ChatReadState,
    ChatReadStateWithUnread, CreateChatAttachmentUploadSession, DeleteChatMessage, EditChatMessage,
    EventCursor, MarkChatRead, SendChatMessage, SendChatRequest, SetChatReaction,
    CHAT_ATTACHMENT_FILENAME_MAX_CHARS, CHAT_ATTACHMENT_ID_MAX_CHARS,
    CHAT_CLIENT_MESSAGE_ID_MAX_CHARS, CHAT_CLIENT_OPERATION_ID_MAX_CHARS, CHAT_EVENT_ID_MAX_CHARS,
    CHAT_EVENT_TYPE_MAX_CHARS, CHAT_REACTION_KEY_MAX_CHARS,
};
pub use content_report::{
    ContentReport, ContentReportAdminRow, ContentReportStatus, ContentReportTarget,
    ContentReportTargetType, CreateContentReport,
};
pub use email_token::EmailTokenType;
pub use file_storage::{
    CompleteFileUploadPart, CompleteFileUploadSession, CompleteFileUploadSessionResult,
    CreateFileUploadSession, FileBlob, FileBlobCompression, FileBlobPart, FileByteRange,
    FileCleanupJob, FileObject, FileObjectData, FileOwnershipProofRange, FileRangeRequest,
    FileReferenceTarget, FileUploadManifestPart, FileUploadPartUrl, FileUploadPlan,
    FileUploadPlanPart, FileUploadPolicy, FileUploadRange, FileUploadSession,
    FileUploadSessionCreateResult, FileUploadSessionKind, FileUploadSessionPart,
    FileUploadSessionRecord, GetFileObject, NewStoredFile, StoreFileUpload, StoreFileUploadResult,
    StoredFileReference, SubmittedFileReference, SubmittedFileReferenceKind,
    FILE_CLEANUP_ORIGIN_MAX_CHARS, FILE_ID_MAX_CHARS, FILE_OBJECT_KEY_MAX_CHARS,
    FILE_REFERENCE_ID_MAX_CHARS, FILE_REFERENCE_KIND_MAX_CHARS, FILE_SHA256_HEX_CHARS,
    FILE_STORAGE_BACKEND_MAX_CHARS,
};
pub use id::{
    generate_id, BanRecordId, ContentReportId, EmailRegistrationTokenId, MediaId, PlaylistId,
    ReviewRequestId, RoomId, TypedId, UserId,
};
pub use media::{
    provider_type_code_from_name, provider_type_codes_from_names, provider_type_name_from_code,
    Danmaku, DirectMultimodeParams, FromProviderParams, Media, MediaListQuery, MediaListSortBy,
    PlaybackInfo, PlaybackResult, PlaybackUrl, PlaybackUrlMetadata, ProviderType, ProviderTypeName,
    ProviderTypeNames, Subtitle, SubtitleUrl,
};
pub use notification::{
    CreateNotificationRequest, MarkAllAsReadRequest, MarkAsReadRequest, Notification,
    NotificationListQuery, NotificationListSortBy, NotificationType,
};
pub use oauth2_client::{
    oauth2_provider_type_code_from_name, oauth2_provider_type_name_from_code,
    OAuth2AuthUrlResponse, OAuth2CallbackRequest, OAuth2CallbackResponse, OAuth2Provider,
    OAuth2ProviderTypeName, OAuth2UserInfo, UserOAuthProviderMapping,
};
pub use opaque_password::{
    OpaquePasswordRecord, OPAQUE_CIPHERSUITE_RISTRETTO255_SHA512_ARGON2ID,
    OPAQUE_SERVER_SETUP_VERSION,
};
pub use pagination::{Page, PageParams, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
pub use permission::{
    Role as RoomRole, RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMemberPermissionBits,
    RoomPermission, RoomPermissionSet,
};
pub use playback::{
    hash_playback_target, ClaimedPlaybackDurationProbe, PlaybackDurationSource,
    PlaybackDurationStatus, PlaybackSourceIdentity, PlaybackSourceMetadata, RoomPlaybackProgress,
    RoomPlaybackState,
};
pub use playlist::{
    CreatePlaylistRequest, Playlist, PlaylistListQuery, PlaylistListSortBy, PlaylistWithCount,
    UpdatePlaylistRequest,
};
pub use provider_instance::{
    is_valid_provider_instance_name, normalize_provider_instance_name,
    normalize_provider_instance_name_owned, resolve_provider_instance_binding,
    validate_provider_instance_name, CredentialProviderInstanceName, NewProviderInstance,
    ProviderCredential, ProviderInstance, ProviderInstanceBindingMismatch,
    ProviderInstanceListQuery, ProviderInstanceListSortBy, UserProviderCredential,
};
pub use query::SortDirection;
pub use review::ReviewStatus;
pub use room::{
    AutoPlaySettings, CreateRoomRequest, PlayMode, Room, RoomListQuery, RoomListSortBy,
    RoomSettingsJson, RoomStatus, RoomWithCount, UpdateRoomRequest,
};
pub use room_member::{
    AddMemberOptions, MemberStatus, MyRoomListQuery, MyRoomListSortBy, MyRoomRelation, RoomMember,
    RoomMemberListQuery, RoomMemberListSortBy, RoomMemberWithUser,
};
pub use room_settings::RoomSettings;
pub use settings::{get_default_settings, SettingsGroup};
pub use user::{
    CreateUserRequest, SignupMethod, UpdateUserRequest, User, UserListQuery, UserListSortBy,
    UserRole, UserStatus,
};
pub use user_preferences::{
    UserAuthFactors, UserNotificationPreferences, UserPreferences, UserPreferencesUpdate,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_sort_field<T>(value: &str) -> T
    where
        T: std::str::FromStr,
        T::Err: std::fmt::Display,
    {
        match value.parse::<T>() {
            Ok(parsed) => parsed,
            Err(error) => std::panic::panic_any(format!("sort field should parse: {error}")),
        }
    }

    macro_rules! assert_sort_field {
        ($ty:ty, $variant:expr, $display:literal, $sql:literal) => {
            assert_eq!($variant.as_str(), $display);
            assert_eq!($variant.to_string(), $display);
            assert_eq!($variant.as_sql(), $sql);
            assert_eq!(parse_sort_field::<$ty>($display), $variant);
        };
    }

    #[test]
    fn list_sort_fields_roundtrip_display_parse_and_sql() {
        assert_sort_field!(MediaListSortBy, MediaListSortBy::Name, "name", "name");
        assert_sort_field!(
            MediaListSortBy,
            MediaListSortBy::ProviderInstanceName,
            "provider_instance_name",
            "provider_instance_name"
        );
        assert_eq!(MediaListSortBy::default(), MediaListSortBy::Position);

        assert_sort_field!(
            PlaylistListSortBy,
            PlaylistListSortBy::CreatedAt,
            "created_at",
            "created_at"
        );
        assert_eq!(PlaylistListSortBy::default(), PlaylistListSortBy::Position);

        assert_sort_field!(
            RoomListSortBy,
            RoomListSortBy::LastActivityAt,
            "last_activity_at",
            "r.last_activity_at"
        );
        assert_eq!(RoomListSortBy::default(), RoomListSortBy::CreatedAt);

        assert_sort_field!(
            RoomMemberListSortBy,
            RoomMemberListSortBy::JoinedAt,
            "joined_at",
            "rm.joined_at"
        );
        assert_eq!(
            RoomMemberListSortBy::default(),
            RoomMemberListSortBy::JoinedAt
        );

        assert_sort_field!(
            MyRoomListSortBy,
            MyRoomListSortBy::LastActivityAt,
            "last_activity_at",
            "r.last_activity_at"
        );
        assert_eq!(MyRoomListSortBy::default(), MyRoomListSortBy::JoinedAt);

        assert_sort_field!(
            NotificationListSortBy,
            NotificationListSortBy::UpdatedAt,
            "updated_at",
            "updated_at"
        );
        assert_eq!(
            NotificationListSortBy::default(),
            NotificationListSortBy::CreatedAt
        );

        assert_sort_field!(
            ProviderInstanceListSortBy,
            ProviderInstanceListSortBy::Endpoint,
            "endpoint",
            "endpoint"
        );
        assert_eq!(
            ProviderInstanceListSortBy::default(),
            ProviderInstanceListSortBy::CreatedAt
        );

        assert_sort_field!(
            UserListSortBy,
            UserListSortBy::UpdatedAt,
            "updated_at",
            "updated_at"
        );
        assert_eq!(UserListSortBy::default(), UserListSortBy::CreatedAt);
    }
}
