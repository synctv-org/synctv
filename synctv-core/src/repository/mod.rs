pub mod audit;
pub mod chat;
pub mod email_token;
pub mod media;
pub mod notification;
pub mod playback;
pub mod playlist;
pub mod provider_instance;
pub mod query_builder;
pub mod room;
pub mod room_member;
pub mod room_settings;
pub mod settings;
pub mod user;
pub mod user_oauth_provider;

use sha2::{Digest, Sha256};

pub use audit::{AuditLogQuery, AuditLogRepository, AuditLogRow};
pub use chat::ChatRepository;
pub use email_token::EmailTokenRepository;
pub use media::MediaRepository;
pub use notification::NotificationRepository;
pub use playback::RoomPlaybackStateRepository;
pub use playlist::PlaylistRepository;
pub use provider_instance::{ProviderInstanceRepository, UserProviderCredentialRepository};
pub use query_builder::WhereClauseBuilder;
pub use room::{JoinRoomContext, RoomRepository};
pub use room_member::RoomMemberRepository;
pub use room_settings::RoomSettingsRepository;
pub use settings::SettingsRepository;
pub use user::UserRepository;
pub use user_oauth_provider::UserOAuthProviderRepository;

#[must_use]
pub(crate) fn stable_scope_lock_key(primary_scope: &str, secondary_scope: Option<&str>) -> i64 {
    let primary_bits = stable_lock_bits(primary_scope);
    let secondary_bits = secondary_scope.map_or(0, stable_lock_bits);
    (i64::from(primary_bits) << 31) | i64::from(secondary_bits)
}

#[must_use]
fn stable_lock_bits(value: &str) -> u32 {
    let digest = Sha256::digest(value.as_bytes());
    u32::from_be_bytes([digest[0], digest[1], digest[2], digest[3]]) & 0x7FFF_FFFF
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_stable_scope_lock_key_matches_fixed_contract() {
        assert_eq!(
            super::stable_scope_lock_key("room12345678", Some("parent123456")),
            3_505_116_572_805_754_167
        );
        assert_eq!(
            super::stable_scope_lock_key("room22222222", None),
            2_580_516_346_865_385_472
        );
    }

    #[test]
    fn test_stable_scope_lock_key_changes_with_scope_components() {
        let base = super::stable_scope_lock_key("room11111111", Some("parent111111"));
        assert_ne!(
            base,
            super::stable_scope_lock_key("room11111111", Some("parent222222"))
        );
        assert_ne!(
            base,
            super::stable_scope_lock_key("room22222222", Some("parent111111"))
        );
        assert_ne!(base, super::stable_scope_lock_key("room11111111", None));
    }
}
