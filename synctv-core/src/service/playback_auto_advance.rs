use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::PlaybackService;
use crate::models::RoomId;
use crate::repository::RoomSettingsRepository;

#[async_trait]
pub trait ActivePlaybackRoomSource: Send + Sync {
    /// Return rooms with realtime connections on this process.
    ///
    /// Playback lifecycle workers use local active rooms as their scheduling
    /// boundary. Every node that owns room connections runs these workers, and
    /// duplicate attempts are expected when a room has connections on several
    /// nodes. Cross-node correctness belongs to the storage/write path:
    /// duration probing claims rows, and auto-advance uses playback state
    /// transactions with optimistic versions.
    ///
    /// This source must reflect the current process's realtime runtime.
    /// Presence, hot-room indexes, and shared room statistics are for lists,
    /// admin views, analytics, and metrics.
    ///
    /// Maintenance rule: keep this wired to `ConnectionRuntime::active_room_ids()`.
    /// Playback workers are per-process lifecycle workers; they are expected to
    /// run on every node and converge through database locks or playback-state
    /// optimistic writes. Global room popularity and presence aggregates are
    /// separate read models.
    async fn active_room_ids(&self) -> crate::Result<Vec<RoomId>>;
}

#[derive(Clone)]
pub struct PlaybackAutoAdvanceService {
    playback_service: PlaybackService,
    settings_repo: RoomSettingsRepository,
    active_room_source: Option<Arc<dyn ActivePlaybackRoomSource>>,
}

impl PlaybackAutoAdvanceService {
    const DEFAULT_SCAN_LIMIT: i64 = 256;

    #[must_use]
    pub const fn new(
        playback_service: PlaybackService,
        settings_repo: RoomSettingsRepository,
    ) -> Self {
        Self {
            playback_service,
            settings_repo,
            active_room_source: None,
        }
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
        interval: std::time::Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let service = self.clone();

        crate::spawn::spawn_monitored("playback_auto_advance", async move {
            let mut ticker = tokio::time::interval(interval);
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel.cancelled() => {
                        info!("Playback auto-advance task cancelled");
                        return;
                    }
                    _ = ticker.tick() => {
                        // Scope each tick to rooms with realtime connections in
                        // this process. Local room connections are the lifecycle
                        // ownership signal for duration, auto-advance, and live
                        // playback resource work. Cross-node duplicate attempts
                        // are resolved by the transactional state update below.
                        let active_room_ids = match service.active_room_ids().await {
                            Ok(active_room_ids) if active_room_ids.is_empty() => continue,
                            Ok(active_room_ids) => active_room_ids,
                            Err(error) => {
                                error!(error = %error, "Playback auto-advance active room lookup failed");
                                continue;
                            }
                        };

                        match service
                            .playback_service
                            .auto_advance_due_sources_for_rooms(
                                &service.settings_repo,
                                &active_room_ids,
                                Self::DEFAULT_SCAN_LIMIT,
                            )
                            .await
                        {
                            Ok(advanced) if advanced > 0 => {
                                info!(advanced, "Playback auto-advance completed");
                            }
                            Ok(_) => {}
                            Err(error) => {
                                error!(error = %error, "Playback auto-advance failed");
                            }
                        }
                    }
                }
            }
        })
    }

    async fn active_room_ids(&self) -> crate::Result<Vec<RoomId>> {
        let Some(active_room_source) = &self.active_room_source else {
            return Ok(Vec::new());
        };
        active_room_source.active_room_ids().await
    }
}
