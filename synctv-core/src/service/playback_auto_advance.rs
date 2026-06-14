use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use super::{LeaderCheck, PlaybackService};
use crate::repository::RoomSettingsRepository;

#[derive(Clone)]
pub struct PlaybackAutoAdvanceService {
    playback_service: PlaybackService,
    settings_repo: RoomSettingsRepository,
    leader_check: Arc<dyn LeaderCheck>,
}

impl PlaybackAutoAdvanceService {
    const DEFAULT_SCAN_LIMIT: i64 = 256;

    #[must_use]
    pub const fn new(
        playback_service: PlaybackService,
        settings_repo: RoomSettingsRepository,
        leader_check: Arc<dyn LeaderCheck>,
    ) -> Self {
        Self {
            playback_service,
            settings_repo,
            leader_check,
        }
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
                        if !service.leader_check.is_leader() {
                            continue;
                        }

                        match service
                            .playback_service
                            .auto_advance_due_sources(&service.settings_repo, Self::DEFAULT_SCAN_LIMIT)
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
}
