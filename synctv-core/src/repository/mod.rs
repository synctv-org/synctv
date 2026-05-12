pub mod audit;
pub mod ban;
pub mod chat;
pub mod email_token;
pub mod media;
pub mod notification;
pub mod playback;
pub mod playlist;
pub mod provider_instance;
pub mod query_builder;
pub mod realtime_outbox;
pub mod review;
pub mod room;
pub mod room_member;
pub mod room_settings;
pub mod settings;
pub mod user;
pub mod user_oauth_provider;
pub mod user_preferences;
pub mod webauthn_credential;

use sha2::{Digest, Sha256};

pub use audit::{AuditLogQuery, AuditLogRepository, AuditLogRow};
pub use ban::{
    BanRecordListQuery, BanRecordPage, BanRecordRepository, BanRecordRow, BanRecordTargetType,
};
pub use chat::ChatRepository;
pub use email_token::EmailTokenRepository;
pub use media::MediaRepository;
pub use notification::NotificationRepository;
pub use playback::RoomPlaybackStateRepository;
pub use playlist::PlaylistRepository;
pub use provider_instance::{ProviderInstanceRepository, UserProviderCredentialRepository};
pub use query_builder::WhereClauseBuilder;
pub use review::{
    ReviewPage, ReviewRepository, RoomCreationReviewListQuery, RoomCreationReviewRecord,
    RoomJoinReviewListQuery, RoomJoinReviewRecord, UserRegistrationReviewListQuery,
    UserRegistrationReviewRecord,
};
pub use room::{JoinRoomContext, RoomRepository};
pub use room_member::RoomMemberRepository;
pub use room_settings::RoomSettingsRepository;
pub use settings::SettingsRepository;
pub use user::{PasswordCredentialMaterial, UserRepository};
pub use user_oauth_provider::UserOAuthProviderRepository;
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
