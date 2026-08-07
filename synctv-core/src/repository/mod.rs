pub(crate) mod audit;
pub(crate) mod ban;
pub mod chat;
pub(crate) mod content_report;
pub(crate) mod email_bind;
pub mod email_outbox;
pub(crate) mod email_registration_token;
pub(crate) mod email_token;
pub(crate) mod file_storage;
pub(crate) mod jsonb;
pub(crate) mod media;
pub(crate) mod notification;
pub(crate) mod playback;
pub(crate) mod playback_history;
pub(crate) mod playback_session;
pub(crate) mod playback_source_metadata;
pub(crate) mod playlist;
pub(crate) mod pools;
pub(crate) mod provider_instance;
pub mod query_builder;
pub mod realtime_outbox;
pub(crate) mod review;
pub(crate) mod room;
pub(crate) mod room_cleanup;
pub mod room_member;
pub(crate) mod room_password;
pub(crate) mod room_resource_event;
pub(crate) mod room_settings;
pub(crate) mod room_taxonomy;
pub(crate) mod settings;
mod sqlx_types;
pub(crate) mod system_stats;
pub(crate) mod totp_credential;
pub(crate) mod user;
pub(crate) mod user_email;
pub(crate) mod user_oauth_provider;
pub(crate) mod user_password;
pub(crate) mod user_preferences;
pub(crate) mod webauthn_credential;

use sha2::{Digest, Sha256};

pub use audit::{AuditLogQuery, AuditLogRepository, AuditLogRow};
pub use ban::{
    BanRecordListQuery, BanRecordPage, BanRecordRepository, BanRecordRow, BanRecordTargetType,
};
pub use chat::{
    ChatMessageOperationIdempotency, ChatRepository, DeleteChatMessageEventRequest,
    EditChatMessageEventRequest, PinChatMessageEventRequest, UnpinChatMessageEventRequest,
};
pub use content_report::{
    ContentReportListQuery, ContentReportListScope, ContentReportPage, ContentReportRepository,
};
pub use email_bind::EmailBindRepository;
pub use email_outbox::{
    EmailOutboxJob, EmailOutboxKind, EmailOutboxRepository, EmailOutboxStatus, NewEmailOutboxJob,
};
pub use email_registration_token::{EmailRegistrationToken, EmailRegistrationTokenRepository};
pub use email_token::EmailTokenRepository;
pub use file_storage::{
    FileStorageRepository, UpsertFileBlob, UpsertFileBlobPart, UpsertFileObject,
    UpsertFileObjectGroup, UpsertFileObjectVariant, UpsertFileUploadSession,
    UpsertFileUploadSessionPart,
};
pub(crate) use jsonb::{JsonbArray, OptionalJsonbArray};
pub use media::{MediaListItem, MediaRepository};
pub use notification::NotificationRepository;
pub use playback::RoomPlaybackStateRepository;
pub use playback_history::{
    AppendPlaybackHistoryEntry, PlaybackHistoryDirection, PlaybackHistoryRepository,
};
pub use playback_session::{NewProviderPlaybackSession, ProviderPlaybackSessionRepository};
pub use playback_source_metadata::PlaybackSourceMetadataRepository;
pub use playlist::{PlaylistListItem, PlaylistRepository};
pub use provider_instance::{ProviderInstanceRepository, UserProviderCredentialRepository};
pub use review::{
    ReviewPage, ReviewRepository, RoomCreationReviewListQuery, RoomCreationReviewRecord,
    RoomJoinReviewListQuery, RoomJoinReviewRecord, UserRegistrationReviewListQuery,
    UserRegistrationReviewRecord,
};
pub use room::{RoomDiscoveryViewerState, RoomRepository};
pub use room_member::RoomMemberRepository;
pub use room_password::RoomPasswordRepository;
pub use room_resource_event::{
    NewRoomResourceEvent, RoomMemberResourceSummary, RoomResourceEventLog,
    RoomResourceEventPayload, RoomResourceEventRepository, RoomResourceEventScope,
    RoomResourceEventSummary, RoomResourceEventSummaryDetails, RoomResourceKind,
};
pub use room_settings::RoomSettingsRepository;
pub use room_taxonomy::{RoomTaxonomyAssignment, RoomTaxonomyRepository};
pub use settings::SettingsRepository;
pub use system_stats::{SystemStats, SystemStatsRepository};
pub use totp_credential::{TotpCredential, TotpCredentialRepository};
pub use user::UserRepository;
pub use user_email::{UserEmailRepository, UserWithEmail};
pub use user_oauth_provider::UserOAuthProviderRepository;
pub use user_password::{PasswordCredentialMaterial, UserPasswordRepository};
pub use user_preferences::UserPreferencesRepository;
pub use webauthn_credential::{WebAuthnCredential, WebAuthnCredentialRepository};

#[must_use]
pub(crate) fn stable_scope_lock_key(primary_scope: i64, secondary_scope: Option<i64>) -> i64 {
    let primary_bits = stable_lock_bits(primary_scope);
    let secondary_bits = secondary_scope.map_or(0, stable_lock_bits);
    (i64::from(primary_bits) << 31) | i64::from(secondary_bits)
}

pub(crate) fn required_count(value: Option<i64>, query_description: &str) -> crate::Result<i64> {
    value.ok_or_else(|| {
        crate::Error::Internal(format!(
            "{query_description} COUNT query returned no scalar value"
        ))
    })
}

#[must_use]
fn stable_lock_bits(value: i64) -> u32 {
    let digest = Sha256::digest(value.to_be_bytes());
    u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) & 0x7FFF_FFFF
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_stable_scope_lock_key_matches_fixed_contract() {
        assert_eq!(
            super::stable_scope_lock_key(1, Some(2)),
            super::stable_scope_lock_key(1, Some(2))
        );
        assert_eq!(
            super::stable_scope_lock_key(2, None),
            super::stable_scope_lock_key(2, None)
        );
    }

    #[test]
    fn test_stable_scope_lock_key_changes_with_scope_components() {
        let base = super::stable_scope_lock_key(1, Some(1));
        assert_ne!(base, super::stable_scope_lock_key(1, Some(2)));
        assert_ne!(base, super::stable_scope_lock_key(2, Some(1)));
        assert_ne!(base, super::stable_scope_lock_key(1, None));
    }
}
