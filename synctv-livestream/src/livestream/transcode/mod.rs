//! Live transcode orchestration.
//!
//! The manager runs on the node that receives the source stream (the node
//! owning the source generation) and exposes quality rungs as additional
//! local streams keyed `room / {mediaId}-{rung}`. Rungs are ordinary StreamHub
//! publications plus registry entries, so HTTP-FLV, HLS, and the
//! multi-replica relay serve them through the existing machinery with no
//! special-casing.
//!
//! Scheduling is demand driven: a viewer asking for a rung that is not live
//! registers demand; the scheduler repeatedly submits the job with the most
//! waiting viewers to the configured [`TranscodeWorker`] and reacts to the
//! worker's structured capacity rejection by backing off and re-evaluating,
//! which may select a different job by then. The scheduler never reads
//! worker-internal settings such as local capacity.

mod config;
mod ffmpeg;
mod publisher;
mod worker;

pub use config::{
    FfmpegWorkerConfig, TranscodeConfig, TranscodePreset, WorkerConfig, WorkerEngine,
};
pub use ffmpeg::FfmpegWorker;
pub use worker::{
    TranscodeError, TranscodeInput, TranscodeJob, TranscodeJobHandle, TranscodeOutputReceiver,
    TranscodeWorker,
};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use synctv_xiu::streamhub::{
    define::{StreamHubEvent, StreamHubEventSender},
    send_event_with_backpressure_timeout_for,
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use crate::relay::registry_trait::StreamRegistryTrait;

/// How long a viewer waits for a scheduled rung to come live before the
/// caller falls back to the source quality.
const RUNG_READY_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a rung stays up after its last viewer leaves.
const RUNG_IDLE_GRACE: Duration = Duration::from_secs(30);
/// Backoff after the worker reports [`TranscodeError::CapacityExhausted`].
const CAPACITY_BACKOFF: Duration = Duration::from_secs(2);
/// Cooldown before a failed rung may be scheduled again.
const RUNG_FAILURE_COOLDOWN: Duration = Duration::from_secs(30);
const STREAMHUB_EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);
/// Poll interval while waiting for a scheduled rung to become ready.
const RUNG_READY_POLL: Duration = Duration::from_millis(200);
/// Cadence of the scheduler's periodic housekeeping tick.
const SCHEDULER_TICK: Duration = Duration::from_secs(1);

/// Separator between the numeric source media id and the rung preset inside a
/// rung stream name. Stream identifiers must not contain `:`, so the rung key
/// keeps to characters the registry and relay validators accept.
const RUNG_SEPARATOR: char = '-';

/// Stream name of a rung inside StreamHub and the cluster registry:
/// `{mediaId}-{preset}`.
#[must_use]
pub fn rung_stream_name(media_id: &str, rung: &str) -> String {
    format!("{media_id}{RUNG_SEPARATOR}{rung}")
}

/// Split a rung stream name back into `(media_id, rung)`.
///
/// Only a numeric base counts: source media ids are numeric, so the first
/// separator after a digit run is unambiguous and no hand-picked stream name
/// can be mistaken for a rung of another stream.
#[must_use]
pub fn split_rung_stream_name(stream_name: &str) -> Option<(&str, &str)> {
    let (media, rung) = stream_name.split_once(RUNG_SEPARATOR)?;
    let is_rung_key =
        !media.is_empty() && !rung.is_empty() && media.bytes().all(|byte| byte.is_ascii_digit());
    is_rung_key.then_some((media, rung))
}

type SourceKey = (String, String);

#[derive(Default)]
struct RungState {
    /// Viewers waiting for this rung to come live.
    waiting_viewers: u32,
    /// Viewers currently watching this rung (local sessions plus relay
    /// subscriptions served by this node as the rung owner).
    subscribers: u32,
    /// Set when the worker reported this rung can never run for this source.
    unsupported: Option<String>,
    last_failure: Option<std::time::Instant>,
    running: Option<RunningRung>,
}

struct RunningRung {
    cancel: tokio_util::sync::CancellationToken,
    /// Instant since which the rung has had no viewers at all.
    idle_since: Option<std::time::Instant>,
}

#[derive(Default)]
struct SourceState {
    rungs: HashMap<String, RungState>,
    /// `width`/`height` observed in the source `onMetaData`, when seen.
    source_resolution: Option<(u32, u32)>,
}

struct ManagerInner {
    config: TranscodeConfig,
    /// Workers in submission order: priority descending, list order on ties.
    workers: Vec<WorkerSlot>,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    local_node_id: String,
    local_cluster_address: String,
    state: Mutex<HashMap<SourceKey, SourceState>>,
    schedule_wake: Notify,
}

struct WorkerSlot {
    label: String,
    worker: Arc<dyn TranscodeWorker>,
}

/// What a [`RungViewerGuard`] counts against while held.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewerRole {
    /// Waiting for the rung to come live; drives scheduling priority.
    Waiting,
    /// Actively watching; keeps the rung from idling out.
    Subscribed,
}

/// Keeps one viewer's rung demand registered until dropped. Held by the
/// serving layer for the lifetime of the viewer's session.
pub struct RungViewerGuard {
    inner: Option<Arc<ManagerInner>>,
    key: SourceKey,
    rung: String,
    role: ViewerRole,
}

impl RungViewerGuard {
    fn release(&mut self) {
        if let Some(inner) = self.inner.take() {
            let now = std::time::Instant::now();
            let mut state = inner.state.lock().expect("transcode state poisoned");
            if let Some(source) = state.get_mut(&self.key) {
                if let Some(rung_state) = source.rungs.get_mut(&self.rung) {
                    match self.role {
                        ViewerRole::Waiting => {
                            rung_state.waiting_viewers =
                                rung_state.waiting_viewers.saturating_sub(1);
                        }
                        ViewerRole::Subscribed => {
                            rung_state.subscribers = rung_state.subscribers.saturating_sub(1);
                        }
                    }
                    if rung_state.waiting_viewers == 0 && rung_state.subscribers == 0 {
                        if let Some(running) = rung_state.running.as_mut() {
                            running.idle_since.get_or_insert(now);
                        }
                    }
                }
                prune_source(source);
            }
            drop(state);
            inner.schedule_wake.notify_one();
        }
    }
}

impl Drop for RungViewerGuard {
    fn drop(&mut self) {
        self.release();
    }
}

/// Drop rung entries that no longer carry information: no viewers, no
/// running job, no permanent verdict.
fn prune_source(source: &mut SourceState) {
    source.rungs.retain(|_, rung_state| {
        rung_state.running.is_some()
            || rung_state.waiting_viewers > 0
            || rung_state.subscribers > 0
            || rung_state.unsupported.is_some()
    });
}

pub struct TranscodeManager {
    inner: Arc<ManagerInner>,
}

impl TranscodeManager {
    /// Construct the manager and start its scheduler. The configuration must
    /// already be validated (see [`TranscodeConfig::validate`]).
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: TranscodeConfig,
        workers: Vec<Arc<dyn TranscodeWorker>>,
        registry: Arc<dyn StreamRegistryTrait>,
        stream_hub_event_sender: StreamHubEventSender,
        local_node_id: String,
        local_cluster_address: String,
    ) -> Arc<Self> {
        assert!(
            config.validate().is_empty(),
            "transcode configuration must be validated before constructing the manager"
        );
        assert_eq!(
            config.workers.len(),
            workers.len(),
            "one engine instance must be built per configured worker"
        );
        // Priority descending; `sort_by_key` is stable, so list order breaks
        // ties and the default worker stays first when untouched.
        let mut slots: Vec<(u32, WorkerSlot)> = config
            .workers
            .iter()
            .enumerate()
            .zip(workers)
            .map(|((index, worker_config), worker)| {
                let priority = worker_config.priority;
                (
                    priority,
                    WorkerSlot {
                        label: worker_config.label(index),
                        worker,
                    },
                )
            })
            .collect();
        slots.sort_by_key(|(priority, _)| std::cmp::Reverse(*priority));
        let workers = slots.into_iter().map(|(_, slot)| slot).collect();
        let inner = Arc::new(ManagerInner {
            config,
            workers,
            registry,
            stream_hub_event_sender,
            local_node_id,
            local_cluster_address,
            state: Mutex::new(HashMap::new()),
            schedule_wake: Notify::new(),
        });
        let manager = Arc::new(Self { inner });
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let scheduler = Arc::clone(&manager.inner);
            handle.spawn(async move {
                scheduler_loop(scheduler).await;
            });
        } else {
            warn!("No Tokio runtime; transcode scheduler not started");
        }
        manager
    }

    /// The configured quality rungs, best-first.
    #[must_use]
    pub fn presets(&self) -> &[TranscodePreset] {
        &self.inner.config.presets
    }

    /// The configured rungs worth offering for one source, best-first:
    /// strictly below the source resolution once it is known, and without
    /// rungs this node already knows the source cannot provide (a verdict
    /// from an earlier scheduling attempt on this node).
    #[must_use]
    pub fn eligible_presets(&self, room_id: &str, media_id: &str) -> Vec<&TranscodePreset> {
        let state = self.inner.state.lock().expect("transcode state poisoned");
        eligible_presets_for(
            &self.inner.config.presets,
            state.get(&(room_id.to_string(), media_id.to_string())),
        )
    }

    /// Ensure the rung `rung` of `media_id` is servable from this node:
    ///
    /// * a rung registered by any node resolves immediately (this node's
    ///   serving path relays it when the owner is remote);
    /// * otherwise the source must publish on this node, and this call
    ///   registers the viewer's demand and waits for the scheduler to bring
    ///   the rung live.
    ///
    /// The returned guard keeps the rung non-idle for the viewer's session
    /// and must be held by the serving layer.
    pub async fn ensure_rung_stream(
        self: &Arc<Self>,
        room_id: &str,
        media_id: &str,
        rung: &str,
    ) -> crate::error::StreamResult<RungViewerGuard> {
        use crate::error::StreamError;

        if !self.inner.config.presets.iter().any(|p| p.name == rung) {
            return Err(StreamError::InvalidStreamKey(format!(
                "unknown transcode rung '{rung}'"
            )));
        }
        let rung_key = rung_stream_name(media_id, rung);

        // A rung that any node already registered needs no scheduling; the
        // normal serving/relay paths resolve it.
        if self.rung_is_registered(room_id, &rung_key).await {
            return Ok(self.attach_rung_viewer(room_id, media_id, rung));
        }

        // Starting a rung is the source-owning node's own decision; other
        // nodes never ask it to transcode. When this node does not own the
        // source, a registered rung is relayed like any stream, and a
        // missing one is an immediate fallback to the source quality.
        match self
            .inner
            .registry
            .get_active_generation(room_id, media_id)
            .await
        {
            Ok(Some(info)) if info.node_id != self.inner.local_node_id => {
                return Err(StreamError::ResourceExhausted(format!(
                    "transcode rung '{rung}' for {room_id}/{media_id} is not available"
                )));
            }
            Ok(_) => {}
            Err(error) => {
                return Err(StreamError::RegistryError(format!(
                    "Failed to look up publisher for {room_id}/{media_id}: {error}"
                )));
            }
        }

        self.inner
            .add_viewer(room_id, media_id, rung, ViewerRole::Waiting);
        let mut guard = RungViewerGuard {
            inner: Some(Arc::clone(&self.inner)),
            key: (room_id.to_string(), media_id.to_string()),
            rung: rung.to_string(),
            role: ViewerRole::Waiting,
        };

        let deadline = tokio::time::Instant::now() + RUNG_READY_TIMEOUT;
        loop {
            if self.rung_is_registered(room_id, &rung_key).await {
                // Upgrade the guard from waiting to subscribed: release()
                // decrements by its current role, so move the count across
                // before flipping the role. Leaving the waiting count behind
                // would block the idle timeout forever.
                let mut state = self.inner.state.lock().expect("transcode state poisoned");
                if let Some(rung_state) = state
                    .get_mut(&guard.key)
                    .and_then(|source| source.rungs.get_mut(&guard.rung))
                {
                    rung_state.waiting_viewers = rung_state.waiting_viewers.saturating_sub(1);
                    rung_state.subscribers = rung_state.subscribers.saturating_add(1);
                    if let Some(running) = rung_state.running.as_mut() {
                        running.idle_since = None;
                    }
                    drop(state);
                    guard.role = ViewerRole::Subscribed;
                    return Ok(guard);
                }
                // The entry vanished (source ended); drop back through the
                // error paths below on the next iteration.
            }

            let verdict = {
                let state = self.inner.state.lock().expect("transcode state poisoned");
                state
                    .get(&guard.key)
                    .and_then(|source| source.rungs.get(&guard.rung))
                    .and_then(|rung_state| rung_state.unsupported.clone())
            };
            if let Some(reason) = verdict {
                guard.release();
                return Err(StreamError::InvalidState(format!(
                    "transcode rung '{rung}' for {room_id}/{media_id}: {reason}"
                )));
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(StreamError::ResourceExhausted(format!(
                    "transcode rung '{rung}' for {room_id}/{media_id} did not become ready in time"
                )));
            }
            tokio::time::sleep(RUNG_READY_POLL).await;
        }
    }

    /// Register an already-live rung viewer (local session or relay
    /// subscription served by this node) without the waiting phase.
    #[must_use]
    pub fn attach_rung_viewer(&self, room_id: &str, media_id: &str, rung: &str) -> RungViewerGuard {
        self.inner
            .add_viewer(room_id, media_id, rung, ViewerRole::Subscribed);
        RungViewerGuard {
            inner: Some(Arc::clone(&self.inner)),
            key: (room_id.to_string(), media_id.to_string()),
            rung: rung.to_string(),
            role: ViewerRole::Subscribed,
        }
    }

    /// Refresh viewer accounting for a rung-addressed stream key. Returns
    /// `None` when the key is not a rung of a configured preset.
    #[must_use]
    pub fn attach_rung_viewer_for_stream_key(
        &self,
        room_id: &str,
        stream_name: &str,
    ) -> Option<RungViewerGuard> {
        let (base, rung) = split_rung_stream_name(stream_name)?;
        self.inner
            .config
            .presets
            .iter()
            .any(|preset| preset.name == rung)
            .then(|| self.attach_rung_viewer(room_id, base, rung))
    }

    async fn rung_is_registered(&self, room_id: &str, rung_key: &str) -> bool {
        matches!(
            self.inner
                .registry
                .get_active_generation(room_id, rung_key)
                .await,
            Ok(Some(_))
        )
    }

    /// Record that a source stream has ended on this node: cancel all its
    /// rungs and drop the bookkeeping.
    pub fn note_source_ended(&self, room_id: &str, media_id: &str) {
        self.inner.clear_source(room_id, media_id);
    }
}

impl ManagerInner {
    fn add_viewer(&self, room_id: &str, media_id: &str, rung: &str, role: ViewerRole) {
        let mut state = self.state.lock().expect("transcode state poisoned");
        let source = state
            .entry((room_id.to_string(), media_id.to_string()))
            .or_default();
        let rung_state = source.rungs.entry(rung.to_string()).or_default();
        match role {
            ViewerRole::Waiting => {
                rung_state.waiting_viewers = rung_state.waiting_viewers.saturating_add(1);
            }
            ViewerRole::Subscribed => {
                rung_state.subscribers = rung_state.subscribers.saturating_add(1);
                if let Some(running) = rung_state.running.as_mut() {
                    running.idle_since = None;
                }
            }
        }
        drop(state);
        self.schedule_wake.notify_one();
    }

    /// Cancel every rung of a source and remove its state. Called when the
    /// source stops publishing on this node.
    fn clear_source(&self, room_id: &str, media_id: &str) {
        let cancels = {
            let mut state = self.state.lock().expect("transcode state poisoned");
            state
                .remove(&(room_id.to_string(), media_id.to_string()))
                .map_or_else(Vec::new, |source| {
                    source
                        .rungs
                        .into_values()
                        .filter_map(|rung_state| rung_state.running.map(|running| running.cancel))
                        .collect::<Vec<_>>()
                })
        };
        for cancel in cancels {
            cancel.cancel();
        }
        self.schedule_wake.notify_one();
    }

    /// Called by the job supervisor when a rung job has fully ended.
    fn note_rung_finished(
        self: &Arc<Self>,
        room_id: &str,
        media_id: &str,
        rung: &str,
        outcome: &Result<(), TranscodeError>,
    ) {
        let mut wake = false;
        {
            let mut state = self.state.lock().expect("transcode state poisoned");
            let now = std::time::Instant::now();
            if let Some(source) = state.get_mut(&(room_id.to_string(), media_id.to_string())) {
                if let Some(rung_state) = source.rungs.get_mut(rung) {
                    if rung_state.running.take().is_some() {
                        wake = true;
                        match outcome {
                            Ok(()) => {
                                debug!(room_id, media_id, rung, "Transcode rung ended");
                            }
                            Err(TranscodeError::Unsupported(reason)) => {
                                warn!(
                                    room_id,
                                    media_id,
                                    rung,
                                    reason,
                                    "Transcode rung permanently unsupported"
                                );
                                rung_state.unsupported = Some(reason.clone());
                            }
                            Err(error) => {
                                warn!(room_id, media_id, rung, %error, "Transcode rung failed");
                                rung_state.last_failure = Some(now);
                            }
                        }
                    }
                }
                prune_source(source);
            }
        }
        if wake {
            self.schedule_wake.notify_one();
        }
    }
}

async fn scheduler_loop(inner: Arc<ManagerInner>) {
    loop {
        tokio::select! {
            () = inner.schedule_wake.notified() => {}
            () = tokio::time::sleep(SCHEDULER_TICK) => {}
        }
        scheduler_tick(&inner).await;
    }
}

/// One scheduling round. Locks are only held for synchronous bookkeeping;
/// every `.await` happens on owned data.
async fn scheduler_tick(inner: &Arc<ManagerInner>) {
    let now = std::time::Instant::now();

    // Cancel rungs idle past the grace period.
    let idle_cancels = {
        let state = inner.state.lock().expect("transcode state poisoned");
        state
            .values()
            .flat_map(|source| source.rungs.values())
            .filter_map(|rung_state| rung_state.running.as_ref())
            .filter(|running| {
                running
                    .idle_since
                    .is_some_and(|idle_since| now.duration_since(idle_since) >= RUNG_IDLE_GRACE)
            })
            .map(|running| running.cancel.clone())
            .collect::<Vec<_>>()
    };
    for cancel in idle_cancels {
        info!("Stopping idle transcode rung");
        cancel.cancel();
    }

    // Pick the most-demanded schedulable rung.
    let picked = {
        let state = inner.state.lock().expect("transcode state poisoned");
        pick_most_demanded(&state, &inner.config.presets, now).map(
            |((room_id, media_id), rung, demand)| {
                (room_id.clone(), media_id.clone(), rung.to_string(), demand)
            },
        )
    };

    let Some((room_id, media_id, rung, demand)) = picked else {
        return;
    };
    let Some(preset) = inner.config.presets.iter().find(|p| p.name == rung) else {
        return;
    };
    let preset = preset.clone();
    let job = TranscodeJob {
        room_id: room_id.clone(),
        media_id: media_id.clone(),
        preset,
    };

    // Subscribe to the source before submitting so the job never races the
    // stream disappearing mid-startup.
    let Some(input) = subscribe_source_frames(inner, &room_id, &media_id).await else {
        info!(
            room_id,
            media_id, rung, "Source gone before transcode submission; dropping demand"
        );
        inner.clear_source(&room_id, &media_id);
        return;
    };
    // Watch the subscription's metadata so failed picks and future picks can
    // skip rungs at or above the source resolution.
    {
        let inner = Arc::clone(inner);
        let key = (room_id.clone(), media_id.clone());
        let slot = Arc::clone(&input.source_resolution);
        tokio::spawn(async move {
            loop {
                let resolution = {
                    let slot = slot.lock().expect("resolution slot poisoned");
                    *slot
                };
                match resolution {
                    Some(resolution) => {
                        let mut state = inner.state.lock().expect("transcode state poisoned");
                        if let Some(source) = state.get_mut(&key) {
                            source.source_resolution = Some(resolution);
                        }
                        return;
                    }
                    None => tokio::time::sleep(Duration::from_millis(200)).await,
                }
            }
        });
    }

    info!(room_id, media_id, rung, demand, "Submitting transcode job");
    let cancel = tokio_util::sync::CancellationToken::new();
    // Offer the job to the workers in priority order; rejections return the
    // input untouched so the next worker can take it.
    let mut input = input;
    let mut started: Option<(TranscodeJobHandle, TranscodeOutputReceiver, String)> = None;
    let mut all_capacity_exhausted = true;
    let mut unsupported: Option<String> = None;
    let mut failed: Option<String> = None;
    for slot in &inner.workers {
        match slot.worker.start(job.clone(), input) {
            Ok((handle, output)) => {
                started = Some((handle, output, slot.label.clone()));
                break;
            }
            Err((error, returned_input)) => {
                input = returned_input;
                match error {
                    TranscodeError::CapacityExhausted => {
                        debug!(
                            room_id, media_id, rung,
                            worker = %slot.label,
                            "Transcode worker at capacity; trying the next worker"
                        );
                    }
                    TranscodeError::Unsupported(reason) => {
                        all_capacity_exhausted = false;
                        warn!(
                            room_id, media_id, rung,
                            worker = %slot.label,
                            reason,
                            "Transcode worker cannot run this job"
                        );
                        unsupported = unsupported.or(Some(reason));
                    }
                    TranscodeError::Failed(reason) => {
                        all_capacity_exhausted = false;
                        warn!(
                            room_id, media_id, rung,
                            worker = %slot.label,
                            reason,
                            "Transcode worker failed to start the job"
                        );
                        failed = failed.or(Some(reason));
                    }
                }
            }
        }
    }

    let Some((handle, output, worker_label)) = started else {
        if all_capacity_exhausted {
            debug!(
                room_id,
                media_id, rung, "Every transcode worker is at capacity; backing off"
            );
            tokio::time::sleep(CAPACITY_BACKOFF).await;
            inner.schedule_wake.notify_one();
        } else if let Some(reason) = unsupported {
            // Permanent only when no worker could run it for engine reasons.
            inner.note_rung_finished(
                &room_id,
                &media_id,
                &rung,
                &Err(TranscodeError::Unsupported(reason)),
            );
        } else if let Some(reason) = failed {
            inner.note_rung_finished(
                &room_id,
                &media_id,
                &rung,
                &Err(TranscodeError::Failed(reason)),
            );
        }
        return;
    };

    {
        let mut state = inner.state.lock().expect("transcode state poisoned");
        let rung_state = state
            .get_mut(&(room_id.clone(), media_id.clone()))
            .and_then(|source| source.rungs.get_mut(&rung));
        // The entry may have vanished if the source ended in between.
        if let Some(rung_state) = rung_state {
            rung_state.running = Some(RunningRung {
                cancel: cancel.clone(),
                idle_since: None,
            });
        } else {
            cancel.cancel();
            handle.cancel.cancel();
            return;
        }
    }
    info!(
        room_id, media_id, rung,
        worker = %worker_label,
        "Transcode job started"
    );

    let supervisor_inner = Arc::clone(inner);
    let (supervisor_room, supervisor_media, supervisor_rung) =
        (room_id.clone(), media_id.clone(), rung.clone());
    let publisher = publisher::RungPublisher {
        room_id: room_id.clone(),
        rung_stream_name: job.rung_stream_name(),
        generation_id: Uuid::new(),
    };
    let registry = Arc::clone(&inner.registry);
    let hub_sender = inner.stream_hub_event_sender.clone();
    let node_id = inner.local_node_id.clone();
    let cluster_address = inner.local_cluster_address.clone();
    tokio::spawn(async move {
        let publisher_result = publisher::run(
            &publisher,
            output,
            registry,
            hub_sender,
            &node_id,
            &cluster_address,
            &cancel,
        )
        .await;
        // End the worker side if it is still running (it usually
        // ended on its own and closed the output channel).
        handle.cancel.cancel();
        let worker_result = tokio::time::timeout(Duration::from_secs(5), handle.done).await;
        // A publisher failure carries the more precise cause; a
        // worker failure alone still means the rung ended badly.
        let outcome = match (publisher_result, worker_result) {
            (Err(error), _) | (Ok(()), Ok(Ok(Err(error)))) => Err(error),
            (Ok(()), _) => Ok(()),
        };
        supervisor_inner.note_rung_finished(
            &supervisor_room,
            &supervisor_media,
            &supervisor_rung,
            &outcome,
        );
    });
}

/// The presets worth offering for one source, best-first: strictly below
/// the source resolution once it is known, and without rungs a previous
/// scheduling attempt on this node proved the source cannot provide.
/// A source with no local state yet offers the full ladder.
fn eligible_presets_for<'a>(
    presets: &'a [TranscodePreset],
    source: Option<&SourceState>,
) -> Vec<&'a TranscodePreset> {
    let Some(source) = source else {
        return presets.iter().collect();
    };
    let max_height = source
        .source_resolution
        .map_or(u32::MAX, |(_, height)| height);
    presets
        .iter()
        .filter(|preset| preset.height < max_height)
        .filter(|preset| {
            source
                .rungs
                .get(&preset.name)
                .is_none_or(|rung| rung.unsupported.is_none())
        })
        .collect()
}

/// Choose the rung with the most waiting viewers among the schedulable
/// ones: not already running, not permanently unsupported, not in the
/// failure cooldown, and strictly below the source resolution (when known).
fn pick_most_demanded<'a>(
    sources: &'a HashMap<SourceKey, SourceState>,
    presets: &[TranscodePreset],
    now: std::time::Instant,
) -> Option<(&'a SourceKey, &'a str, u32)> {
    let mut best: Option<(&SourceKey, &str, u32)> = None;
    for (key, source) in sources {
        // Skip rungs at or above the source resolution: they would only
        // re-encode at the source's own quality.
        let min_height = source
            .source_resolution
            .map_or(u32::MAX, |(_, height)| height);
        for (rung, rung_state) in &source.rungs {
            if rung_state.running.is_some()
                || rung_state.unsupported.is_some()
                || rung_state.waiting_viewers == 0
            {
                continue;
            }
            if rung_state
                .last_failure
                .is_some_and(|at| now.duration_since(at) < RUNG_FAILURE_COOLDOWN)
            {
                continue;
            }
            let Some(preset) = presets.iter().find(|p| &p.name == rung) else {
                continue;
            };
            if preset.height >= min_height {
                continue;
            }
            let demand = rung_state.waiting_viewers;
            if best
                .as_ref()
                .is_none_or(|(_, _, best_demand)| demand > *best_demand)
            {
                best = Some((key, rung, demand));
            }
        }
    }
    best
}

/// Subscribe to the source stream's FLV-format frames. Returns `None` when
/// the source is not publishing on this node.
async fn subscribe_source_frames(
    inner: &Arc<ManagerInner>,
    room_id: &str,
    media_id: &str,
) -> Option<TranscodeInput> {
    use synctv_xiu::streamhub::define::{
        DataReceiver, NotifyInfo, SubDataType, SubscribeType, SubscriberInfo,
    };

    let identifier = StreamIdentifier::Rtmp {
        app_name: room_id.to_string(),
        stream_name: media_id.to_string(),
    };
    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    let event = StreamHubEvent::Subscribe {
        identifier,
        info: SubscriberInfo {
            id: Uuid::new(),
            sub_type: SubscribeType::RtmpPull,
            notify_info: NotifyInfo {
                request_url: format!("transcode://{room_id}/{media_id}"),
                remote_addr: String::new(),
            },
            sub_data_type: SubDataType::Frame,
        },
        result_sender,
    };
    if send_event_with_backpressure_timeout_for(
        &inner.stream_hub_event_sender,
        event,
        STREAMHUB_EVENT_SEND_TIMEOUT,
    )
    .await
    .is_err()
    {
        return None;
    }
    let Ok(Ok((data_receiver, _statistics))) = result_receiver.await else {
        return None;
    };
    let DataReceiver { frame_receiver, .. } = data_receiver;
    let frames = frame_receiver?;
    Some(TranscodeInput {
        frames,
        source_resolution: Arc::new(std::sync::Mutex::new(None)),
    })
}

/// Parse `width`/`height` out of an `onMetaData` FLV script tag body.
#[must_use]
pub(crate) fn parse_metadata_resolution(data: &[u8]) -> Option<(u32, u32)> {
    use synctv_xiu::bytesio::bytes_reader::BytesReader;
    use synctv_xiu::flv::amf0::{amf0_reader::Amf0Reader, Amf0ValueType};

    let mut reader = Amf0Reader::new(BytesReader::new(bytes::BytesMut::from(data)));
    let values = reader.read_all().ok()?;
    let map = values.iter().find_map(|value| match value {
        Amf0ValueType::EcmaArray(map) | Amf0ValueType::Object(map) => Some(map),
        _ => None,
    })?;
    let number = |key: &str| -> Option<u32> {
        match map.get(key)? {
            Amf0ValueType::Number(value) if *value >= 1.0 && *value <= f64::from(u32::MAX) => {
                // Range-checked above; fractional metadata floors.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                Some(*value as u32)
            }
            _ => None,
        }
    };
    Some((number("width")?, number("height")?))
}

#[cfg(test)]
mod tests;
