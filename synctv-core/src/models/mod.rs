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

macro_rules! i16_enum {
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
pub mod playback_session;
pub mod playlist;
pub mod provider_instance;
pub mod provider_target;
pub mod query;
pub mod realtime_event;
pub mod review;
pub mod room;
pub mod room_member;
pub mod room_settings;
pub mod settings;
pub mod source_config;
pub mod user;
pub mod user_preferences;
pub mod validation;

pub use audit::{AuditAction, AuditDetails, AuditTargetType, AuditUpdatedFields};
pub use chat::{
    ChatAttachment, ChatAttachmentKind, ChatAttachmentUploadSession, ChatEventKind,
    ChatHistoryCursor, ChatHistoryPage, ChatMemberJoinedMetadata, ChatMention, ChatMentionInput,
    ChatMessage, ChatMessageContext, ChatMessageEvent, ChatMessageEventLog,
    ChatMessageOperationKind, ChatMessagePin, ChatMessageReadReceiptMember,
    ChatMessageReadReceiptUser, ChatMessageReadReceiptsPage, ChatMessageSelection,
    ChatMessageStatus, ChatMessageType, ChatMessageWithAttachments, ChatMetadata, ChatPinEvent,
    ChatPinEventKind, ChatPinEventLog, ChatPinnedMessage, ChatPlaybackChangedMetadata,
    ChatPlaybackMessagesQuery, ChatPlaybackMetadata, ChatPresentationMetadata, ChatReaction,
    ChatReactionSummary, ChatReactionUser, ChatReactionUsersCursor, ChatReactionUsersPage,
    ChatReadState, ChatReadStateWithUnread, ChatSearchMessagesPage, ChatSearchMessagesQuery,
    ChatUserMetadata, CreateChatAttachmentUploadSession, DeleteChatMessage, EditChatMessage,
    EventCursor, MarkChatRead, PinChatMessage, PlaybackChangeReason, SendChatMessage,
    SendChatRequest, SetChatReaction, UnpinChatMessage, CHAT_ATTACHMENT_FILENAME_MAX_CHARS,
    CHAT_ATTACHMENT_ID_MAX_CHARS, CHAT_CLIENT_MESSAGE_ID_MAX_CHARS,
    CHAT_CLIENT_OPERATION_ID_MAX_CHARS, CHAT_EVENT_ID_MAX_CHARS, CHAT_EVENT_TYPE_MAX_CHARS,
    CHAT_PIN_NOTE_MAX_CHARS, CHAT_REACTION_KEY_MAX_CHARS,
};
pub use content_report::{
    ContentReport, ContentReportAdminRow, ContentReportMetadata, ContentReportStatus,
    ContentReportTarget, ContentReportTargetType, CreateContentReport,
};
pub use email_token::EmailTokenType;
pub use file_storage::{
    CompleteFileUploadPart, CompleteFileUploadSession, CompleteFileUploadSessionResult,
    CreateFileUploadSession, FileAudioMetadata, FileBlob, FileBlobCompression, FileBlobPart,
    FileByteRange, FileCleanupJob, FileCleanupMetadata, FileMetadata, FileObject, FileObjectAccess,
    FileObjectData, FileObjectDownload, FileObjectGroup, FileObjectKind, FileObjectMetadata,
    FileObjectVariant, FileOwnershipProofRange, FileRangeRequest, FileReferenceMetadata,
    FileReferenceTarget, FileUploadManifestPart, FileUploadOwnershipProofMetadata,
    FileUploadPartUrl, FileUploadPlan, FileUploadPlanPart, FileUploadPolicy, FileUploadRange,
    FileUploadSession, FileUploadSessionCreateResult, FileUploadSessionKind,
    FileUploadSessionMetadata, FileUploadSessionPart, FileUploadSessionRecord, FileVariantMetadata,
    GetFileObject, NewStoredFile, StoreFileUpload, StoreFileUploadResult, StoredFileReference,
    SubmittedFileReference, SubmittedFileReferenceKind, FILE_CLEANUP_ORIGIN_MAX_CHARS,
    FILE_ID_MAX_CHARS, FILE_OBJECT_KEY_MAX_CHARS, FILE_REFERENCE_ID_MAX_CHARS,
    FILE_REFERENCE_KIND_MAX_CHARS, FILE_SHA256_HEX_CHARS, FILE_STORAGE_BACKEND_MAX_CHARS,
};
pub use id::{
    generate_id, BanRecordId, ContentReportId, EmailRegistrationTokenId, MediaId, PlaylistId,
    ReviewRequestId, RoomCategoryId, RoomId, RoomLabelId, TypedId, UserId,
    LOCAL_MANAGEMENT_ACTOR_USER_ID,
};
pub use media::{
    provider_type_code_from_name, provider_type_codes_from_names, provider_type_name_from_code,
    AcFunPlaybackFormat, AcFunPlaybackMetadata, AcFunPlaybackResourceKind, AlistPlaybackMetadata,
    AlistTranscodingTaskMetadata, AlistVideoPreviewMetadata, BilibiliDashAudioStream,
    BilibiliDashManifest, BilibiliDashManifestSlot, BilibiliDashManifests, BilibiliDashSegmentBase,
    BilibiliDashVideoStream, BilibiliPlaybackMetadata, CctvChapterMetadata, CctvPlaybackMetadata,
    CctvPlaybackStreamKind, DirectMultimodeParams, DirectUrlPlaybackMetadata,
    DouyinPlaybackMetadata, DouyinPlaybackResource, DouyuPlaybackCodec, DouyuPlaybackFormat,
    DouyuPlaybackMetadata, EmbyPlaybackMetadata, FnosAudioTrackMetadata, FnosFilePlaybackMetadata,
    FnosMediaPlaybackMetadata, FnosPlaybackMetadata, FnosProxyResource, FnosSubtitleTrackMetadata,
    FnosTranscodeResource, FromProviderParams, HuyaPlaybackFormat, HuyaPlaybackMetadata,
    HuyaPlaybackResourceKind, LivePlaybackMetadata, LiveProxyPlaybackMetadata, Media,
    MediaListQuery, MediaListSortBy, NextcloudPlaybackMetadata, PlaybackAcFunDanmaku,
    PlaybackAcFunMedia, PlaybackAlistMedia, PlaybackAlistSubtitle, PlaybackBilibiliDanmaku,
    PlaybackBilibiliMedia, PlaybackBilibiliSubtitle, PlaybackCctvMedia, PlaybackCloudreveMedia,
    PlaybackCloudreveSubtitle, PlaybackDanmaku, PlaybackDanmakuProvider, PlaybackDirectUrlDanmaku,
    PlaybackDirectUrlMedia, PlaybackDirectUrlSubtitle, PlaybackDouyinDanmaku, PlaybackDouyinMedia,
    PlaybackDouyuDanmaku, PlaybackDouyuMedia, PlaybackEmbyMedia, PlaybackEmbySubtitle,
    PlaybackFnosMedia, PlaybackFnosSubtitle, PlaybackHuyaDanmaku, PlaybackHuyaMedia, PlaybackInfo,
    PlaybackLiveProxyMedia, PlaybackMedia, PlaybackMediaMetadata, PlaybackMediaProvider,
    PlaybackMetadata, PlaybackNextcloudMedia, PlaybackNextcloudSubtitle, PlaybackQnapMedia,
    PlaybackQnapSubtitle, PlaybackResult, PlaybackRtmpMedia, PlaybackSeafileMedia,
    PlaybackSeafileSubtitle, PlaybackSubtitle, PlaybackSubtitleProvider, PlaybackSynologyMedia,
    PlaybackSynologySubtitle, PlaybackTikTokMedia, PlaybackTikTokSubtitle, PlaybackTrueNasMedia,
    PlaybackTrueNasSubtitle, PlaybackTwitchDanmaku, PlaybackTwitchMedia, PlaybackYoutubeMedia,
    PlaybackYoutubeSubtitle, ProviderType, QnapPlaybackMetadata, QnapPlaybackMode,
    QnapPlaybackResource, SeafilePlaybackMetadata, SourceProvider, SynologyAudioTrackMetadata,
    SynologyPlaybackMetadata, SynologyPlaybackProfile, SynologyPlaybackResource,
    SynologySubtitleMetadata, TikTokPlaybackMetadata, TikTokPlaybackResource,
    TrueNasPlaybackMetadata, TwitchChapterMetadata, TwitchPlaybackMetadata,
    TwitchPlaybackResourceKind, YoutubePlaybackMetadata, YoutubePlaybackResource,
};
pub use notification::{
    CreateNotificationRequest, MarkAllAsReadRequest, MarkAsReadRequest, Notification,
    NotificationData, NotificationListQuery, NotificationListSortBy, NotificationType,
};
pub use oauth2_client::{
    oauth2_provider_type_code_from_name, oauth2_provider_type_name_from_code,
    OAuth2AuthUrlResponse, OAuth2CallbackRequest, OAuth2CallbackResponse, OAuth2Provider,
    OAuth2UserInfo, UserOAuthProviderMapping,
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
    hash_empty_playback_target, try_hash_playback_target, ClaimedPlaybackDurationProbe,
    PlaybackDurationSource, PlaybackDurationStatus, PlaybackHistoryEntry, PlaybackHistoryPage,
    PlaybackKind, PlaybackSourceIdentity, PlaybackSourceMetadata, RoomPlaybackProgress,
    RoomPlaybackState,
};
pub use playback_session::{
    EmbyPlaybackSession, FnosPlaybackSession, ProviderPlaybackSession,
    ProviderPlaybackSessionRecord, ProviderPlaybackSessionState, ProviderPlaybackStopReason,
    SynologyPlaybackSession,
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
    ProviderInstanceListQuery, ProviderInstanceListSortBy, SynologyApiBinding,
    UserProviderCredential,
};
pub use provider_target::{
    hash_empty_provider_target, hash_optional_provider_target, AlistTarget, BilibiliTarget,
    CloudreveTarget, EmbyTarget, FnosTarget, FnosTargetKind, NextcloudTarget, ProviderTarget,
    QnapTarget, SeafileTarget, SynologyTarget, TikTokTarget, TrueNasTarget, TwitchTarget,
    TwitchTargetKind, YoutubeTarget,
};
pub use query::SortDirection;
pub use realtime_event::{
    CacheTarget, NotificationLevel, RealtimeDeliveryRoute, RealtimeEvent, WebRTCSignalKind,
};
pub use review::ReviewStatus;
pub use room::{
    AutoPlaySettings, CreateRoomRequest, PlayMode, Room, RoomCategory, RoomLabel, RoomListQuery,
    RoomListSortBy, RoomStatus, RoomWithCount, UpdateRoomRequest, UpsertRoomCategory,
    UpsertRoomLabel,
};
pub use room_member::{
    AddMemberOptions, MemberStatus, MyRoomListQuery, MyRoomListSortBy, MyRoomRelation, RoomMember,
    RoomMemberListQuery, RoomMemberListSortBy, RoomMemberWithUser,
};
pub use room_settings::RoomSettings;
pub use settings::RuntimeSetting;
pub use source_config::{
    detect_direct_url_format, AcFunMediaSourceConfig, AlistMediaSourceConfig,
    AlistPlaylistSourceConfig, BilibiliHistoryType, BilibiliLiveSourceConfig,
    BilibiliMediaSourceConfig, BilibiliPgcSourceConfig, BilibiliPgcTimelineType,
    BilibiliPlaylistSource, BilibiliPlaylistSourceConfig, BilibiliVideoSourceConfig,
    CctvMediaSourceConfig, CloudreveMediaSourceConfig, CloudrevePlaylistSourceConfig,
    DirectUrlDanmakuSourceConfig, DirectUrlMediaResourceConfig, DirectUrlMediaSourceConfig,
    DirectUrlSubtitleSourceConfig, DouyinMediaSourceConfig, DouyinPlaylistSourceConfig,
    DouyuMediaSourceConfig, EmbyMediaSourceConfig, EmbyPlaylistSource, EmbyPlaylistSourceConfig,
    ExternalLiveSourceConfig, FnosMediaSource, FnosMediaSourceConfig, FnosPlaylistSource,
    FnosPlaylistSourceConfig, HuyaMediaSourceConfig, LiveProxyMediaSourceConfig, MediaSourceConfig,
    NextcloudMediaSourceConfig, NextcloudPlaylistSource, NextcloudPlaylistSourceConfig,
    PlaylistSourceConfig, QnapMediaSourceConfig, QnapPlaylistSourceConfig, RtmpMediaSourceConfig,
    RtmpStreamMode, RtspTrackSelection, RtspTransport, SeafileMediaSourceConfig,
    SeafilePlaylistSource, SeafilePlaylistSourceConfig, SynologyLibraryItemKind,
    SynologyMediaSource, SynologyMediaSourceConfig, SynologyPlaylistSource,
    SynologyPlaylistSourceConfig, TikTokMediaSourceConfig, TikTokPlaylistSourceConfig,
    TrueNasMediaSourceConfig, TrueNasPlaylistSource, TrueNasPlaylistSourceConfig,
    TwitchMediaSourceConfig, TwitchPlaylistContent, TwitchPlaylistSourceConfig,
    YoutubeChannelContent, YoutubeMediaSourceConfig, YoutubePlaylistSourceConfig,
};
pub use user::{
    CreateUserRequest, SignupMethod, UpdateUserRequest, User, UserListQuery, UserListSortBy,
    UserRole, UserStatus,
};
pub use user_preferences::{
    UserAuthFactors, UserNotificationPreferences, UserPreferences, UserPreferencesUpdate,
};
pub use validation::SettingsValidationContext;

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
        assert_eq!(MyRoomListSortBy::default(), MyRoomListSortBy::Frequent);

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

    #[test]
    fn source_provider_round_trips_db_code_and_wire_name() {
        const PROVIDERS: &[(SourceProvider, i16, &str)] = &[
            (SourceProvider::DirectUrl, 1, "direct_url"),
            (SourceProvider::Bilibili, 2, "bilibili"),
            (SourceProvider::Alist, 3, "alist"),
            (SourceProvider::Emby, 4, "emby"),
            (SourceProvider::Rtmp, 5, "rtmp"),
            (SourceProvider::LiveProxy, 6, "live_proxy"),
            (SourceProvider::Cloudreve, 7, "cloudreve"),
            (SourceProvider::Twitch, 8, "twitch"),
            (SourceProvider::Huya, 9, "huya"),
            (SourceProvider::Douyu, 10, "douyu"),
            (SourceProvider::Douyin, 11, "douyin"),
            (SourceProvider::AcFun, 12, "acfun"),
            (SourceProvider::Cctv, 13, "cctv"),
            (SourceProvider::Fnos, 14, "fnos"),
            (SourceProvider::Qnap, 15, "qnap"),
            (SourceProvider::Synology, 16, "synology"),
            (SourceProvider::Nextcloud, 17, "nextcloud"),
            (SourceProvider::Seafile, 18, "seafile"),
            (SourceProvider::TrueNas, 19, "truenas"),
            (SourceProvider::Youtube, 20, "youtube"),
            (SourceProvider::TikTok, 21, "tiktok"),
        ];

        assert_eq!(SourceProvider::ALL.len(), PROVIDERS.len());
        for ((provider, code, name), listed_provider) in
            PROVIDERS.iter().zip(SourceProvider::ALL.iter())
        {
            assert_eq!(provider, listed_provider);
            assert_eq!(provider.as_i16(), *code);
            assert_eq!(i16::from(*provider), *code);
            assert_eq!(SourceProvider::try_from(*code), Ok(*provider));
            assert_eq!(provider.as_str(), *name);
            assert_eq!(provider.to_string(), *name);
            assert_eq!(name.parse::<SourceProvider>(), Ok(*provider));
        }

        assert!("directurl".parse::<SourceProvider>().is_err());
        assert!("liveproxy".parse::<SourceProvider>().is_err());
    }
}
