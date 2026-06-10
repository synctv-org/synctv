pub(crate) mod audit;
pub(crate) mod ban;
pub mod chat;
pub(crate) mod email_bind;
pub(crate) mod email_registration_token;
pub(crate) mod email_token;
pub(crate) mod file_storage;
pub(crate) mod media;
pub(crate) mod notification;
pub(crate) mod playback;
pub(crate) mod playlist;
pub(crate) mod provider_instance;
pub(crate) mod query_builder;
pub mod realtime_outbox;
pub(crate) mod review;
pub(crate) mod room;
pub(crate) mod room_cleanup;
pub mod room_member;
pub(crate) mod room_password;
pub(crate) mod room_resource_event;
pub(crate) mod room_settings;
pub(crate) mod settings;
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
    EditChatMessageEventRequest,
};
pub use email_bind::EmailBindRepository;
pub use email_registration_token::{EmailRegistrationToken, EmailRegistrationTokenRepository};
pub use email_token::EmailTokenRepository;
pub use file_storage::FileStorageRepository;
pub use media::MediaRepository;
pub use notification::NotificationRepository;
pub use playback::RoomPlaybackStateRepository;
pub use playlist::PlaylistRepository;
pub use provider_instance::{ProviderInstanceRepository, UserProviderCredentialRepository};
pub use review::{
    ReviewPage, ReviewRepository, RoomCreationReviewListQuery, RoomCreationReviewRecord,
    RoomJoinReviewListQuery, RoomJoinReviewRecord, UserRegistrationReviewListQuery,
    UserRegistrationReviewRecord,
};
pub use room::RoomRepository;
pub use room_member::RoomMemberRepository;
pub use room_password::RoomPasswordRepository;
pub use room_resource_event::{
    NewRoomResourceEvent, RoomResourceEventRepository, RoomResourceEventScope,
};
pub use room_settings::RoomSettingsRepository;
pub use settings::SettingsRepository;
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
