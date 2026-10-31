//! Per-observation fair outbound queue for realtime sessions.
//!
//! Every transport (WebSocket, gRPC, WebTransport, HTTP realtime sessions)
//! funnels server messages through one [`MessageSender`]. Historically that
//! sender exposed at most two priority classes, so a burst on one observation
//! (e.g. a chat history replay) delayed or dropped traffic for unrelated
//! observations (e.g. playback state or WebRTC signaling) — cross-observation
//! head-of-line blocking.
//!
//! [`FairSessionSender`] replaces those shared queues with:
//!
//! - a **control lane** for connection-scoped messages (`Error`,
//!   `Termination`, `Notification`, `HeartbeatAck`) that must not wait behind
//!   bulk traffic;
//! - one bounded lane **per `observe_id`** (plus the synthetic `"webrtc"`
//!   lane), classified as *interactive* (playback state, WebRTC signaling)
//!   or *bulk* (chat, playlists, members, settings, history);
//! - a **weighted round-robin drain** that always prefers the control lane,
//!   gives interactive lanes a configurable weight over bulk lanes, and
//!   preserves FIFO order *within* each lane — the only ordering the
//!   protocol guarantees;
//! - **slow-client escalation** matching the previous WebSocket policy:
//!   evicting the oldest queued message keeps the newest state flowing, but
//!   `slow_client_drop_threshold` consecutive evictions (or any control-lane
//!   overflow) reports a failure so the transport disconnects the client,
//!   which then reconnects and resumes from its observation cursors.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use prost::Message as _;
use synctv_proto::client::{resource_event, ServerMessage};

use super::MessageSender;

/// Synthetic observation lane carrying WebRTC signaling events (voice chat
/// and media swarms). See `webrtc_event_server_message_for_current_connection`.
const WEBRTC_OBSERVE_ID: &str = "webrtc";

/// Default consecutive-eviction threshold before a slow client is disconnected.
const DEFAULT_SLOW_CLIENT_DROP_THRESHOLD: u32 = 10;

/// Default outbound channel capacity handed to the transport forwarder.
const DEFAULT_FORWARD_CAPACITY: usize = 128;

/// Interactive lanes are drained with this many picks per bulk-lane pick.
const DEFAULT_INTERACTIVE_WEIGHT: u32 = 4;

/// Byte budgets bound worst-case memory even when a single message is large
/// (e.g. playlist or chat snapshots). `encoded_len` is cheap in `prost`.
const DEFAULT_CONTROL_LANE_BYTES: usize = 256 * 1024;
const DEFAULT_INTERACTIVE_LANE_BYTES: usize = 1024 * 1024;
const DEFAULT_BULK_LANE_BYTES: usize = 4 * 1024 * 1024;

const DEFAULT_CONTROL_LANE_MESSAGES: usize = 64;
const DEFAULT_INTERACTIVE_LANE_MESSAGES: usize = 128;
const DEFAULT_BULK_LANE_MESSAGES: usize = 256;

/// Upper bound on concurrent observation lanes. `ResourceObserver` already
/// caps observations per connection; this defends the queue against
/// observe-id flooding.
const MAX_LANES: usize = 96;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaneClass {
    Control,
    Interactive,
    Bulk,
}

impl LaneClass {
    fn message_capacity(self, config: &FairQueueConfig) -> usize {
        match self {
            Self::Control => config.control_lane_messages,
            Self::Interactive => config.interactive_lane_messages,
            Self::Bulk => config.bulk_lane_messages,
        }
    }

    fn byte_capacity(self, config: &FairQueueConfig) -> usize {
        match self {
            Self::Control => config.control_lane_bytes,
            Self::Interactive => config.interactive_lane_bytes,
            Self::Bulk => config.bulk_lane_bytes,
        }
    }
}

/// Tunables for [`FairSessionSender`]. Defaults mirror the capacities and
/// escalation threshold the WebSocket transport used before per-observation
/// lanes were introduced.
#[derive(Debug, Clone)]
pub struct FairQueueConfig {
    pub control_lane_messages: usize,
    pub control_lane_bytes: usize,
    pub interactive_lane_messages: usize,
    pub interactive_lane_bytes: usize,
    pub bulk_lane_messages: usize,
    pub bulk_lane_bytes: usize,
    pub max_lanes: usize,
    pub slow_client_drop_threshold: u32,
    pub interactive_weight: u32,
    pub forward_capacity: usize,
}

impl Default for FairQueueConfig {
    fn default() -> Self {
        Self {
            control_lane_messages: DEFAULT_CONTROL_LANE_MESSAGES,
            control_lane_bytes: DEFAULT_CONTROL_LANE_BYTES,
            interactive_lane_messages: DEFAULT_INTERACTIVE_LANE_MESSAGES,
            interactive_lane_bytes: DEFAULT_INTERACTIVE_LANE_BYTES,
            bulk_lane_messages: DEFAULT_BULK_LANE_MESSAGES,
            bulk_lane_bytes: DEFAULT_BULK_LANE_BYTES,
            max_lanes: MAX_LANES,
            slow_client_drop_threshold: DEFAULT_SLOW_CLIENT_DROP_THRESHOLD,
            interactive_weight: DEFAULT_INTERACTIVE_WEIGHT,
            forward_capacity: DEFAULT_FORWARD_CAPACITY,
        }
    }
}

struct Lane {
    class: LaneClass,
    /// `(message, encoded size)` pairs; sizes are cached so eviction stays
    /// O(1) instead of re-encoding.
    messages: VecDeque<(ServerMessage, usize)>,
    bytes: usize,
}

impl Lane {
    fn new(class: LaneClass) -> Self {
        Self {
            class,
            messages: VecDeque::new(),
            bytes: 0,
        }
    }

    /// Enqueues `message`, evicting oldest entries until it fits both the
    /// message-count and byte budgets. Returns the number of evictions; an
    /// oversized single message still enqueues (newest state wins) unless
    /// even the front slot cannot be freed.
    fn push_bounded(&mut self, message: ServerMessage, config: &FairQueueConfig) -> usize {
        let size = message.encoded_len().max(1);
        let message_capacity = self.class.message_capacity(config);
        let byte_capacity = self.class.byte_capacity(config);

        let mut evicted = 0;
        while self.messages.len() >= message_capacity || self.bytes + size > byte_capacity {
            if self.messages.len() <= 1 && self.bytes + size > byte_capacity {
                // Freeing the last slot would still not fit an oversized
                // message: enqueue it anyway (bounded by the message cap) so
                // the newest state is delivered rather than nothing.
                break;
            }
            if let Some((_, dropped)) = self.messages.pop_front() {
                self.bytes = self.bytes.saturating_sub(dropped);
                evicted += 1;
            } else {
                break;
            }
        }
        if self.messages.len() < message_capacity {
            self.bytes += size;
            self.messages.push_back((message, size));
        }
        evicted
    }
}

struct Lanes {
    control: Lane,
    by_observe_id: HashMap<String, Lane>,
    /// Rotation order for round-robin fairness across observation lanes.
    rotation: VecDeque<String>,
    /// Remaining picks for the lane currently at the front of `rotation`;
    /// zero means the next visit starts with a fresh weight.
    current_picks: u32,
    /// Cumulative evictions across all lanes; kept at this level so reaping
    /// an empty lane does not lose its history.
    total_evicted: u64,
}

impl Lanes {
    fn new() -> Self {
        Self {
            control: Lane::new(LaneClass::Control),
            by_observe_id: HashMap::new(),
            rotation: VecDeque::new(),
            current_picks: 0,
            total_evicted: 0,
        }
    }

    fn lane_for(
        &mut self,
        observe_id: &str,
        class: LaneClass,
        max_lanes: usize,
    ) -> Option<&mut Lane> {
        if !self.by_observe_id.contains_key(observe_id) {
            if self.by_observe_id.len() >= max_lanes {
                return None;
            }
            self.by_observe_id
                .insert(observe_id.to_string(), Lane::new(class));
            self.rotation.push_back(observe_id.to_string());
        }
        self.by_observe_id.get_mut(observe_id)
    }

    fn total_evicted(&self) -> u64 {
        self.total_evicted
    }

    /// Pops the next message according to the scheduling policy: control
    /// lane first, then weighted round-robin across observation lanes.
    /// Lanes drained to empty are reaped so stale observations do not pin
    /// memory or lane-quota.
    fn pop_weighted(&mut self, interactive_weight: u32) -> Option<ServerMessage> {
        if !self.control.messages.is_empty() {
            return self
                .control
                .messages
                .pop_front()
                .map(|(message, _)| message);
        }

        while let Some(front_id) = self.rotation.front().cloned() {
            let Some(lane) = self.by_observe_id.get_mut(&front_id) else {
                self.rotation.pop_front();
                self.current_picks = 0;
                continue;
            };
            if lane.messages.is_empty() {
                self.by_observe_id.remove(&front_id);
                self.rotation.pop_front();
                self.current_picks = 0;
                continue;
            }
            if self.current_picks == 0 {
                self.current_picks = match lane.class {
                    LaneClass::Interactive => interactive_weight.max(1),
                    LaneClass::Bulk | LaneClass::Control => 1,
                };
            }
            let message = lane.messages.pop_front().map(|(message, _)| message);
            self.current_picks = self.current_picks.saturating_sub(1);
            let visit_finished = self.current_picks == 0;
            if visit_finished {
                self.rotation.rotate_left(1);
            }
            if lane.messages.is_empty() {
                // Reap immediately so the lane quota frees up for new
                // observations. When the visit already finished, the entry
                // moved to the back and is cleaned up lazily on its next
                // turn.
                self.by_observe_id.remove(&front_id);
                if !visit_finished {
                    self.rotation.pop_front();
                    self.current_picks = 0;
                }
            }
            return message;
        }
        None
    }
}

struct FairShared {
    lanes: parking_lot::Mutex<Lanes>,
    notify: tokio::sync::Notify,
    failed: AtomicBool,
    consecutive_drops: AtomicU32,
    config: FairQueueConfig,
}

impl FairShared {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        if self.failed.load(Ordering::Acquire) {
            return Err("Fair queue already failed: session is closing".to_string());
        }

        let message_kind = message_kind_name(&message);
        let (key, class) = classify(&message);
        // The classify result borrows `message`; own the lane id up front so
        // `message` can move into the lane.
        let observe_id = match key {
            LaneKey::Observe(id) => Some(id.to_string()),
            LaneKey::Control => None,
        };
        let mut lanes = self.lanes.lock();

        let evicted = match observe_id.as_deref() {
            None => {
                let evicted = lanes.control.push_bounded(message, &self.config);
                if evicted > 0 {
                    // Control traffic (errors, terminations, stateful
                    // notifications) must never be evicted: escalate so the
                    // client disconnects, reconnects, and resyncs.
                    self.fail();
                    return Err(format!(
                        "Control lane overflow evicted {evicted} message(s) (type={message_kind}): slow client disconnected"
                    ));
                }
                0
            }
            Some(id) => {
                let max_lanes = self.config.max_lanes;
                let Some(lane) = lanes.lane_for(id, class, max_lanes) else {
                    self.fail();
                    return Err(format!(
                        "Observation lane limit ({max_lanes}) exceeded: slow client disconnected"
                    ));
                };
                lane.push_bounded(message, &self.config)
            }
        };

        if evicted == 0 {
            self.consecutive_drops.store(0, Ordering::Relaxed);
        } else {
            lanes.total_evicted = lanes.total_evicted.saturating_add(evicted as u64);
            // Evicted the oldest message(s) in the lane to make room. Newer
            // state supersedes older snapshots for the same observation; the
            // client resyncs from its cursors across any gap after reconnect.
            let drops = self.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
            tracing::warn!(
                observe_id = observe_id.as_deref().unwrap_or("control"),
                evicted,
                consecutive_drops = drops,
                "Fair queue evicted oldest observation message (slow client)"
            );
            if drops >= self.config.slow_client_drop_threshold {
                self.fail();
                return Err(format!(
                    "Slow client disconnected: {drops} consecutive fair-queue evictions"
                ));
            }
        }

        drop(lanes);
        self.notify.notify_one();
        Ok(())
    }

    fn fail(&self) {
        self.failed.store(true, Ordering::Release);
        self.notify.notify_one();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaneKey<S> {
    Control,
    Observe(S),
}

/// Public lane classification for transports and tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageLane<'a> {
    /// Connection-scoped traffic (errors, terminations, stateful
    /// notifications, heartbeat acks): delivered ahead of all observations
    /// and never evicted.
    Control,
    /// Traffic belonging to one observation, delivered FIFO per observation.
    Observation {
        observe_id: &'a str,
        /// Latency-sensitive payloads (playback state, WebRTC signaling)
        /// drain with the interactive weight.
        interactive: bool,
    },
}

/// Classifies a server message into (lane key, lane class), borrowing the
/// observe id from the message.
fn classify(message: &ServerMessage) -> (LaneKey<&str>, LaneClass) {
    use synctv_proto::client::server_message::Message;
    let Some(message) = &message.message else {
        return (LaneKey::Control, LaneClass::Control);
    };
    match message {
        Message::ResourceObserved(_)
        | Message::ResourceObserveError(_)
        | Message::ResourceEvent(_) => {
            let (observe_id, payload) = match message {
                Message::ResourceObserved(observed) => (observed.observe_id.as_str(), None),
                Message::ResourceObserveError(error) => (error.observe_id.as_str(), None),
                Message::ResourceEvent(event) => {
                    (event.observe_id.as_str(), event.payload.as_ref())
                }
                _ => unreachable!("matched above"),
            };
            let class = match payload {
                Some(
                    resource_event::Payload::PlaybackState(_)
                    | resource_event::Payload::WebrtcEvent(_),
                ) => LaneClass::Interactive,
                _ if observe_id == WEBRTC_OBSERVE_ID => LaneClass::Interactive,
                _ => LaneClass::Bulk,
            };
            (LaneKey::Observe(observe_id), class)
        }
        Message::Error(_)
        | Message::Termination(_)
        | Message::Notification(_)
        | Message::HeartbeatAck(_) => (LaneKey::Control, LaneClass::Control),
    }
}

/// Classifies a server message into its fair-queue lane.
///
/// Ordering contract: `ResourceObserved`/`ResourceObserveError`/
/// `ResourceEvent` for one `observe_id` share that observation's lane, so
/// per-observation FIFO order is preserved end-to-end. Everything else is
/// connection-scoped control traffic.
#[must_use]
pub fn classify_message_lane(message: &ServerMessage) -> MessageLane<'_> {
    match classify(message) {
        (LaneKey::Control, _) => MessageLane::Control,
        (LaneKey::Observe(id), class) => MessageLane::Observation {
            observe_id: id,
            interactive: class == LaneClass::Interactive,
        },
    }
}

fn message_kind_name(message: &ServerMessage) -> &'static str {
    use synctv_proto::client::server_message::Message;
    match &message.message {
        Some(Message::HeartbeatAck(_)) => "HeartbeatAck",
        Some(Message::Error(_)) => "Error",
        Some(Message::Termination(_)) => "Termination",
        Some(Message::ResourceObserved(_)) => "ResourceObserved",
        Some(Message::ResourceEvent(_)) => "ResourceEvent",
        Some(Message::ResourceObserveError(_)) => "ResourceObserveError",
        Some(Message::Notification(_)) => "Notification",
        None => "None",
    }
}

/// [`MessageSender`] implementation with per-observation fair queuing.
///
/// Create one per session with [`FairSessionSender::new`] (spawns the drain
/// forwarder onto the current Tokio runtime and returns its receiver for the
/// transport write loop), or use [`FairSessionSender::unspawned`] with
/// [`Self::try_pop`]/[`Self::notified`] when the transport integrates the
/// drain into its own write task.
#[derive(Clone)]
pub struct FairSessionSender {
    shared: Arc<FairShared>,
}

impl FairSessionSender {
    pub fn new(config: FairQueueConfig) -> (Self, tokio::sync::mpsc::Receiver<ServerMessage>) {
        let (tx, rx) = tokio::sync::mpsc::channel(config.forward_capacity);
        let sender = Self::unspawned(config);
        let forwarder = sender.clone();
        tokio::spawn(async move {
            forwarder.run(tx).await;
        });
        (sender, rx)
    }

    pub fn unspawned(config: FairQueueConfig) -> Self {
        Self {
            shared: Arc::new(FairShared {
                lanes: parking_lot::Mutex::new(Lanes::new()),
                notify: tokio::sync::Notify::new(),
                failed: AtomicBool::new(false),
                consecutive_drops: AtomicU32::new(0),
                config,
            }),
        }
    }

    /// Pops the next message according to the fair scheduling policy, or
    /// `None` when every lane is currently empty.
    #[must_use]
    pub fn try_pop(&self) -> Option<ServerMessage> {
        let mut lanes = self.shared.lanes.lock();
        lanes.pop_weighted(self.shared.config.interactive_weight)
    }

    /// Waits until a message might be available (lane push or failure).
    pub async fn notified(&self) {
        self.shared.notify.notified().await;
    }

    /// True once the escalation policy has tripped; transports should treat
    /// this as a connection failure.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        self.shared.failed.load(Ordering::Acquire)
    }

    /// Total messages evicted across all lanes (observability).
    #[must_use]
    pub fn total_evicted(&self) -> u64 {
        self.shared.lanes.lock().total_evicted()
    }

    async fn run(self, tx: tokio::sync::mpsc::Sender<ServerMessage>) {
        loop {
            if let Some(message) = self.try_pop() {
                if tx.send(message).await.is_err() {
                    // Receiver dropped: transport is gone.
                    self.shared.fail();
                    return;
                }
            } else {
                if self.shared.failed.load(Ordering::Acquire) {
                    return;
                }
                self.shared.notify.notified().await;
            }
        }
    }
}

impl MessageSender for FairSessionSender {
    fn send(&self, message: ServerMessage) -> Result<(), String> {
        self.shared.send(message)
    }

    fn is_alive(&self) -> bool {
        !self.shared.failed.load(Ordering::Acquire)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_proto::client::server_message::Message as SMMessage;
    use synctv_proto::client::{ErrorMessage, ResourceEvent, ResourceObserveError};

    fn event_message(observe_id: &str, payload: Option<resource_event::Payload>) -> ServerMessage {
        ServerMessage {
            message: Some(SMMessage::ResourceEvent(ResourceEvent {
                observe_id: observe_id.to_string(),
                payload,
                event_cursor: None,
            })),
        }
    }

    fn control_message() -> ServerMessage {
        ServerMessage {
            message: Some(SMMessage::HeartbeatAck(Default::default())),
        }
    }

    /// Drains everything currently queued (no waiting) in policy order,
    /// describing each message as `control` or `event:<observe_id>`.
    fn drain_all(sender: &FairSessionSender) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(message) = sender.try_pop() {
            out.push(match &message.message {
                Some(SMMessage::ResourceEvent(event)) => format!("event:{}", event.observe_id),
                Some(SMMessage::ResourceObserveError(error)) => {
                    format!("event:{}", error.observe_id)
                }
                Some(SMMessage::ResourceObserved(observed)) => {
                    format!("event:{}", observed.observe_id)
                }
                _ => "control".to_string(),
            });
        }
        out
    }

    #[test]
    fn control_traffic_precedes_observation_traffic() {
        let sender = FairSessionSender::unspawned(FairQueueConfig::default());
        sender
            .send(event_message("chat", None))
            .expect("fair queue send must succeed");
        sender
            .send(control_message())
            .expect("fair queue send must succeed");
        assert_eq!(
            drain_all(&sender),
            vec!["control", "event:chat"],
            "control lane must be drained first"
        );
    }

    #[test]
    fn bulk_lanes_round_robin_and_fifo_within_lane() {
        let sender = FairSessionSender::unspawned(FairQueueConfig::default());
        for lane in ["chat", "playlist"] {
            for _ in 0..3 {
                sender
                    .send(event_message(lane, None))
                    .expect("fair queue send must succeed");
            }
        }
        assert_eq!(
            drain_all(&sender),
            vec![
                "event:chat",
                "event:playlist",
                "event:chat",
                "event:playlist",
                "event:chat",
                "event:playlist",
            ],
            "bulk lanes interleave fairly and stay FIFO within a lane"
        );
    }

    #[test]
    fn interactive_lanes_are_weighted() {
        let mut config = FairQueueConfig::default();
        config.interactive_weight = 3;
        let sender = FairSessionSender::unspawned(config);
        let webrtc_payload = resource_event::Payload::WebrtcEvent(Default::default());
        for _ in 0..3 {
            // Insert interactive traffic first so its lane leads rotation;
            // the assertion then pins the weight semantics deterministically.
            sender
                .send(event_message("webrtc", Some(webrtc_payload.clone())))
                .expect("fair queue send must succeed");
            sender
                .send(event_message("chat", None))
                .expect("fair queue send must succeed");
        }
        assert_eq!(
            drain_all(&sender),
            vec![
                "event:webrtc",
                "event:webrtc",
                "event:webrtc",
                "event:chat",
                "event:chat",
                "event:chat",
            ],
            "one interactive visit drains `interactive_weight` messages before bulk lanes"
        );
    }

    #[test]
    fn playback_state_events_are_interactive() {
        let interactive = event_message(
            "playback_state",
            Some(resource_event::Payload::PlaybackState(Default::default())),
        );
        let (key, class) = classify(&interactive);
        assert_eq!(key, LaneKey::Observe("playback_state"));
        assert_eq!(class, LaneClass::Interactive);

        let bulk = event_message("chat", None);
        let (key, class) = classify(&bulk);
        assert_eq!(key, LaneKey::Observe("chat"));
        assert_eq!(class, LaneClass::Bulk);
    }

    #[test]
    fn slow_client_escalates_after_consecutive_evictions() {
        let mut config = FairQueueConfig::default();
        config.bulk_lane_messages = 2;
        config.slow_client_drop_threshold = 3;
        let sender = FairSessionSender::unspawned(config);
        assert!(sender.send(event_message("chat", None)).is_ok());
        assert!(sender.send(event_message("chat", None)).is_ok());
        assert!(sender.send(event_message("chat", None)).is_ok()); // evicts 1
        assert!(sender.send(event_message("chat", None)).is_ok()); // evicts 2
        let result = sender.send(event_message("chat", None)); // evicts 3 -> disconnect
        assert!(result.is_err());
        assert!(sender.is_failed());
        assert_eq!(sender.total_evicted(), 3);
    }

    #[test]
    fn evictions_reset_once_traffic_flows_again() {
        let mut config = FairQueueConfig::default();
        config.bulk_lane_messages = 1;
        config.slow_client_drop_threshold = 3;
        let sender = FairSessionSender::unspawned(config);
        for _ in 0..10 {
            // Each send evicts the previous message (capacity 1), but the
            // queue is drained in between, resetting the counter.
            assert!(sender.send(event_message("chat", None)).is_ok());
            assert!(sender.try_pop().is_some());
        }
        assert!(!sender.is_failed());
    }

    #[test]
    fn byte_budget_evicts_oldest() {
        let mut config = FairQueueConfig::default();
        // An empty-payload ResourceEvent encodes to well under 16 bytes, so
        // a 16-byte budget holds at most one message.
        config.bulk_lane_bytes = 16;
        config.bulk_lane_messages = 10;
        let sender = FairSessionSender::unspawned(config);
        for _ in 0..4 {
            assert!(
                sender.send(event_message("chat", None)).is_ok(),
                "evictions must absorb the overflow instead of failing"
            );
        }
        let drained = drain_all(&sender);
        assert!(
            drained.len() <= 2,
            "byte budget must bound the lane: {drained:?}"
        );
        assert!(sender.total_evicted() >= 1);
    }

    #[test]
    fn oversized_single_message_still_delivered() {
        let mut config = FairQueueConfig::default();
        config.bulk_lane_bytes = 8;
        config.bulk_lane_messages = 2;
        let sender = FairSessionSender::unspawned(config);
        assert!(sender.send(event_message("chat", None)).is_ok());
        assert_eq!(drain_all(&sender).len(), 1, "newest state must survive");
    }

    #[test]
    fn lane_cap_rejects_new_observation_ids() {
        let mut config = FairQueueConfig::default();
        config.max_lanes = 2;
        let sender = FairSessionSender::unspawned(config);
        assert!(sender.send(event_message("a", None)).is_ok());
        assert!(sender.send(event_message("b", None)).is_ok());
        let result = sender.send(event_message("c", None));
        assert!(result.is_err());
        assert!(sender.is_failed());
    }

    #[test]
    fn empty_lanes_are_reaped() {
        let mut config = FairQueueConfig::default();
        config.max_lanes = 1;
        let sender = FairSessionSender::unspawned(config);
        assert!(sender.send(event_message("a", None)).is_ok());
        assert!(sender.try_pop().is_some()); // lane now empty and reaped
        assert!(
            sender.send(event_message("b", None)).is_ok(),
            "a reaped lane frees its slot for new observe ids"
        );
    }

    #[test]
    fn control_overflow_disconnects_immediately() {
        let mut config = FairQueueConfig::default();
        config.control_lane_messages = 2;
        config.control_lane_bytes = usize::MAX;
        let sender = FairSessionSender::unspawned(config);
        assert!(sender.send(control_message()).is_ok());
        assert!(sender.send(control_message()).is_ok());
        let result = sender.send(ServerMessage {
            message: Some(SMMessage::Error(ErrorMessage::default())),
        });
        assert!(result.is_err(), "control evictions must escalate");
        assert!(sender.is_failed());
    }

    #[test]
    fn observe_errors_share_the_observation_lane() {
        let sender = FairSessionSender::unspawned(FairQueueConfig::default());
        sender
            .send(event_message("chat", None))
            .expect("fair queue send must succeed");
        sender
            .send(ServerMessage {
                message: Some(SMMessage::ResourceObserveError(ResourceObserveError {
                    observe_id: "chat".to_string(),
                    error: None,
                })),
            })
            .expect("fair queue send must succeed");
        // Same lane -> FIFO ordering between event and observe error holds.
        assert_eq!(
            drain_all(&sender),
            vec!["event:chat", "event:chat"],
            "ResourceObserveError routes to the observation lane"
        );
    }

    #[tokio::test]
    async fn spawned_forwarder_delivers_in_policy_order() {
        let (sender, mut rx) = FairSessionSender::new(FairQueueConfig::default());
        sender
            .send(event_message("chat", None))
            .expect("fair queue send must succeed");
        sender
            .send(control_message())
            .expect("fair queue send must succeed");
        let first = rx.recv().await.expect("first message");
        assert!(matches!(first.message, Some(SMMessage::HeartbeatAck(_))));
        let second = rx.recv().await.expect("second message");
        assert!(matches!(second.message, Some(SMMessage::ResourceEvent(_))));
        drop(sender);
    }
}
