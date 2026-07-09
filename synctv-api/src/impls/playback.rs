use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use synctv_core::models::{PlaybackSourceIdentity, RoomId, RoomPlaybackState, UserId};
use synctv_core::provider::{ProviderCredentialDependency, ProviderStoreResolver, StoreError};

use crate::impls::ApiError;

#[async_trait]
pub trait PlaybackService: Send + Sync {
    async fn room_playback_state(&self, room_id: &RoomId) -> Result<RoomPlaybackState, ApiError>;

    async fn get_playback(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        state: &RoomPlaybackState,
        playback_client_profile: Option<&synctv_core::provider::PlaybackClientProfile>,
    ) -> Result<synctv_proto::client::Playback, ApiError>;

    async fn playback_credential_dependencies(
        &self,
        _user_id: &UserId,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) -> Result<Vec<ProviderCredentialDependency>, ApiError> {
        Ok(Vec::new())
    }

    async fn handle_provider_lifecycle_transition(
        &self,
        _previous: Option<&RoomPlaybackState>,
        _current: &RoomPlaybackState,
    ) {
    }

    async fn report_provider_playback_progress(
        &self,
        _state: &RoomPlaybackState,
        _position: f64,
        _is_paused: bool,
        _force: bool,
    ) {
    }

    async fn refresh_observed_playback_metadata_and_auto_advance(
        &self,
        _room_id: &RoomId,
        _state: &RoomPlaybackState,
    ) {
    }
}

pub(crate) fn playback_expires_at(playback: &synctv_proto::client::Playback) -> Option<i64> {
    playback
        .playback_infos
        .values()
        .flat_map(|info| info.medias.iter().filter_map(|media| media.expire_at))
        .min()
}

pub(crate) const fn playback_generation_error_allows_state_only(error: &ApiError) -> bool {
    matches!(
        error,
        ApiError::ServiceUnavailable(_) | ApiError::Timeout(_)
    )
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct ResolvedPlaybackSourceMetadata {
    pub duration_seconds: Option<f64>,
    pub is_live: bool,
}

#[derive(Debug, Clone, Copy)]
struct ProviderSourceMetadataObservation {
    is_live: bool,
    duration_seconds: Option<f64>,
}

const SOURCE_METADATA_WRITE_GATE_TTL: Duration = Duration::from_mins(1);
static SOURCE_METADATA_WRITE_L1_GATE: LazyLock<moka::future::Cache<String, ()>> =
    LazyLock::new(|| {
        moka::future::Cache::builder()
            .max_capacity(100_000)
            .time_to_live(SOURCE_METADATA_WRITE_GATE_TTL)
            .build()
    });

fn positive_duration(duration_seconds: Option<f64>) -> Option<f64> {
    duration_seconds.filter(|duration| duration.is_finite() && *duration > 0.0)
}

impl ProviderSourceMetadataObservation {
    fn new(provider_is_live: Option<bool>, provider_duration_seconds: Option<f64>) -> Option<Self> {
        provider_is_live.map(|is_live| Self {
            is_live,
            duration_seconds: if is_live {
                None
            } else {
                positive_duration(provider_duration_seconds)
            },
        })
    }

    fn gate_key(self, identity: &PlaybackSourceIdentity) -> String {
        let duration_bucket = self.duration_seconds.map_or_else(
            || "none".to_string(),
            |duration| duration.to_bits().to_string(),
        );
        format!(
            "metadata:{}:{}:{}:{}:{}:{}",
            identity.room_id.as_i64(),
            identity.media_id.map_or(0, |id| id.as_i64()),
            identity.playlist_id.map_or(0, |id| id.as_i64()),
            identity.target_hash,
            self.is_live,
            duration_bucket
        )
    }
}

async fn persist_provider_source_metadata(
    playback_service: &synctv_core::service::PlaybackService,
    identity: &PlaybackSourceIdentity,
    observation: ProviderSourceMetadataObservation,
) -> Result<(), ApiError> {
    if observation.is_live {
        playback_service
            .upsert_provider_playback_source_metadata(identity, true, None)
            .await
            .map_err(ApiError::from)?;
    } else if let Some(duration_seconds) = observation.duration_seconds {
        playback_service
            .upsert_provider_playback_source_metadata(identity, false, Some(duration_seconds))
            .await
            .map_err(ApiError::from)?;
    } else {
        playback_service
            .mark_probeable_playback_source_metadata_unknown_if_absent(identity)
            .await
            .map_err(ApiError::from)?;
    }
    Ok(())
}

async fn persist_provider_source_metadata_with_gate(
    playback_service: &synctv_core::service::PlaybackService,
    provider_stores: &dyn ProviderStoreResolver,
    identity: &PlaybackSourceIdentity,
    observation: ProviderSourceMetadataObservation,
) -> Result<(), ApiError> {
    let gate_key = observation.gate_key(identity);
    if SOURCE_METADATA_WRITE_L1_GATE.get(&gate_key).await.is_some() {
        return Ok(());
    }

    let store = provider_stores.load("playback_source_metadata");
    match store.get_raw(&gate_key).await {
        Ok(Some(_)) => {
            SOURCE_METADATA_WRITE_L1_GATE.insert(gate_key, ()).await;
            return Ok(());
        }
        Ok(None) => {}
        Err(error) => {
            tracing::debug!(
                key = %gate_key,
                error = %error,
                "Playback source metadata L2 gate read unavailable; using database throttle"
            );
            persist_provider_source_metadata(playback_service, identity, observation).await?;
            SOURCE_METADATA_WRITE_L1_GATE.insert(gate_key, ()).await;
            return Ok(());
        }
    }

    let lock_key = format!("lock:{gate_key}");
    match store.lock(&lock_key, SOURCE_METADATA_WRITE_GATE_TTL).await {
        Ok(_guard) => {
            persist_provider_source_metadata(playback_service, identity, observation).await?;
            if let Err(error) = store
                .set_raw(&gate_key, b"1", SOURCE_METADATA_WRITE_GATE_TTL)
                .await
            {
                tracing::debug!(
                    key = %gate_key,
                    error = %error,
                    "Playback source metadata L2 gate write unavailable"
                );
            }
            SOURCE_METADATA_WRITE_L1_GATE.insert(gate_key, ()).await;
        }
        Err(StoreError::LockFailed(_)) => {}
        Err(error) => {
            tracing::debug!(
                key = %lock_key,
                error = %error,
                "Playback source metadata L2 gate unavailable; using database throttle"
            );
            persist_provider_source_metadata(playback_service, identity, observation).await?;
            SOURCE_METADATA_WRITE_L1_GATE.insert(gate_key, ()).await;
        }
    }

    Ok(())
}

pub(crate) async fn resolve_playback_source_metadata(
    room_service: &synctv_core::service::RoomService,
    provider_stores: &dyn ProviderStoreResolver,
    identity: PlaybackSourceIdentity,
    provider_is_live: Option<bool>,
    provider_duration_seconds: Option<f64>,
) -> Result<ResolvedPlaybackSourceMetadata, ApiError> {
    let playback_service = room_service.playback_service();
    let observation =
        ProviderSourceMetadataObservation::new(provider_is_live, provider_duration_seconds);
    if let Some(observation) = observation {
        persist_provider_source_metadata_with_gate(
            playback_service,
            provider_stores,
            &identity,
            observation,
        )
        .await?;
    }

    let metadata = playback_service
        .get_playback_source_metadata(&identity)
        .await
        .map_err(ApiError::from)?;
    if let Some(metadata) = metadata {
        return Ok(ResolvedPlaybackSourceMetadata {
            duration_seconds: positive_duration(metadata.duration_seconds),
            is_live: metadata.is_live.unwrap_or(false),
        });
    }

    Ok(ResolvedPlaybackSourceMetadata {
        duration_seconds: observation.and_then(|observation| observation.duration_seconds),
        is_live: observation.is_some_and(|observation| observation.is_live),
    })
}
