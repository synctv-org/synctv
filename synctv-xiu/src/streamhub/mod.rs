use crate::flv::define::aac_packet_type;
use define::{
    FrameDataReceiver, PacketDataReceiver, PacketDataSender, StatisticData, StatisticDataReceiver,
    StatisticDataSender,
};

use define::PacketData;

pub mod define;
pub mod errors;
pub mod statistics;
pub mod stream;
pub mod utils;

use {
    define::{
        BroadcastEvent, BroadcastEventSender, DataReceiver, DataSender, FrameData, FrameDataSender,
        StreamHubEvent, StreamHubEventReceiver, StreamHubEventSender, SubscriberInfo,
        TStreamHandler, TransceiverEvent, TransceiverEventReceiver, TransceiverEventSender,
    },
    errors::{StreamHubError, StreamHubErrorValue},
    std::collections::HashMap,
    std::sync::atomic::{AtomicU64, Ordering},
    std::sync::Arc,
    stream::StreamIdentifier,
    tokio::sync::{broadcast, mpsc, Mutex},
    tokio::task::JoinSet,
    utils::Uuid,
};

fn map_task_join_error(task_name: &str, error: tokio::task::JoinError) -> StreamHubError {
    let detail = if error.is_panic() {
        format!("{task_name} panicked")
    } else if error.is_cancelled() {
        format!("{task_name} was cancelled")
    } else {
        format!("{task_name} failed: {error}")
    };

    StreamHubError {
        value: StreamHubErrorValue::InternalTaskError(detail),
    }
}

/// Tracks per-subscriber frame drop counts for diagnostics.
struct SubscriberDropCounter {
    sender: FrameDataSender,
    drop_count: Arc<AtomicU64>,
}

/// Tracks per-subscriber packet drop counts for diagnostics.
struct PacketSubscriberDropCounter {
    sender: PacketDataSender,
    drop_count: Arc<AtomicU64>,
}

use statistics::StatisticsStream;

const EVENT_SEND_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);
const EVENT_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

pub async fn send_event_with_backpressure_timeout(
    sender: &StreamHubEventSender,
    event: StreamHubEvent,
) -> Result<(), StreamHubError> {
    match sender.try_send(event) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Closed(_)) => Err(StreamHubError {
            value: StreamHubErrorValue::SendError,
        }),
        Err(mpsc::error::TrySendError::Full(event)) => {
            let send_future = async {
                let mut pending = event;
                loop {
                    match sender.try_send(pending) {
                        Ok(()) => return Ok(()),
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            return Err(StreamHubError {
                                value: StreamHubErrorValue::SendError,
                            });
                        }
                        Err(mpsc::error::TrySendError::Full(event)) => {
                            pending = event;
                            tokio::time::sleep(EVENT_SEND_RETRY_DELAY).await;
                        }
                    }
                }
            };

            match tokio::time::timeout(EVENT_SEND_TIMEOUT, send_future).await {
                Ok(result) => result,
                Err(_) => Err(StreamHubError {
                    value: StreamHubErrorValue::SendError,
                }),
            }
        }
    }
}

pub enum SubscribeWithRollbackError {
    Timeout,
    StreamHub(StreamHubError),
}

impl From<StreamHubError> for SubscribeWithRollbackError {
    fn from(error: StreamHubError) -> Self {
        Self::StreamHub(error)
    }
}

pub async fn subscribe_with_rollback_on_timeout(
    sender: &StreamHubEventSender,
    identifier: StreamIdentifier,
    info: SubscriberInfo,
    timeout: std::time::Duration,
) -> Result<
    (
        define::DataReceiver,
        Option<define::StatisticDataSender>,
    ),
    SubscribeWithRollbackError,
> {
    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();

    send_event_with_backpressure_timeout(
        sender,
        StreamHubEvent::Subscribe {
            identifier: identifier.clone(),
            info: info.clone(),
            result_sender,
        },
    )
    .await?;

    match tokio::time::timeout(timeout, result_receiver).await {
        Ok(result) => result
            .map_err(|_| StreamHubError {
                value: StreamHubErrorValue::SendError,
            })?
            .map_err(SubscribeWithRollbackError::StreamHub),
        Err(_) => {
            if let Err(err) = send_event_with_backpressure_timeout(
                sender,
                StreamHubEvent::UnSubscribe { identifier, info },
            )
            .await
            {
                tracing::warn!("subscribe timeout rollback failed: {err}");
            }

            Err(SubscribeWithRollbackError::Timeout)
        }
    }
}

pub fn spawn_event_delivery_with_backpressure_timeout(
    sender: StreamHubEventSender,
    event: StreamHubEvent,
) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                if let Err(err) = send_event_with_backpressure_timeout(&sender, event).await {
                    tracing::warn!("deferred event delivery failed: {err}");
                }
            });
        }
        Err(_) => {
            if let Err(err) = sender.try_send(event) {
                tracing::warn!("deferred event delivery failed without runtime: {err}");
            }
        }
    }
}

//Receive audio data/video data/meta data/media info from a publisher and send to players/subscribers
//Receive statistic information from a publisher and send to api callers.
pub struct StreamDataTransceiver {
    //used for receiving Audio/Video data from publishers
    data_receiver: DataReceiver,
    //used for receiving event
    event_receiver: TransceiverEventReceiver,
    //used for sending audio/video frame data to players/subscribers (with drop counters)
    id_to_frame_sender: Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
    //used for sending audio/video packet data to players/subscribers (with drop counters)
    id_to_packet_sender: Arc<Mutex<HashMap<Uuid, PacketSubscriberDropCounter>>>,
    /// Generation counter for frame subscriber set. Bumped on subscribe/unsubscribe.
    /// Fan-out loop caches the snapshot and only rebuilds when generation changes.
    frame_generation: Arc<AtomicU64>,
    /// Generation counter for packet subscriber set.
    packet_generation: Arc<AtomicU64>,
    //publisher and subscribers use this sender to submit statistical data
    statistic_data_sender: StatisticDataSender,
    //used for receiving statistical data from publishers and subscribers
    statistic_data_receiver: StatisticDataReceiver,
    //The publisher and subscribers's statistics data of a stream need to be aggregated and sent to the caller as needed.
    statistic_data: Arc<Mutex<StatisticsStream>>,
    //a hander implement by protocols, such as rtmp, webrtc, http-flv, hls
    stream_handler: Arc<dyn TStreamHandler>,
}

/// How often to log per-subscriber drop warnings (every N drops).
const DROP_LOG_INTERVAL: u64 = 100;

impl StreamDataTransceiver {
    fn new(
        data_receiver: DataReceiver,
        event_receiver: TransceiverEventReceiver,
        identifier: StreamIdentifier,
        h: Arc<dyn TStreamHandler>,
    ) -> Self {
        let (statistic_data_sender, statistic_data_receiver) =
            mpsc::channel(define::STATISTIC_DATA_CHANNEL_CAPACITY);
        Self {
            data_receiver,
            event_receiver,
            statistic_data_sender,
            statistic_data_receiver,
            id_to_frame_sender: Arc::new(Mutex::new(HashMap::new())),
            id_to_packet_sender: Arc::new(Mutex::new(HashMap::new())),
            frame_generation: Arc::new(AtomicU64::new(0)),
            packet_generation: Arc::new(AtomicU64::new(0)),
            stream_handler: h,
            statistic_data: Arc::new(Mutex::new(StatisticsStream::new(identifier))),
        }
    }

    /// Snapshot the frame senders map and fan out to all subscribers without holding the lock.
    /// Collects closed/failed subscriber IDs and removes them in a separate lock acquisition.
    /// Drop counters are snapshotted as Arc<AtomicU64> so no lock is needed during fan-out.
    /// `FrameData` uses `Bytes` internally so clone is O(1) reference count bump.
    fn fan_out_frame(
        snapshot: &[(Uuid, FrameDataSender, Arc<AtomicU64>)],
        data: &FrameData,
    ) -> Vec<Uuid> {
        let mut closed_ids = Vec::new();
        for (id, sender, drop_count) in snapshot {
            match sender.try_send(data.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let prev = drop_count.fetch_add(1, Ordering::Relaxed);
                    if (prev + 1) % DROP_LOG_INTERVAL == 0 {
                        tracing::warn!(
                            "Subscriber {} dropped {} frames due to backpressure",
                            id,
                            prev + 1
                        );
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    closed_ids.push(*id);
                }
            }
        }
        closed_ids
    }

    /// Snapshot the packet senders map and fan out to all subscribers without holding the lock.
    /// Drop counters are snapshotted as Arc<AtomicU64> so no lock is needed during fan-out.
    fn fan_out_packet(
        snapshot: &[(Uuid, PacketDataSender, Arc<AtomicU64>)],
        data: &PacketData,
    ) -> Vec<Uuid> {
        let mut closed_ids = Vec::new();
        for (id, sender, drop_count) in snapshot {
            match sender.try_send(data.clone()) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    let prev = drop_count.fetch_add(1, Ordering::Relaxed);
                    if (prev + 1) % DROP_LOG_INTERVAL == 0 {
                        tracing::warn!(
                            "Packet subscriber {} dropped {} packets due to backpressure",
                            id,
                            prev + 1
                        );
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    closed_ids.push(*id);
                }
            }
        }
        closed_ids
    }

    /// Fan out frame data using a cached snapshot. Only rebuilds the snapshot when
    /// the generation counter indicates subscribers have changed (subscribe/unsubscribe).
    /// This avoids acquiring the lock on every frame in the hot path.
    async fn receive_frame_data(
        data: Option<FrameData>,
        frame_senders: &Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
        generation: &Arc<AtomicU64>,
        cached_snapshot: &mut Vec<(Uuid, FrameDataSender, Arc<AtomicU64>)>,
        cached_gen: &mut u64,
        statistics_data: &Arc<Mutex<StatisticsStream>>,
    ) {
        if let Some(val) = data {
            // Only rebuild snapshot when subscriber set has changed
            let current_gen = generation.load(Ordering::Acquire);
            if current_gen != *cached_gen {
                let guard = frame_senders.lock().await;
                *cached_snapshot = guard
                    .iter()
                    .map(|(id, sc)| (*id, sc.sender.clone(), Arc::clone(&sc.drop_count)))
                    .collect();
                drop(guard);
                *cached_gen = current_gen;
            }

            if cached_snapshot.is_empty() {
                return;
            }

            // Fan out to all subscribers without holding any lock.
            // FrameData uses Bytes internally so clone is O(1) Arc reference bump.
            let closed_ids = Self::fan_out_frame(cached_snapshot, &val);

            // Remove closed subscribers and bump generation
            if !closed_ids.is_empty() {
                let closed_count = closed_ids.len();
                for id in &closed_ids {
                    frame_senders.lock().await.remove(id);
                    tracing::debug!("Removed closed frame subscriber: {}", id);
                }
                // Bump generation so next call rebuilds snapshot
                generation.fetch_add(1, Ordering::Release);
                // Invalidate cached snapshot immediately
                *cached_gen = cached_gen.wrapping_add(u64::MAX); // Force mismatch

                // Decrement subscriber_count for subscribers removed by fan-out.
                // Without this, subscriber_count only decrements on explicit UnSubscribe
                // events, causing permanently inflated counts when subscribers disconnect
                // without sending UnSubscribe.
                let mut stats = statistics_data.lock().await;
                stats.subscriber_count = stats.subscriber_count.saturating_sub(closed_count);
            }
        }
    }

    fn receive_frame_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: FrameDataReceiver,
        frame_senders: Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
        generation: Arc<AtomicU64>,
        event_sender: Option<mpsc::Sender<TransceiverEvent>>,
        statistics_data: Arc<Mutex<StatisticsStream>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            // Cached snapshot: only rebuilt when generation counter changes
            let mut cached_snapshot: Vec<(Uuid, FrameDataSender, Arc<AtomicU64>)> = Vec::new();
            let mut cached_gen: u64 = u64::MAX; // Force initial rebuild

            loop {
                tokio::select! {
                    data = receiver.recv() => {
                        if data.is_none() {
                            // H-3: Publisher dropped — send synthetic UnPublish
                            // to trigger cleanup of the streams HashMap entry.
                            tracing::warn!("Frame data receiver closed (publisher dropped)");
                            if let Some(sender) = &event_sender {
                                // Use a retry loop to ensure UnPublish is not silently
                                // dropped when the channel is full (zombie stream prevention).
                                let mut sent = false;
                                for _ in 0..3 {
                                    match sender.try_send(TransceiverEvent::UnPublish {}) {
                                        Ok(()) => { sent = true; break; }
                                        Err(mpsc::error::TrySendError::Full(_)) => {
                                            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                                        }
                                        Err(e) => {
                                            tracing::error!("Failed to send synthetic UnPublish (channel closed): {e}");
                                            break;
                                        }
                                    }
                                }
                                if !sent {
                                    tracing::error!("Failed to send synthetic UnPublish after retries: channel full or closed (zombie stream risk)");
                                }
                            }
                            break;
                        }
                        Self::receive_frame_data(
                            data,
                            &frame_senders,
                            &generation,
                            &mut cached_snapshot,
                            &mut cached_gen,
                            &statistics_data,
                        ).await;
                    }
                    _ = exit.recv()=>{
                        break;
                    }
                }
            }
        })
    }

    /// Fan out packet data using a cached snapshot, same generation-counter approach as frames.
    async fn receive_packet_data(
        data: Option<PacketData>,
        packet_senders: &Arc<Mutex<HashMap<Uuid, PacketSubscriberDropCounter>>>,
        generation: &Arc<AtomicU64>,
        cached_snapshot: &mut Vec<(Uuid, PacketDataSender, Arc<AtomicU64>)>,
        cached_gen: &mut u64,
        statistics_data: &Arc<Mutex<StatisticsStream>>,
    ) {
        if let Some(val) = data {
            let current_gen = generation.load(Ordering::Acquire);
            if current_gen != *cached_gen {
                let guard = packet_senders.lock().await;
                *cached_snapshot = guard
                    .iter()
                    .map(|(id, sc)| (*id, sc.sender.clone(), Arc::clone(&sc.drop_count)))
                    .collect();
                drop(guard);
                *cached_gen = current_gen;
            }

            if cached_snapshot.is_empty() {
                return;
            }

            let closed_ids = Self::fan_out_packet(cached_snapshot, &val);

            if !closed_ids.is_empty() {
                let closed_count = closed_ids.len();
                for id in &closed_ids {
                    packet_senders.lock().await.remove(id);
                    tracing::debug!("Removed closed packet subscriber: {}", id);
                }
                generation.fetch_add(1, Ordering::Release);
                *cached_gen = cached_gen.wrapping_add(u64::MAX);

                // Decrement subscriber_count for subscribers removed by fan-out.
                let mut stats = statistics_data.lock().await;
                stats.subscriber_count = stats.subscriber_count.saturating_sub(closed_count);
            }
        }
    }

    fn receive_packet_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: PacketDataReceiver,
        packet_senders: Arc<Mutex<HashMap<Uuid, PacketSubscriberDropCounter>>>,
        generation: Arc<AtomicU64>,
        event_sender: Option<mpsc::Sender<TransceiverEvent>>,
        statistics_data: Arc<Mutex<StatisticsStream>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut cached_snapshot: Vec<(Uuid, PacketDataSender, Arc<AtomicU64>)> = Vec::new();
            let mut cached_gen: u64 = u64::MAX;

            loop {
                tokio::select! {
                    data = receiver.recv() => {
                        if data.is_none() {
                            // H-3: Publisher dropped — send synthetic UnPublish
                            tracing::warn!("Packet data receiver closed (publisher dropped)");
                            if let Some(sender) = &event_sender {
                                // Use a retry loop to ensure UnPublish is not silently
                                // dropped when the channel is full (zombie stream prevention).
                                let mut sent = false;
                                for _ in 0..3 {
                                    match sender.try_send(TransceiverEvent::UnPublish {}) {
                                        Ok(()) => { sent = true; break; }
                                        Err(mpsc::error::TrySendError::Full(_)) => {
                                            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
                                        }
                                        Err(e) => {
                                            tracing::error!("Failed to send synthetic UnPublish (channel closed): {e}");
                                            break;
                                        }
                                    }
                                }
                                if !sent {
                                    tracing::error!("Failed to send synthetic UnPublish after retries: channel full or closed (zombie stream risk)");
                                }
                            }
                            break;
                        }
                        Self::receive_packet_data(
                            data,
                            &packet_senders,
                            &generation,
                            &mut cached_snapshot,
                            &mut cached_gen,
                            &statistics_data,
                        ).await;
                    }
                    _ = exit.recv()=>{
                        break;
                    }
                }
            }
        })
    }

    async fn receive_statistics_data(
        data: Option<StatisticData>,
        statistics_data: &Arc<Mutex<StatisticsStream>>,
    ) {
        if let Some(val) = data {
            match val {
                StatisticData::Audio {
                    uuid,
                    data_size,
                    aac_packet_type,
                    duration: _,
                } => {
                    let mut guard = statistics_data.lock().await;
                    if let Some(uid) = uuid {
                        if let Some(sub) = guard.subscribers.get_mut(&uid) {
                            sub.send_bytes += data_size;
                        }
                        guard.total_send_bytes += data_size;
                    } else {
                        if aac_packet_type == aac_packet_type::AAC_RAW {
                            guard.publisher.audio.recv_bytes += data_size;
                        }
                        guard.total_recv_bytes += data_size;
                    }
                }
                StatisticData::Video {
                    uuid,
                    data_size,
                    frame_count,
                    is_key_frame,
                    duration: _,
                } => {
                    let mut guard = statistics_data.lock().await;
                    if let Some(uid) = uuid {
                        if let Some(sub) = guard.subscribers.get_mut(&uid) {
                            sub.send_bytes += data_size;
                            sub.total_send_bytes += data_size;
                        }
                        guard.total_send_bytes += data_size;
                    } else {
                        guard.total_recv_bytes += data_size;
                        guard.publisher.video.recv_bytes += data_size;
                        guard.publisher.video.recv_frame_count += frame_count;
                        guard.publisher.recv_bytes += data_size;
                        if let Some(is_key) = is_key_frame {
                            if is_key {
                                guard.publisher.video.gop =
                                    guard.publisher.video.recv_frame_count_for_gop;
                                guard.publisher.video.recv_frame_count_for_gop = 1;
                            } else {
                                guard.publisher.video.recv_frame_count_for_gop += frame_count;
                            }
                        }
                    }
                }
                StatisticData::AudioCodec {
                    sound_format,
                    profile,
                    samplerate,
                    channels,
                } => {
                    let audio_codec_data = &mut statistics_data.lock().await.publisher.audio;
                    audio_codec_data.sound_format = sound_format;
                    audio_codec_data.profile = profile;
                    audio_codec_data.samplerate = samplerate;
                    audio_codec_data.channels = channels;
                }
                StatisticData::VideoCodec {
                    codec,
                    profile,
                    level,
                    width,
                    height,
                } => {
                    let video_codec_data = &mut statistics_data.lock().await.publisher.video;
                    video_codec_data.codec = codec;
                    video_codec_data.profile = profile;
                    video_codec_data.level = level;
                    video_codec_data.width = width;
                    video_codec_data.height = height;
                }
                StatisticData::HevcCodec {
                    codec,
                    profile: _,
                    level: _,
                    width,
                    height,
                } => {
                    let video_codec_data = &mut statistics_data.lock().await.publisher.video;
                    video_codec_data.codec = codec;
                    video_codec_data.width = width;
                    video_codec_data.height = height;
                }
                StatisticData::Publisher {
                    id,
                    remote_addr,
                    start_time,
                } => {
                    let publisher = &mut statistics_data.lock().await.publisher;
                    publisher.id = id;
                    publisher.remote_address = remote_addr;

                    publisher.start_time = start_time;
                }
                StatisticData::Subscriber {
                    id,
                    remote_addr,
                    sub_type,
                    start_time,
                } => {
                    let subscriber = &mut statistics_data.lock().await.subscribers;
                    let sub = statistics::StatisticSubscriber {
                        id,
                        remote_address: remote_addr,
                        sub_type,
                        start_time,
                        send_bitrate: 0,
                        send_bytes: 0,
                        total_send_bytes: 0,
                    };
                    subscriber.insert(id, sub);
                }
            }
        }
    }

    fn receive_statistics_data_loop(
        mut exit_receive: broadcast::Receiver<()>,
        exit_caclulate: broadcast::Receiver<()>,
        mut receiver: StatisticDataReceiver,
        statistics_data: Arc<Mutex<StatisticsStream>>,
    ) -> tokio::task::JoinHandle<()> {
        let mut statistic_calculate =
            statistics::StatisticsCalculate::new(statistics_data.clone(), exit_caclulate);
        tokio::spawn(async move { statistic_calculate.start().await });

        tokio::spawn(async move {
            loop {
                tokio::select! {
                    data = receiver.recv()  =>
                    {
                        if data.is_none() {
                            break;
                        }
                        Self::receive_statistics_data(data, &statistics_data).await;
                    }
                    _ = exit_receive.recv()=>{
                        break;
                    }
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)] // Internal plumbing for transceiver event loop
    fn receive_event_loop(
        stream_handler: Arc<dyn TStreamHandler>,
        exit: broadcast::Sender<()>,
        mut receiver: TransceiverEventReceiver,
        packet_senders: Arc<Mutex<HashMap<Uuid, PacketSubscriberDropCounter>>>,
        frame_senders: Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
        frame_generation: Arc<AtomicU64>,
        packet_generation: Arc<AtomicU64>,
        statistic_sender: StatisticDataSender,
        statistics_data: Arc<Mutex<StatisticsStream>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                let Some(val) = receiver.recv().await else {
                    if let Err(err) = exit.send(()) {
                        tracing::debug!(
                            "receive_event_loop: shutdown broadcast had no receivers: {err}"
                        );
                    }
                    break;
                };

                match val {
                    TransceiverEvent::Subscribe {
                        sender,
                        info,
                        result_sender,
                    } => {
                        if let Err(err) = stream_handler
                            .send_prior_data(sender.clone(), info.sub_type)
                            .await
                        {
                            // A single subscriber's channel may be closed before
                            // prior data finishes sending. Skip this subscriber
                            // instead of breaking the entire event loop.
                            tracing::warn!("receive_event_loop send_prior_data err (skipping subscriber): {err}");
                            continue;
                        }
                        match sender {
                            DataSender::Frame {
                                sender: frame_sender,
                            } => {
                                frame_senders.lock().await.insert(
                                    info.id,
                                    SubscriberDropCounter {
                                        sender: frame_sender,
                                        drop_count: Arc::new(AtomicU64::new(0)),
                                    },
                                );
                                // Bump generation so fan-out loop rebuilds snapshot
                                frame_generation.fetch_add(1, Ordering::Release);
                            }
                            DataSender::Packet {
                                sender: packet_sender,
                            } => {
                                packet_senders.lock().await.insert(
                                    info.id,
                                    PacketSubscriberDropCounter {
                                        sender: packet_sender,
                                        drop_count: Arc::new(AtomicU64::new(0)),
                                    },
                                );
                                packet_generation.fetch_add(1, Ordering::Release);
                            }
                        }

                        if let Err(err) = result_sender.send(statistic_sender.clone()) {
                            tracing::error!("receive_event_loop:send statistic send err :{err:?} ");
                        }

                        let mut statistics_data = statistics_data.lock().await;
                        statistics_data.subscriber_count += 1;
                    }
                    TransceiverEvent::UnSubscribe { info } => {
                        // Remove from both sender maps and update statistics
                        // in a single logical block to minimize lock hold times.
                        {
                            frame_senders.lock().await.remove(&info.id);
                            let mut ps = packet_senders.lock().await;

                            ps.remove(&info.id);
                        }
                        frame_generation.fetch_add(1, Ordering::Release);
                        packet_generation.fetch_add(1, Ordering::Release);

                        let mut statistics_data = statistics_data.lock().await;
                        statistics_data.subscribers.remove(&info.id);
                        statistics_data.subscriber_count =
                            statistics_data.subscriber_count.saturating_sub(1);
                    }
                    TransceiverEvent::UnPublish {} => {
                        if let Err(err) = exit.send(()) {
                            tracing::error!("TransmitterEvent::UnPublish send error: {err}");
                        }
                        break;
                    }
                }
            }
        })
    }

    pub async fn run(
        self,
        event_sender: mpsc::Sender<TransceiverEvent>,
    ) -> Result<(), StreamHubError> {
        let (tx, _) = broadcast::channel::<()>(1);
        let mut tasks = JoinSet::new();

        if let Some(receiver) = self.data_receiver.frame_receiver {
            let handle = Self::receive_frame_data_loop(
                tx.subscribe(),
                receiver,
                self.id_to_frame_sender.clone(),
                Arc::clone(&self.frame_generation),
                Some(event_sender.clone()),
                self.statistic_data.clone(),
            );
            tasks.spawn(async move {
                handle
                    .await
                    .map_err(|error| map_task_join_error("frame loop", error))
            });
        }

        if let Some(receiver) = self.data_receiver.packet_receiver {
            let handle = Self::receive_packet_data_loop(
                tx.subscribe(),
                receiver,
                self.id_to_packet_sender.clone(),
                Arc::clone(&self.packet_generation),
                Some(event_sender.clone()),
                self.statistic_data.clone(),
            );
            tasks.spawn(async move {
                handle
                    .await
                    .map_err(|error| map_task_join_error("packet loop", error))
            });
        }

        let stats_handle = Self::receive_statistics_data_loop(
            tx.subscribe(),
            tx.subscribe(),
            self.statistic_data_receiver,
            self.statistic_data.clone(),
        );
        tasks.spawn(async move {
            stats_handle
                .await
                .map_err(|error| map_task_join_error("statistics loop", error))
        });

        let event_handle = Self::receive_event_loop(
            self.stream_handler,
            tx,
            self.event_receiver,
            self.id_to_packet_sender,
            self.id_to_frame_sender,
            self.frame_generation,
            self.packet_generation,
            self.statistic_data_sender,
            self.statistic_data.clone(),
        );
        let event_result = event_handle
            .await
            .map_err(|error| map_task_join_error("event loop", error));
        tasks.abort_all();

        let mut first_error = event_result.err();
        while let Some(join_result) = tasks.join_next().await {
            match join_result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
                Err(error) => {
                    if first_error.is_none() && !error.is_cancelled() {
                        first_error = Some(map_task_join_error("transceiver child task", error));
                    }
                }
            }
        }

        if let Some(error) = first_error {
            return Err(error);
        }

        Ok(())
    }

    #[must_use]
    pub fn get_statistics_data_sender(&self) -> StatisticDataSender {
        self.statistic_data_sender.clone()
    }
}

pub struct StreamsHub {
    //stream identifier to transceiver event sender
    streams: HashMap<StreamIdentifier, TransceiverEventSender>,
    //event is consumed in Stream hub, produced from other protocol sessions
    hub_event_receiver: StreamHubEventReceiver,
    //event is produced from other protocol sessions
    hub_event_sender: StreamHubEventSender,
    //broadcast publish/unpublish events to subscribers (HLS remuxer, publisher manager, etc.)
    client_event_sender: BroadcastEventSender,
}

impl StreamsHub {
    #[must_use]
    pub fn new(
        event_producer: StreamHubEventSender,
        event_consumer: StreamHubEventReceiver,
    ) -> Self {
        let (client_producer, _) = broadcast::channel(1000);

        Self {
            streams: HashMap::new(),
            hub_event_receiver: event_consumer,
            hub_event_sender: event_producer,
            client_event_sender: client_producer,
        }
    }
    /// Run the event loop, returning the exit reason.
    ///
    /// - `Ok(())` -- all event senders were dropped (normal shutdown).
    /// - `Err(msg)` -- the event loop panicked; `msg` describes the panic.
    ///
    /// The caller (supervision loop in `server.rs`) uses this to decide
    /// whether to restart with backoff or shut down.
    pub async fn run(&mut self) -> Result<(), String> {
        use futures::FutureExt;
        use std::panic::AssertUnwindSafe;

        match AssertUnwindSafe(self.event_loop()).catch_unwind().await {
            Ok(()) => {
                tracing::error!("StreamHub event_loop exited: all event senders dropped.");
                Ok(())
            }
            Err(panic_payload) => {
                let msg = panic_payload.downcast_ref::<&str>().map_or_else(
                    || {
                        if let Some(s) = panic_payload.downcast_ref::<String>() {
                            s.clone()
                        } else {
                            "unknown panic".to_string()
                        }
                    },
                    |s| (*s).to_string(),
                );
                tracing::error!(
                    "StreamHub event_loop panicked: {}. \
                     The streaming infrastructure is no longer functional.",
                    msg
                );
                Err(msg)
            }
        }
    }

    pub fn get_hub_event_sender(&mut self) -> StreamHubEventSender {
        self.hub_event_sender.clone()
    }

    pub fn get_client_event_consumer(&mut self) -> define::BroadcastEventReceiver {
        self.client_event_sender.subscribe()
    }

    pub async fn event_loop(&mut self) {
        while let Some(event) = self.hub_event_receiver.recv().await {
            match event {
                StreamHubEvent::Publish {
                    identifier,
                    info,
                    result_sender,
                    stream_handler,
                } => {
                    let (frame_sender, packet_sender, receiver) = match info.pub_data_type {
                        define::PubDataType::Frame => {
                            let (sender_chan, receiver_chan) =
                                mpsc::channel(define::FRAME_DATA_CHANNEL_CAPACITY);
                            (
                                Some(sender_chan),
                                None,
                                DataReceiver {
                                    frame_receiver: Some(receiver_chan),
                                    packet_receiver: None,
                                },
                            )
                        }
                        define::PubDataType::Packet => {
                            let (sender_chan, receiver_chan) =
                                mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);
                            (
                                None,
                                Some(sender_chan),
                                DataReceiver {
                                    frame_receiver: None,
                                    packet_receiver: Some(receiver_chan),
                                },
                            )
                        }
                        define::PubDataType::Both => {
                            let (sender_frame_chan, receiver_frame_chan) =
                                mpsc::channel(define::FRAME_DATA_CHANNEL_CAPACITY);
                            let (sender_packet_chan, receiver_packet_chan) =
                                mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);

                            (
                                Some(sender_frame_chan),
                                Some(sender_packet_chan),
                                DataReceiver {
                                    frame_receiver: Some(receiver_frame_chan),
                                    packet_receiver: Some(receiver_packet_chan),
                                },
                            )
                        }
                    };

                    let result = match self
                        .publish(identifier.clone(), info.pub_type, receiver, stream_handler)
                        .await
                    {
                        Ok(statistic_data_sender) => {
                            Ok((frame_sender, packet_sender, Some(statistic_data_sender)))
                        }
                        Err(err) => {
                            tracing::error!("event_loop Publish err: {err}");
                            Err(err)
                        }
                    };

                    if result_sender.send(result).is_err() {
                        tracing::error!("event_loop Subscribe error: The receiver dropped.");
                    }
                }

                StreamHubEvent::UnPublish { identifier } => {
                    if let Err(err) = self.unpublish(&identifier) {
                        tracing::error!(
                            "event_loop Unpublish err: {err} with identifier: {identifier}"
                        );
                    }
                }
                StreamHubEvent::Subscribe {
                    identifier,
                    info,
                    result_sender,
                } => {
                    let info_clone = info.clone();

                    //new chan for Frame/Packet sender and receiver
                    let (sender, receiver) = match info.sub_data_type {
                        define::SubDataType::Frame => {
                            let (sender_chan, receiver_chan) =
                                mpsc::channel(define::FRAME_DATA_CHANNEL_CAPACITY);
                            (
                                DataSender::Frame {
                                    sender: sender_chan,
                                },
                                DataReceiver {
                                    frame_receiver: Some(receiver_chan),
                                    packet_receiver: None,
                                },
                            )
                        }
                        define::SubDataType::Packet => {
                            let (sender_chan, receiver_chan) =
                                mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);
                            (
                                DataSender::Packet {
                                    sender: sender_chan,
                                },
                                DataReceiver {
                                    frame_receiver: None,
                                    packet_receiver: Some(receiver_chan),
                                },
                            )
                        }
                    };

                    let rv = match self.subscribe(&identifier, info_clone, sender).await {
                        Ok(statistic_data_sender) => Ok((receiver, Some(statistic_data_sender))),
                        Err(err) => {
                            tracing::error!("event_loop Subscribe error: {err}");
                            Err(err)
                        }
                    };

                    if result_sender.send(rv).is_err() {
                        tracing::error!("event_loop Subscribe error: The receiver dropped.");
                    }
                }
                StreamHubEvent::UnSubscribe { identifier, info } => {
                    let _ = self.unsubscribe(&identifier, info);
                }
            }
        }
    }

    //player subscribe a stream
    pub async fn subscribe(
        &mut self,
        identifer: &StreamIdentifier,
        sub_info: SubscriberInfo,
        sender: DataSender,
    ) -> Result<StatisticDataSender, StreamHubError> {
        if let Some(event_sender) = self.streams.get_mut(identifer) {
            let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
            let event = TransceiverEvent::Subscribe {
                sender,
                info: sub_info,
                result_sender,
            };
            tracing::info!("subscribe:  stream identifier: {identifer}");
            event_sender.send(event).await.map_err(|_| StreamHubError {
                value: StreamHubErrorValue::SendError,
            })?;

            return Ok(result_receiver.await?);
        }

        Err(StreamHubError {
            value: StreamHubErrorValue::NoAppOrStreamName,
        })
    }

    pub fn unsubscribe(
        &mut self,
        identifer: &StreamIdentifier,
        sub_info: SubscriberInfo,
    ) -> Result<(), StreamHubError> {
        if let Some(producer) = self.streams.get_mut(identifer) {
            tracing::info!("unsubscribe....:{identifer}");
            let event = TransceiverEvent::UnSubscribe { info: sub_info };
            producer.try_send(event).map_err(|_| StreamHubError {
                value: StreamHubErrorValue::SendError,
            })?;
        } else {
            tracing::info!("unsubscribe None....:{identifer}");
            return Err(StreamHubError {
                value: StreamHubErrorValue::NoAppName,
            });
        }

        Ok(())
    }

    //publish a stream
    pub async fn publish(
        &mut self,
        identifier: StreamIdentifier,
        pub_type: define::PublishType,
        receiver: DataReceiver,
        handler: Arc<dyn TStreamHandler>,
    ) -> Result<StatisticDataSender, StreamHubError> {
        // Use entry API for atomic check-and-insert
        let entry = match self.streams.entry(identifier.clone()) {
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(StreamHubError {
                    value: StreamHubErrorValue::Exists,
                });
            }
            std::collections::hash_map::Entry::Vacant(e) => e,
        };

        let (event_sender, event_receiver) =
            mpsc::channel(define::TRANSCEIVER_EVENT_CHANNEL_CAPACITY);
        let transceiver =
            StreamDataTransceiver::new(receiver, event_receiver, identifier.clone(), handler);

        let statistic_data_sender = transceiver.get_statistics_data_sender();
        let identifier_clone = identifier.clone();
        let event_sender_clone = event_sender.clone();
        let hub_sender_for_cleanup = self.hub_event_sender.clone();

        // H-1: Run transceiver with event_sender so spawned loops can send synthetic UnPublish.
        // Wraps the task so that if the transceiver panics or errors out, an UnPublish
        // event is sent to clean up the dead entry from the `streams` HashMap (LIVE-13).
        tokio::spawn({
            let hub_sender = hub_sender_for_cleanup;
            let identifier_for_cleanup = identifier_clone.clone();
            async move {
                use futures::FutureExt;
                use std::panic::AssertUnwindSafe;

                let result = AssertUnwindSafe(transceiver.run(event_sender_clone))
                    .catch_unwind()
                    .await;

                let needs_cleanup = match result {
                    Ok(Ok(())) => {
                        tracing::info!("transceiver run success, idetifier: {identifier_clone}");
                        false
                    }
                    Ok(Err(err)) => {
                        tracing::error!(
                            "transceiver run error, idetifier: {identifier_clone}, error: {err}",
                        );
                        true
                    }
                    Err(_panic) => {
                        tracing::error!(
                            "transceiver task panicked for {identifier_clone}. \
                             Sending UnPublish to prevent zombie stream entry."
                        );
                        true
                    }
                };

                if needs_cleanup {
                    if let Err(e) = hub_sender
                        .send(StreamHubEvent::UnPublish {
                            identifier: identifier_for_cleanup.clone(),
                        })
                        .await
                    {
                        tracing::error!(
                            "Failed to send cleanup UnPublish for {identifier_for_cleanup}: {e}"
                        );
                    }
                }
            }
        });

        entry.insert(event_sender);

        // Always broadcast publish event to listeners (HLS remuxer, publisher manager, etc.)
        let client_event = BroadcastEvent::Publish {
            identifier,
            pub_type,
        };
        if let Err(err) = self.client_event_sender.send(client_event) {
            tracing::debug!("broadcast Publish event: no receivers ({err})");
        }

        Ok(statistic_data_sender)
    }

    fn unpublish(&mut self, identifier: &StreamIdentifier) -> Result<(), StreamHubError> {
        match self.streams.remove(identifier) {
            Some(producer) => {
                let event = TransceiverEvent::UnPublish {};
                match producer.try_send(event) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(event)) => {
                        // Channel full: spawn a task to deliver with a timeout.
                        // Without this, the transceiver's data loops would keep running
                        // as zombie tasks until their data channels close naturally.
                        let id_str = format!("{identifier}");
                        tokio::spawn(async move {
                            match tokio::time::timeout(
                                std::time::Duration::from_secs(5),
                                producer.send(event),
                            )
                            .await
                            {
                                Ok(Ok(())) => {
                                    tracing::info!(
                                        "unpublish: delivered UnPublish after backpressure for {id_str}"
                                    );
                                }
                                Ok(Err(_)) => {
                                    tracing::warn!(
                                        "unpublish: channel closed for {id_str}; \
                                         transceiver will exit when all senders drop"
                                    );
                                }
                                Err(_) => {
                                    tracing::error!(
                                        "unpublish: timed out sending UnPublish for {id_str}; \
                                         dropping sender to force transceiver exit"
                                    );
                                    // Dropping `producer` reduces sender count, eventually
                                    // causing transceiver exit when data loops also finish.
                                }
                            }
                        });
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        tracing::warn!(
                            "unpublish: channel already closed for {identifier}; \
                             transceiver has already exited"
                        );
                    }
                }
                tracing::info!("unpublish remove stream, stream identifier: {identifier}");

                // Broadcast unpublish event to listeners
                let client_event = BroadcastEvent::UnPublish {
                    identifier: identifier.clone(),
                };
                if let Err(err) = self.client_event_sender.send(client_event) {
                    tracing::debug!("broadcast UnPublish event: no receivers ({err})");
                }
            }
            None => {
                return Err(StreamHubError {
                    value: StreamHubErrorValue::NoAppName,
                });
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::streamhub::define::{NotifyInfo, SubDataType, SubscribeType};
    use async_trait::async_trait;
    use std::time::Duration;
    use tokio::sync::oneshot;

    struct NoopHandler;

    #[async_trait]
    impl TStreamHandler for NoopHandler {
        async fn send_prior_data(
            &self,
            _sender: DataSender,
            _sub_type: SubscribeType,
        ) -> Result<(), StreamHubError> {
            Ok(())
        }
    }

    struct PanicOnPriorDataHandler;

    #[async_trait]
    impl TStreamHandler for PanicOnPriorDataHandler {
        async fn send_prior_data(
            &self,
            _sender: DataSender,
            _sub_type: SubscribeType,
        ) -> Result<(), StreamHubError> {
            panic!("intentional streamhub event loop panic");
        }
    }

    fn test_identifier() -> StreamIdentifier {
        StreamIdentifier::Rtmp {
            app_name: "live".to_string(),
            stream_name: "panic-test".to_string(),
        }
    }

    fn test_subscriber() -> SubscriberInfo {
        SubscriberInfo {
            id: Uuid::new(),
            sub_type: SubscribeType::RtmpPull,
            notify_info: NotifyInfo {
                request_url: "http://localhost/test".to_string(),
                remote_addr: "127.0.0.1:12345".to_string(),
            },
            sub_data_type: SubDataType::Frame,
        }
    }

    #[tokio::test]
    async fn test_receive_event_loop_exits_when_event_channel_closes() {
        let (exit_tx, _) = broadcast::channel(1);
        let (event_tx, event_rx) = mpsc::channel(1);
        let (stat_tx, _stat_rx) = mpsc::channel(1);

        let handle = StreamDataTransceiver::receive_event_loop(
            Arc::new(NoopHandler),
            exit_tx,
            event_rx,
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(AtomicU64::new(0)),
            Arc::new(AtomicU64::new(0)),
            stat_tx,
            Arc::new(Mutex::new(StatisticsStream::new(test_identifier()))),
        );

        drop(event_tx);

        let join_result = tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .expect("event loop should exit promptly when channel closes");

        assert!(join_result.is_ok(), "event loop task should not panic");
    }

    #[tokio::test]
    async fn test_transceiver_run_propagates_event_loop_panic() {
        let (event_tx, event_rx) = mpsc::channel(1);
        let transceiver = StreamDataTransceiver::new(
            DataReceiver {
                frame_receiver: None,
                packet_receiver: None,
            },
            event_rx,
            test_identifier(),
            Arc::new(PanicOnPriorDataHandler),
        );

        let run_handle = tokio::spawn(transceiver.run(event_tx.clone()));

        let (result_sender, _result_receiver) = oneshot::channel();
        let (frame_sender, _frame_receiver) = mpsc::channel(1);
        event_tx
            .send(TransceiverEvent::Subscribe {
                sender: DataSender::Frame {
                    sender: frame_sender,
                },
                info: test_subscriber(),
                result_sender,
            })
            .await
            .expect("subscribe event should be delivered");
        drop(event_tx);

        let run_result = tokio::time::timeout(Duration::from_secs(1), run_handle)
            .await
            .expect("transceiver should stop after event loop panic")
            .expect("run task should not panic");

        let err = run_result.expect_err("event loop panic must be propagated");
        assert!(
            err.to_string().contains("event loop"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn test_send_event_with_backpressure_timeout_retries_when_temporarily_full() {
        let (sender, mut receiver) = mpsc::channel(1);
        sender
            .try_send(StreamHubEvent::UnPublish {
                identifier: test_identifier(),
            })
            .expect("prefill event channel");

        let send_task = tokio::spawn(async move {
            send_event_with_backpressure_timeout(
                &sender,
                StreamHubEvent::UnSubscribe {
                    identifier: test_identifier(),
                    info: test_subscriber(),
                },
            )
            .await
        });

        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(
            !send_task.is_finished(),
            "send helper should wait for temporary backpressure"
        );

        let first = receiver
            .recv()
            .await
            .expect("blocked event should remain queued");
        assert!(matches!(first, StreamHubEvent::UnPublish { .. }));

        let second = tokio::time::timeout(Duration::from_secs(1), receiver.recv())
            .await
            .expect("unsubscribe should be delivered after queue drains")
            .expect("event channel should stay open");
        assert!(matches!(second, StreamHubEvent::UnSubscribe { .. }));

        let result = send_task.await.expect("send task should join");
        assert!(result.is_ok(), "send helper should eventually succeed");
    }

    #[tokio::test]
    async fn test_send_event_with_backpressure_timeout_errors_when_closed() {
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);

        let err = send_event_with_backpressure_timeout(
            &sender,
            StreamHubEvent::UnPublish {
                identifier: test_identifier(),
            },
        )
        .await
        .expect_err("closed channel should surface send error");

        assert!(matches!(err.value, StreamHubErrorValue::SendError));
    }
}
