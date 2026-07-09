mod parsing;
mod transport;

use std::{sync::Arc, time::Duration};

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use super::{media::BackendPlaybackRequest, ActivePlaybackRoomSource, PlaybackService};
use crate::{
    models::{PlaybackDurationStatus, PlaybackSourceIdentity, RoomId},
    provider::PlaybackResult,
    Error, Result,
};

use transport::{is_http_url, probe_duration, ProbeTarget};

#[derive(Clone)]
pub struct PlaybackDurationProbeService {
    playback_service: PlaybackService,
    ssrf_guard: synctv_common::ssrf::SsrfGuard,
    concurrency: usize,
    active_room_source: Option<Arc<dyn ActivePlaybackRoomSource>>,
}

impl PlaybackDurationProbeService {
    const DEFAULT_SCAN_LIMIT: i64 = 32;
    const DEFAULT_CONCURRENCY: usize = 4;
    const RETRY_AFTER_TRANSIENT: chrono::Duration = chrono::Duration::minutes(10);
    const RETRY_AFTER_UNAVAILABLE: chrono::Duration = chrono::Duration::hours(6);

    #[must_use]
    pub const fn new(
        playback_service: PlaybackService,
        ssrf_guard: synctv_common::ssrf::SsrfGuard,
    ) -> Self {
        Self {
            playback_service,
            ssrf_guard,
            concurrency: Self::DEFAULT_CONCURRENCY,
            active_room_source: None,
        }
    }

    #[must_use]
    pub const fn with_concurrency(mut self, concurrency: usize) -> Self {
        self.concurrency = concurrency;
        self
    }

    #[must_use]
    pub fn with_active_room_source(
        mut self,
        active_room_source: Arc<dyn ActivePlaybackRoomSource>,
    ) -> Self {
        self.active_room_source = Some(active_room_source);
        self
    }

    #[must_use]
    pub fn spawn(
        &self,
        interval: Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();

        crate::spawn::spawn_monitored("playback_duration_probe", async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Playback duration probe task cancelled");
                        return;
                    }
                    _ = ticker.tick() => {
                        match service.run_once().await {
                            Ok(probed) if probed > 0 => {
                                info!(probed, "Playback duration probe completed");
                            }
                            Ok(_) => {}
                            Err(error) => {
                                error!(error = %error, "Playback duration probe failed");
                            }
                        }
                    }
                }
            }
        })
    }

    pub async fn run_once(&self) -> Result<usize> {
        // Run from rooms with realtime connections in this process. Several
        // nodes can host the same room, so repository claims use row locks and
        // SKIP LOCKED to serialize probe attempts. Keep this input tied to the
        // local realtime runtime: duration probing is a lifecycle optimization
        // for rooms this node is serving. The repository query also joins the
        // current playback target hash, which keeps dynamic playlist probing
        // bound to the item the room is currently watching.
        let active_room_ids = self.active_room_ids().await?;
        if active_room_ids.is_empty() {
            return Ok(0);
        }

        self.initialize_active_sources(&active_room_ids).await?;

        let claims = self
            .playback_service
            .source_metadata_repository()
            .claim_duration_probe_batch_for_rooms(&active_room_ids, Self::DEFAULT_SCAN_LIMIT)
            .await?;
        if claims.is_empty() {
            return Ok(0);
        }

        let semaphore = Arc::new(Semaphore::new(self.concurrency.max(1)));
        let mut tasks = tokio::task::JoinSet::new();

        for claim in claims {
            let service = self.clone();
            let semaphore = semaphore.clone();
            tasks.spawn(async move {
                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .map_err(|_| Error::ServiceUnavailable("duration probe stopped".to_string()))?;
                service.probe_claim(claim).await
            });
        }

        let mut completed = 0_usize;
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => completed += 1,
                Ok(Err(error)) => warn!(error = %error, "Playback duration probe claim failed"),
                Err(error) => warn!(error = %error, "Playback duration probe task failed"),
            }
        }

        Ok(completed)
    }

    async fn active_room_ids(&self) -> Result<Vec<RoomId>> {
        let Some(active_room_source) = &self.active_room_source else {
            return Ok(Vec::new());
        };
        active_room_source.active_room_ids().await
    }

    async fn initialize_active_sources(&self, room_ids: &[RoomId]) -> Result<()> {
        // Seed missing metadata only for local active rooms. Inactive rooms can
        // have stale playback state in storage; probing local active rooms keeps
        // this lifecycle optimization bounded to the node's connection set.
        for room_id in room_ids {
            let state = self.playback_service.get_state(room_id).await?;
            if !state.is_playing {
                continue;
            }
            let Some(identity) = PlaybackSourceIdentity::from_state(&state)? else {
                continue;
            };
            match self
                .playback_service
                .source_live_status_for_state(&state)
                .await?
            {
                Some(true) => {
                    self.playback_service
                        .source_metadata_repository()
                        .upsert_provider_source_metadata(&identity, true, None)
                        .await?;
                }
                Some(false) => {
                    self.playback_service
                        .source_metadata_repository()
                        .mark_probeable_unknown_if_absent(&identity)
                        .await?;
                }
                None => {}
            }
        }
        Ok(())
    }

    pub async fn probe_active_source_once(
        &self,
        state: &crate::models::RoomPlaybackState,
    ) -> Result<bool> {
        if !state.is_playing {
            return Ok(false);
        }

        let Some(identity) = PlaybackSourceIdentity::from_state(state)? else {
            return Ok(false);
        };
        if self
            .playback_service
            .source_live_status_for_state(state)
            .await?
            != Some(false)
        {
            return Ok(false);
        }

        let Some(claim) = self
            .playback_service
            .source_metadata_repository()
            .claim_duration_probe_for_active_source(&identity)
            .await?
        else {
            return Ok(false);
        };

        self.probe_claim(claim).await?;
        Ok(true)
    }

    async fn probe_claim(&self, claim: crate::models::ClaimedPlaybackDurationProbe) -> Result<()> {
        let Some(identity) = PlaybackSourceIdentity::from_state(&claim.state)? else {
            return Ok(());
        };
        if identity != playback_identity_from_metadata(&claim.metadata) {
            return Ok(());
        }

        let playback = self
            .playback_service
            .generate_backend_playback_for_source(BackendPlaybackRequest {
                room_id: claim.metadata.room_id,
                media_id: claim.metadata.media_id,
                playlist_id: claim.metadata.playlist_id,
                target: Some(claim.state.target.as_ref().ok_or_else(|| {
                    Error::InvalidInput(
                        "target is required for dynamic playlist duration probing".to_string(),
                    )
                })?),
            })
            .await?;
        let Some(playback) = playback else {
            self.mark_failed(
                &identity,
                claim.metadata.version,
                PlaybackDurationStatus::Unavailable,
                "playback source disappeared",
                Self::RETRY_AFTER_UNAVAILABLE,
            )
            .await?;
            return Ok(());
        };

        if playback.is_live == Some(true) {
            self.playback_service
                .source_metadata_repository()
                .upsert_provider_source_metadata(&identity, true, None)
                .await?;
            return Ok(());
        }

        if let Some(duration_seconds) = playback
            .duration_seconds
            .filter(|duration| duration.is_finite() && *duration > 0.0)
        {
            self.playback_service
                .source_metadata_repository()
                .complete_probe_duration(&identity, claim.metadata.version, duration_seconds)
                .await?;
            return Ok(());
        }

        let Some(target) = select_probe_target(&playback) else {
            self.mark_failed(
                &identity,
                claim.metadata.version,
                PlaybackDurationStatus::Unavailable,
                "playback has no probeable URL",
                Self::RETRY_AFTER_UNAVAILABLE,
            )
            .await?;
            return Ok(());
        };

        match probe_duration(&target, &self.ssrf_guard).await {
            Ok(duration_seconds) => {
                self.playback_service
                    .source_metadata_repository()
                    .complete_probe_duration(&identity, claim.metadata.version, duration_seconds)
                    .await?;
            }
            Err(error) => {
                self.mark_failed(
                    &identity,
                    claim.metadata.version,
                    PlaybackDurationStatus::Failed,
                    &error.to_string(),
                    Self::RETRY_AFTER_TRANSIENT,
                )
                .await?;
            }
        }

        Ok(())
    }

    async fn mark_failed(
        &self,
        identity: &PlaybackSourceIdentity,
        expected_version: i64,
        status: PlaybackDurationStatus,
        error: &str,
        retry_after: chrono::Duration,
    ) -> Result<()> {
        self.playback_service
            .source_metadata_repository()
            .mark_probe_failed(identity, expected_version, status, error, retry_after)
            .await?;
        Ok(())
    }
}

fn playback_identity_from_metadata(
    metadata: &crate::models::PlaybackSourceMetadata,
) -> PlaybackSourceIdentity {
    PlaybackSourceIdentity {
        room_id: metadata.room_id,
        media_id: metadata.media_id,
        playlist_id: metadata.playlist_id,
        target_hash: metadata.target_hash.clone(),
    }
}

fn select_probe_target(playback: &PlaybackResult) -> Option<ProbeTarget> {
    let info = playback
        .playback_infos
        .get(&playback.default_mode)
        .or_else(|| playback.playback_infos.values().next())?;
    let media = info
        .medias
        .iter()
        .find(|media| media.upstream_url().is_some_and(is_http_url))?;
    let url = media.upstream_url()?.to_string();
    Some(ProbeTarget {
        url,
        format: media.format.clone(),
        headers: media.upstream_headers(),
    })
}
