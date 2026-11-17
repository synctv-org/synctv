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
    /// `framerate` observed in the source `onMetaData`, when published.
    source_frame_rate: Option<f64>,
    /// `videodatarate` observed in the source `onMetaData`, when published.
    source_video_bitrate_kbps: Option<u32>,
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
                    if rung_state.waiting_viewers == 0
                        && rung_state.subscribers == 0
                        && let Some(running) = rung_state.running.as_mut()
                    {
                        running.idle_since.get_or_insert(now);
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
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                let scheduler = Arc::clone(&manager.inner);
                handle.spawn(async move {
                    scheduler_loop(scheduler).await;
                });
            }
            _ => {
                warn!("No Tokio runtime; transcode scheduler not started");
            }
        }
        manager
    }

    /// The configured quality rungs, best-first.
    #[must_use]
    pub fn presets(&self) -> &[TranscodePreset] {
        &self.inner.config.presets
    }

    /// The configured rungs worth offering for one source, best-first:
    /// without rungs that would only re-encode at the source's own quality
    /// (no resolution step-down and no frame-rate cap) once the source
    /// metadata is known, and without rungs this node already knows the
    /// source cannot provide (a verdict from an earlier scheduling attempt
    /// on this node).
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

    /// Record that a source stream has started publishing on this node:
    /// drop the previous session's metadata while keeping rung demand, so
    /// eligibility is decided from this session's `onMetaData` and not from
    /// whatever an earlier publish negotiated. Encoders restart the stream
    /// (new RTMP session, new metadata) to change output parameters; a
    /// shorter stream at 720p followed by a longer one at 1080p60 must not
    /// have its rungs judged against the 720p session forever.
    pub fn note_source_started(&self, room_id: &str, media_id: &str) {
        let mut state = self.inner.state.lock().expect("transcode state poisoned");
        if let Some(source) = state.get_mut(&(room_id.to_string(), media_id.to_string())) {
            source.source_resolution = None;
            source.source_frame_rate = None;
            source.source_video_bitrate_kbps = None;
        }
        drop(state);
        self.inner.schedule_wake.notify_one();
    }

    /// Consume hub publish lifecycle events to keep per-source session state
    /// honest. A source end cancels its rungs and drops its bookkeeping; a
    /// source start clears stale metadata from a previous session. Rung
    /// publications carry the rung stream name, which never matches a
    /// source's `(room, media)` key, so they land as harmless no-ops.
    pub fn spawn_lifecycle_watcher(
        self: &Arc<Self>,
        mut events: synctv_xiu::streamhub::define::BroadcastEventReceiver,
    ) -> tokio::task::JoinHandle<()> {
        use synctv_xiu::streamhub::define::BroadcastEvent;

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(BroadcastEvent::Publish { identifier, .. }) => {
                        let StreamIdentifier::Rtmp {
                            app_name,
                            stream_name,
                        } = identifier;
                        manager.note_source_started(&app_name, &stream_name);
                    }
                    Ok(BroadcastEvent::UnPublish { identifier, .. }) => {
                        let StreamIdentifier::Rtmp {
                            app_name,
                            stream_name,
                        } = identifier;
                        manager.note_source_ended(&app_name, &stream_name);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(count)) => {
                        warn!(
                            count,
                            "Transcode lifecycle watcher lagged; stale source metadata may persist until the next pick refreshes it"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        })
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
                if let Some(rung_state) = source.rungs.get_mut(rung)
                    && rung_state.running.take().is_some()
                {
                    wake = true;
                    match outcome {
                        Ok(()) => {
                            debug!(room_id, media_id, rung, "Transcode rung ended");
                        }
                        Err(TranscodeError::Unsupported(reason)) => {
                            warn!(
                                room_id,
                                media_id, rung, reason, "Transcode rung permanently unsupported"
                            );
                            rung_state.unsupported = Some(reason.clone());
                        }
                        Err(error) => {
                            warn!(room_id, media_id, rung, %error, "Transcode rung failed");
                            rung_state.last_failure = Some(now);
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
    // skip rungs that would only re-encode at the source's own quality.
    // `onMetaData` carries resolution and frame rate in one tag, so both
    // slots fill together; exit once either is observed.
    {
        let inner = Arc::clone(inner);
        let key = (room_id.clone(), media_id.clone());
        let resolution_slot = Arc::clone(&input.source_resolution);
        let frame_rate_slot = Arc::clone(&input.source_frame_rate);
        let video_bitrate_slot = Arc::clone(&input.source_video_bitrate_kbps);
        tokio::spawn(async move {
            loop {
                let (resolution, frame_rate, video_bitrate) = {
                    let resolution = *resolution_slot.lock().expect("resolution slot poisoned");
                    let frame_rate = *frame_rate_slot.lock().expect("frame-rate slot poisoned");
                    let video_bitrate = *video_bitrate_slot.lock().expect("bitrate slot poisoned");
                    (resolution, frame_rate, video_bitrate)
                };
                if resolution.is_some() || frame_rate.is_some() || video_bitrate.is_some() {
                    let mut state = inner.state.lock().expect("transcode state poisoned");
                    if let Some(source) = state.get_mut(&key) {
                        if let Some(resolution) = resolution {
                            source.source_resolution = Some(resolution);
                        }
                        if let Some(frame_rate) = frame_rate {
                            source.source_frame_rate = Some(frame_rate);
                        }
                        if let Some(video_bitrate) = video_bitrate {
                            source.source_video_bitrate_kbps = Some(video_bitrate);
                        }
                    }
                    return;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
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
    presets
        .iter()
        .filter(|preset| !rung_reencodes_same_quality(preset, source))
        .filter(|preset| {
            source
                .rungs
                .get(&preset.name)
                .is_none_or(|rung| rung.unsupported.is_none())
        })
        .collect()
}

/// The tier's video bitrate adjusted to the actual output shape.
///
/// `video_kbps` is the reference for the tier's height at 16:9 and at the
/// `max_fps` ceiling (matching common published ladders, which are quoted
/// per resolution and frame rate). The factor scales with the actual output
/// pixel count and with the output frame rate relative to the ceiling. This
/// is what the ffmpeg worker passes as `-maxrate`/`-bufsize` and what the
/// scheduler's bitrate gate compares against the source's own published
/// bitrate.
pub(crate) fn effective_video_kbps(preset: &TranscodePreset, metadata: &SourceMetadata) -> u32 {
    let mut factor = 1.0_f64;

    if let Some((source_width, source_height)) = metadata.resolution {
        let output_height = f64::from(preset.height.min(source_height));
        let output_width = output_height * f64::from(source_width) / f64::from(source_height);
        let reference = f64::from(preset.height) * f64::from(preset.height) * 16.0 / 9.0;
        let pixel_ratio = output_width * output_height / reference;
        factor *= pixel_ratio.clamp(0.25, 2.0);
    }

    if let Some(frame_rate) = metadata
        .frame_rate
        .filter(|rate| rate.is_finite() && *rate > 0.0)
    {
        let capped = frame_rate.round().min(f64::from(preset.max_fps));
        let fps_ratio = capped / f64::from(preset.max_fps);
        factor *= fps_ratio.clamp(0.5, 1.0);
    }

    // Range-checked by the clamp; the tier itself is validated to
    // 1..=100000 kbps and the factor is clamped to [0.125, 2].
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        ((f64::from(preset.video_kbps) * factor)
            .round()
            .clamp(64.0, 100_000.0)) as u32
    }
}

/// True when the rung would only re-encode at the source's own quality: its
/// effective bitrate is not below the source's published bitrate, or (when
/// no bitrate is published) its height does not step below the source height
/// and its frame-rate ceiling does not cap a higher source frame rate. Any
/// one genuine improvement alone makes the rung worth running: a same-height
/// rung can still halve the frame rate of a 1080p60 source.
fn rung_reencodes_same_quality(preset: &TranscodePreset, source: &SourceState) -> bool {
    // Bitrate gate (when the source publishes one): a rung exists to reduce
    // the data rate. A rung whose effective bitrate is not strictly below
    // the source's cannot improve on the original - re-encoding only adds
    // generation loss - and up-transcoding a starved source (e.g. a
    // high-resolution, high-framerate, near-zero-bitrate stream) would
    // amplify it into every rung at full ladder bitrates, which is exactly
    // the shape an abusive publish takes.
    if let Some(source_kbps) = source.source_video_bitrate_kbps {
        let effective = effective_video_kbps(
            preset,
            &SourceMetadata {
                resolution: source.source_resolution,
                frame_rate: source.source_frame_rate,
                video_bitrate_kbps: None,
            },
        );
        if effective >= source_kbps {
            return true;
        }
    }

    let height_pointless = source
        .source_resolution
        .is_some_and(|(_, height)| preset.height >= height);
    // An unknown source frame rate is treated as already within the ceiling;
    // fractional rates (29.97, 59.94) round to their nominal values.
    let fps_pointless = source.source_frame_rate.is_none_or(|fps| {
        let nominal = fps.round();
        nominal.is_finite() && nominal <= f64::from(preset.max_fps)
    });
    height_pointless && fps_pointless
}

/// Choose the rung with the most waiting viewers among the schedulable
/// ones: not already running, not permanently unsupported, not in the
/// failure cooldown, and not one that would only re-encode at the source's
/// own quality.
fn pick_most_demanded<'a>(
    sources: &'a HashMap<SourceKey, SourceState>,
    presets: &[TranscodePreset],
    now: std::time::Instant,
) -> Option<(&'a SourceKey, &'a str, u32)> {
    let mut best: Option<(&SourceKey, &str, u32)> = None;
    for (key, source) in sources {
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
            if rung_reencodes_same_quality(preset, source) {
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
        source_frame_rate: Arc::new(std::sync::Mutex::new(None)),
        source_video_bitrate_kbps: Arc::new(std::sync::Mutex::new(None)),
    })
}

/// Fields of interest parsed out of an `onMetaData` FLV script tag body.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct SourceMetadata {
    pub resolution: Option<(u32, u32)>,
    pub frame_rate: Option<f64>,
    pub video_bitrate_kbps: Option<u32>,
}

/// Parse `width`/`height`/`framerate` out of an `onMetaData` FLV script tag
/// body. Each field is optional on its own; encoders publish the frame rate
/// under either the `framerate` or `frame_rate` key.
#[must_use]
pub(crate) fn parse_source_metadata(data: &[u8]) -> SourceMetadata {
    use synctv_xiu::bytesio::bytes_reader::BytesReader;
    use synctv_xiu::flv::amf0::{Amf0ValueType, amf0_reader::Amf0Reader};

    let map = Amf0Reader::new(BytesReader::new(bytes::BytesMut::from(data)))
        .read_all()
        .ok()
        .and_then(|values| {
            values.iter().find_map(|value| match value {
                Amf0ValueType::EcmaArray(map) | Amf0ValueType::Object(map) => Some(map.clone()),
                _ => None,
            })
        });
    let Some(map) = map else {
        return SourceMetadata::default();
    };
    let number = |key: &str| -> Option<f64> {
        match map.get(key)? {
            Amf0ValueType::Number(value) => Some(*value),
            _ => None,
        }
    };
    let resolution = match (number("width"), number("height")) {
        (Some(width), Some(height))
            if (1.0..=f64::from(u32::MAX)).contains(&width)
                && (1.0..=f64::from(u32::MAX)).contains(&height) =>
        {
            // Range-checked above; fractional metadata floors.
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            Some((width as u32, height as u32))
        }
        _ => None,
    };
    // Sensible frame-rate envelope; rejects junk like 0 or -1 metadata.
    let frame_rate = number("framerate")
        .or_else(|| number("frame_rate"))
        .filter(|fps| (0.0..=240.0).contains(fps) && *fps > 0.0);
    // `videodatarate` is the RTMP/OBS convention for the source's video
    // bitrate in kbps; bounded to the same envelope the preset validator
    // uses.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let video_bitrate_kbps = number("videodatarate")
        .filter(|kbps| (1.0..=100_000.0).contains(kbps))
        .map(|kbps| kbps.round() as u32);
    SourceMetadata {
        resolution,
        frame_rate,
        video_bitrate_kbps,
    }
}

#[cfg(test)]
mod tests;
