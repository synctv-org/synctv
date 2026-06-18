use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use synctv_core::models::{RoomId, RoomPlaybackState};
use synctv_core::spawn::spawn_monitored;

use super::resource_observer::ResourceObserver;
use crate::impls::playback::PlaybackService;

const OBSERVED_PLAYBACK_LIFECYCLE_TICK_INTERVAL: Duration = Duration::from_secs(1);
const OBSERVED_PLAYBACK_LIFECYCLE_CONCURRENCY: usize = 16;

#[derive(Debug, Clone)]
pub struct ObservedPlaybackLifecycleEvent {
    pub room_id: RoomId,
    pub state: RoomPlaybackState,
}

#[async_trait]
pub trait ObservedPlaybackLifecycleSubscriber: Send + Sync {
    async fn handle_observed_playback_lifecycle_event(
        &self,
        event: ObservedPlaybackLifecycleEvent,
    ) -> Result<(), String>;
}

pub struct ProviderPlaybackProgressSubscriber {
    playback_service: Arc<dyn PlaybackService>,
}

impl ProviderPlaybackProgressSubscriber {
    #[must_use]
    pub fn new(playback_service: Arc<dyn PlaybackService>) -> Self {
        Self { playback_service }
    }
}

#[async_trait]
impl ObservedPlaybackLifecycleSubscriber for ProviderPlaybackProgressSubscriber {
    async fn handle_observed_playback_lifecycle_event(
        &self,
        event: ObservedPlaybackLifecycleEvent,
    ) -> Result<(), String> {
        if !event.state.is_playing {
            return Ok(());
        }

        if !ResourceObserver::room_has_playback_observers(event.room_id).await {
            return Ok(());
        }

        self.playback_service
            .report_provider_playback_progress(
                &event.state,
                event.state.computed_position(),
                false,
                false,
            )
            .await;
        Ok(())
    }
}

pub struct PlaybackAutoAdvanceSubscriber {
    playback_service: Arc<dyn PlaybackService>,
}

impl PlaybackAutoAdvanceSubscriber {
    #[must_use]
    pub fn new(playback_service: Arc<dyn PlaybackService>) -> Self {
        Self { playback_service }
    }
}

#[async_trait]
impl ObservedPlaybackLifecycleSubscriber for PlaybackAutoAdvanceSubscriber {
    async fn handle_observed_playback_lifecycle_event(
        &self,
        event: ObservedPlaybackLifecycleEvent,
    ) -> Result<(), String> {
        if !event.state.is_playing {
            return Ok(());
        }

        self.playback_service
            .refresh_observed_playback_metadata_and_auto_advance(&event.room_id, &event.state)
            .await;
        Ok(())
    }
}

pub fn spawn_observed_playback_lifecycle_event_source(
    playback_service: Arc<dyn PlaybackService>,
    subscribers: Vec<Arc<dyn ObservedPlaybackLifecycleSubscriber>>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> tokio::task::JoinHandle<()> {
    spawn_monitored("observed_playback_lifecycle_event_source", async move {
        let mut ticker = tokio::time::interval(OBSERVED_PLAYBACK_LIFECYCLE_TICK_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    publish_observed_playback_lifecycle_events(
                        Arc::clone(&playback_service),
                        subscribers.as_slice(),
                    )
                    .await;
                }
                changed = shutdown_rx.changed() => {
                    if changed.is_err() || *shutdown_rx.borrow() {
                        break;
                    }
                }
            }
        }
    })
}

async fn publish_observed_playback_lifecycle_events(
    playback_service: Arc<dyn PlaybackService>,
    subscribers: &[Arc<dyn ObservedPlaybackLifecycleSubscriber>],
) {
    if subscribers.is_empty() {
        return;
    }

    let active_rooms = ResourceObserver::active_playback_rooms().await;
    if active_rooms.is_empty() {
        return;
    }

    tokio_stream::iter(active_rooms)
        .for_each_concurrent(OBSERVED_PLAYBACK_LIFECYCLE_CONCURRENCY, |room_id| {
            let playback_service = Arc::clone(&playback_service);
            let subscribers = subscribers.to_vec();
            async move {
                if let Err(error) = publish_observed_playback_lifecycle_event(
                    playback_service,
                    subscribers,
                    room_id,
                )
                .await
                {
                    tracing::warn!(
                        error = %error,
                        room_id = %room_id,
                        "Failed to publish observed playback lifecycle event"
                    );
                }
            }
        })
        .await;
}

async fn publish_observed_playback_lifecycle_event(
    playback_service: Arc<dyn PlaybackService>,
    subscribers: Vec<Arc<dyn ObservedPlaybackLifecycleSubscriber>>,
    room_id: RoomId,
) -> Result<(), String> {
    if !ResourceObserver::room_has_playback_observers(room_id).await {
        return Ok(());
    }

    let state = playback_service
        .room_playback_state(&room_id)
        .await
        .map_err(|error| error.to_string())?;
    if !state.is_playing {
        return Ok(());
    }

    if !ResourceObserver::room_has_playback_observers(room_id).await {
        return Ok(());
    }

    let event = ObservedPlaybackLifecycleEvent { room_id, state };
    tokio_stream::iter(subscribers)
        .for_each_concurrent(OBSERVED_PLAYBACK_LIFECYCLE_CONCURRENCY, |subscriber| {
            let event = event.clone();
            async move {
                if let Err(error) = subscriber
                    .handle_observed_playback_lifecycle_event(event)
                    .await
                {
                    tracing::warn!(
                        error = %error,
                        "Observed playback lifecycle subscriber failed"
                    );
                }
            }
        })
        .await;
    Ok(())
}
