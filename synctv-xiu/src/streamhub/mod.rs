use define::{
    FrameDataReceiver, PacketDataReceiver, PacketDataSender, StatisticData,
    StatisticDataReceiver, StatisticDataSender,
};
use crate::flv::define::aac_packet_type;

use define::PacketData;

pub mod define;
pub mod errors;
pub mod statistics;
pub mod stream;
pub mod utils;

use {
    define::{
        BroadcastEvent, BroadcastEventSender, DataReceiver, DataSender,
        FrameData, FrameDataSender, StreamHubEvent, StreamHubEventReceiver,
        StreamHubEventSender, SubscriberInfo, TStreamHandler, TransceiverEvent,
        TransceiverEventReceiver, TransceiverEventSender,
    },
    errors::{StreamHubError, StreamHubErrorValue},
    std::collections::HashMap,
    std::sync::Arc,
    std::sync::atomic::{AtomicU64, Ordering},
    stream::StreamIdentifier,
    tokio::sync::{broadcast, mpsc, Mutex},
    tokio::task::JoinSet,
    utils::Uuid,
};

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
        let (statistic_data_sender, statistic_data_receiver) = mpsc::channel(define::STATISTIC_DATA_CHANNEL_CAPACITY);
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
        data: FrameData,
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
                            id, prev + 1
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
        data: PacketData,
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
                            id, prev + 1
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
                *cached_gen = current_gen;
            }

            if cached_snapshot.is_empty() {
                return;
            }

            // Fan out to all subscribers without holding any lock.
            // FrameData uses Bytes internally so clone is O(1) Arc reference bump.
            let closed_ids = Self::fan_out_frame(cached_snapshot, val);

            // Remove closed subscribers and bump generation
            if !closed_ids.is_empty() {
                let mut guard = frame_senders.lock().await;
                for id in &closed_ids {
                    guard.remove(id);
                    tracing::debug!("Removed closed frame subscriber: {}", id);
                }
                // Bump generation so next call rebuilds snapshot
                generation.fetch_add(1, Ordering::Release);
                // Invalidate cached snapshot immediately
                *cached_gen = cached_gen.wrapping_add(u64::MAX); // Force mismatch
            }
        }
    }

    fn receive_frame_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: FrameDataReceiver,
        frame_senders: Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
        generation: Arc<AtomicU64>,
        event_sender: Option<mpsc::Sender<TransceiverEvent>>,
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
    ) {
        if let Some(val) = data {
            let current_gen = generation.load(Ordering::Acquire);
            if current_gen != *cached_gen {
                let guard = packet_senders.lock().await;
                *cached_snapshot = guard
                    .iter()
                    .map(|(id, sc)| (*id, sc.sender.clone(), Arc::clone(&sc.drop_count)))
                    .collect();
                *cached_gen = current_gen;
            }

            if cached_snapshot.is_empty() {
                return;
            }

            let closed_ids = Self::fan_out_packet(cached_snapshot, val);

            if !closed_ids.is_empty() {
                let mut guard = packet_senders.lock().await;
                for id in &closed_ids {
                    guard.remove(id);
                    tracing::debug!("Removed closed packet subscriber: {}", id);
                }
                generation.fetch_add(1, Ordering::Release);
                *cached_gen = cached_gen.wrapping_add(u64::MAX);
            }
        }
    }

    fn receive_packet_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: PacketDataReceiver,
        packet_senders: Arc<Mutex<HashMap<Uuid, PacketSubscriberDropCounter>>>,
        generation: Arc<AtomicU64>,
        event_sender: Option<mpsc::Sender<TransceiverEvent>>,
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
                        match aac_packet_type {
                            aac_packet_type::AAC_RAW => {
                                guard.publisher.audio.recv_bytes += data_size;
                            }
                            aac_packet_type::AAC_SEQHDR => {}
                            _ => {}
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
                if let Some(val) = receiver.recv().await {
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
                                    frame_senders.lock().await.insert(info.id, SubscriberDropCounter {
                                        sender: frame_sender,
                                        drop_count: Arc::new(AtomicU64::new(0)),
                                    });
                                    // Bump generation so fan-out loop rebuilds snapshot
                                    frame_generation.fetch_add(1, Ordering::Release);
                                }
                                DataSender::Packet {
                                    sender: packet_sender,
                                } => {
                                    packet_senders.lock().await.insert(info.id, PacketSubscriberDropCounter {
                                        sender: packet_sender,
                                        drop_count: Arc::new(AtomicU64::new(0)),
                                    });
                                    packet_generation.fetch_add(1, Ordering::Release);
                                }
                            }

                            if let Err(err) = result_sender.send(statistic_sender.clone()) {
                                tracing::error!(
                                    "receive_event_loop:send statistic send err :{err:?} "
                                );
                            }

                            let mut statistics_data = statistics_data.lock().await;
                            statistics_data.subscriber_count += 1;
                        }
                        TransceiverEvent::UnSubscribe { info } => {
                            // Remove from both sender maps and update statistics
                            // in a single logical block to minimize lock hold times.
                            {
                                let mut fs = frame_senders.lock().await;
                                let mut ps = packet_senders.lock().await;
                                fs.remove(&info.id);
                                ps.remove(&info.id);
                            }
                            frame_generation.fetch_add(1, Ordering::Release);
                            packet_generation.fetch_add(1, Ordering::Release);

                            let mut statistics_data = statistics_data.lock().await;
                            statistics_data.subscribers.remove(&info.id);
                            statistics_data.subscriber_count = statistics_data.subscriber_count.saturating_sub(1);
                        }
                        TransceiverEvent::UnPublish {} => {
                            if let Err(err) = exit.send(()) {
                                tracing::error!("TransmitterEvent::UnPublish send error: {err}");
                            }
                            break;
                        }
                    }
                }
            }
        })
    }

    pub async fn run(self, event_sender: mpsc::Sender<TransceiverEvent>) -> Result<(), StreamHubError> {
        let (tx, _) = broadcast::channel::<()>(1);
        let mut tasks = JoinSet::new();

        if let Some(receiver) = self.data_receiver.frame_receiver {
            let handle = Self::receive_frame_data_loop(
                tx.subscribe(),
                receiver,
                self.id_to_frame_sender.clone(),
                Arc::clone(&self.frame_generation),
                Some(event_sender.clone()),
            );
            tasks.spawn(async move { handle.await.ok(); });
        }

        if let Some(receiver) = self.data_receiver.packet_receiver {
            let handle = Self::receive_packet_data_loop(
                tx.subscribe(),
                receiver,
                self.id_to_packet_sender.clone(),
                Arc::clone(&self.packet_generation),
                Some(event_sender.clone()),
            );
            tasks.spawn(async move { handle.await.ok(); });
        }

        let stats_handle = Self::receive_statistics_data_loop(
            tx.subscribe(),
            tx.subscribe(),
            self.statistic_data_receiver,
            self.statistic_data.clone(),
        );
        tasks.spawn(async move { stats_handle.await.ok(); });

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
        // Wait for the event loop to finish (triggered by UnPublish),
        // then abort remaining tasks.
        let _ = event_handle.await;
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}

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
        use std::panic::AssertUnwindSafe;
        use futures::FutureExt;

        match AssertUnwindSafe(self.event_loop()).catch_unwind().await {
            Ok(()) => {
                tracing::error!(
                    "StreamHub event_loop exited: all event senders dropped."
                );
                Ok(())
            }
            Err(panic_payload) => {
                let msg = if let Some(s) = panic_payload.downcast_ref::<&str>() {
                    (*s).to_string()
                } else if let Some(s) = panic_payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "unknown panic".to_string()
                };
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
                            let (sender_chan, receiver_chan) = mpsc::channel(define::FRAME_DATA_CHANNEL_CAPACITY);
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
                            let (sender_chan, receiver_chan) = mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);
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

                StreamHubEvent::UnPublish {
                    identifier,
                } => {
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
                            let (sender_chan, receiver_chan) = mpsc::channel(define::FRAME_DATA_CHANNEL_CAPACITY);
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
                            let (sender_chan, receiver_chan) = mpsc::channel(define::PACKET_DATA_CHANNEL_CAPACITY);
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
                        Ok(statistic_data_sender) => {
                            Ok((receiver, Some(statistic_data_sender)))
                        }
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

        let (event_sender, event_receiver) = mpsc::channel(define::TRANSCEIVER_EVENT_CHANNEL_CAPACITY);
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
                use std::panic::AssertUnwindSafe;
                use futures::FutureExt;

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
                    if let Err(e) = hub_sender.send(StreamHubEvent::UnPublish {
                        identifier: identifier_for_cleanup.clone(),
                    }).await {
                        tracing::error!(
                            "Failed to send cleanup UnPublish for {identifier_for_cleanup}: {e}"
                        );
                    }
                }
            }
        });

        entry.insert(event_sender);

        // Always broadcast publish event to listeners (HLS remuxer, publisher manager, etc.)
        let client_event = BroadcastEvent::Publish { identifier, pub_type };
        if let Err(err) = self.client_event_sender.send(client_event) {
            tracing::debug!("broadcast Publish event: no receivers ({err})");
        }

        Ok(statistic_data_sender)
    }

    fn unpublish(&mut self, identifier: &StreamIdentifier) -> Result<(), StreamHubError> {
        match self.streams.remove(identifier) {
            Some(producer) => {
                let event = TransceiverEvent::UnPublish {};
                if let Err(e) = producer.try_send(event) {
                    // Channel full or closed. Since we already removed the sender
                    // from `streams`, the transceiver holds the only remaining
                    // sender clone (via the data loops). Dropping `producer` here
                    // reduces the sender count. When the data loops also finish,
                    // all senders are dropped, causing `event_receiver.recv()` in
                    // the transceiver to return `None`, which triggers exit.
                    // This prevents zombie transceivers when try_send fails.
                    tracing::warn!(
                        "unpublish try_send failed for {identifier}: {e}. \
                         Streams entry removed; transceiver will exit when all senders drop."
                    );
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
