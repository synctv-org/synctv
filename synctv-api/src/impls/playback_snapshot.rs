use async_trait::async_trait;
use sha2::{Digest, Sha256};
use synctv_core::models::{RoomId, RoomPlaybackState, UserId};
use synctv_core::provider::ProviderCredentialDependency;
use synctv_core::repository::UserProviderCredentialRepository;

use crate::impls::ApiError;

#[async_trait]
pub trait PlaybackSnapshotService: Send + Sync {
    async fn get_playback_snapshot(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        state: &RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<crate::proto::client::PlaybackSnapshot, ApiError>;

    async fn playback_credential_dependencies(
        &self,
        _user_id: &UserId,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) -> Result<Vec<ProviderCredentialDependency>, ApiError> {
        Ok(Vec::new())
    }

    async fn playback_snapshot_version_for_state(
        &self,
        _user_id: &UserId,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) -> Result<Option<String>, ApiError> {
        Ok(None)
    }
}

pub(crate) fn static_playback_snapshot_version(media: &synctv_core::models::Media) -> String {
    media.version.to_string()
}

pub(crate) fn dynamic_playback_snapshot_version(
    playlist: &synctv_core::models::Playlist,
) -> String {
    playlist.version.to_string()
}

pub(crate) fn compose_playback_snapshot_version(
    base_version: impl AsRef<str>,
    credential_fingerprint: Option<&str>,
) -> String {
    match credential_fingerprint {
        Some(fingerprint) => format!("{}:cred:{fingerprint}", base_version.as_ref()),
        None => base_version.as_ref().to_string(),
    }
}

pub(crate) async fn provider_credential_dependency_fingerprint(
    repo: Option<&UserProviderCredentialRepository>,
    dependencies: &[ProviderCredentialDependency],
) -> Result<Option<String>, ApiError> {
    if dependencies.is_empty() {
        return Ok(None);
    }

    let Some(repo) = repo else {
        return Ok(Some("missing-repository".to_string()));
    };

    let mut dependencies = dependencies.to_vec();
    dependencies.sort_by(|left, right| {
        (&left.provider, &left.user_id, &left.server_id).cmp(&(
            &right.provider,
            &right.user_id,
            &right.server_id,
        ))
    });

    let mut hasher = Sha256::new();
    for dependency in dependencies {
        hasher.update(dependency.provider.as_bytes());
        hasher.update(b"\0");
        hasher.update(dependency.user_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(dependency.server_id.as_bytes());
        hasher.update(b"\0");

        let credential = repo
            .get_by_provider_and_server(
                &dependency.user_id,
                &dependency.provider,
                &dependency.server_id,
            )
            .await
            .map_err(ApiError::from)?;
        match credential {
            Some(credential) => {
                let state = if credential.is_expired() {
                    "expired"
                } else {
                    "active"
                };
                hasher.update(state.as_bytes());
                hasher.update(b"\0");
                hasher.update(
                    synctv_core::provider::credential_resolver::credential_revision(
                        &credential.id,
                        credential.updated_at,
                    )
                    .as_bytes(),
                );
            }
            None => hasher.update(b"missing"),
        }
        hasher.update(b"\0");
    }

    Ok(Some(
        hex::encode(hasher.finalize()).chars().take(24).collect(),
    ))
}

pub(crate) fn playback_snapshot_expires_at(
    snapshot: &crate::proto::client::PlaybackSnapshot,
) -> Option<i64> {
    snapshot
        .playback_infos
        .values()
        .flat_map(|info| info.urls.iter().filter_map(|url| url.expire_at))
        .min()
}

#[cfg(test)]
mod tests {
    #[test]
    fn compose_playback_snapshot_version_appends_credential_fingerprint() {
        assert_eq!(
            super::compose_playback_snapshot_version("7", Some("abc123")),
            "7:cred:abc123"
        );
        assert_eq!(super::compose_playback_snapshot_version("7", None), "7");
    }

    #[tokio::test]
    async fn provider_credential_dependency_fingerprint_ignores_empty_dependencies() {
        let fingerprint = super::provider_credential_dependency_fingerprint(None, &[])
            .await
            .expect("empty dependency list should not require a repository");

        assert_eq!(fingerprint, None);
    }
}
