use super::{
    define::{
        DataReceiver, DataSender, FrameData, FrameDataReceiver, FrameDataSender, FrameTrySendError,
        PacketData, PacketDataReceiver, PacketDataSender, StatisticData, StatisticDataReceiver,
        StatisticDataSender, TStreamHandler, TransceiverEvent, TransceiverEventReceiver,
    },
    errors::{StreamHubError, StreamHubErrorValue},
    statistics::{self, StatisticsStream},
    stream::StreamIdentifier,
    utils::Uuid,
};
use crate::flv::define::aac_packet_type;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::task::JoinSet;

fn map_task_join_error(task_name: &str, error: &tokio::task::JoinError) -> StreamHubError {
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
pub(crate) struct SubscriberDropCounter {
    pub(crate) sender: FrameDataSender,
    pub(crate) drop_count: Arc<AtomicU64>,
}

/// Tracks per-subscriber packet drop counts for diagnostics.
pub(crate) struct PacketSubscriberDropCounter {
    sender: PacketDataSender,
    drop_count: Arc<AtomicU64>,
}

/// How often to log per-subscriber drop warnings (every N drops).
const DROP_LOG_INTERVAL: u64 = 100;

fn request_synthetic_unpublish(event_sender: Option<&mpsc::UnboundedSender<TransceiverEvent>>) {
    let Some(sender) = event_sender else {
        return;
    };

    if let Err(error) = sender.send(TransceiverEvent::UnPublish {}) {
        tracing::error!("Failed to send synthetic UnPublish (channel closed): {error}");
    }
}

// Receive audio/video/media info from a publisher and send to subscribers,
// while also aggregating stream statistics.
pub(crate) struct StreamDataTransceiver {
    data_receiver: DataReceiver,
    event_receiver: TransceiverEventReceiver,
    id_to_frame_sender: Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
    id_to_packet_sender: Arc<Mutex<HashMap<Uuid, PacketSubscriberDropCounter>>>,
    frame_generation: Arc<AtomicU64>,
    packet_generation: Arc<AtomicU64>,
    statistic_data_sender: StatisticDataSender,
    statistic_data_receiver: StatisticDataReceiver,
    statistic_data: Arc<Mutex<StatisticsStream>>,
    stream_handler: Arc<dyn TStreamHandler>,
}

impl StreamDataTransceiver {
    pub(crate) fn new(
        data_receiver: DataReceiver,
        event_receiver: TransceiverEventReceiver,
        identifier: StreamIdentifier,
        handler: Arc<dyn TStreamHandler>,
    ) -> Self {
        let (statistic_data_sender, statistic_data_receiver) = mpsc::unbounded_channel();
        Self {
            data_receiver,
            event_receiver,
            statistic_data_sender,
            statistic_data_receiver,
            id_to_frame_sender: Arc::new(Mutex::new(HashMap::new())),
            id_to_packet_sender: Arc::new(Mutex::new(HashMap::new())),
            frame_generation: Arc::new(AtomicU64::new(0)),
            packet_generation: Arc::new(AtomicU64::new(0)),
            stream_handler: handler,
            statistic_data: Arc::new(Mutex::new(StatisticsStream::new(identifier))),
        }
    }

    fn fan_out_frame(
        snapshot: &[(Uuid, FrameDataSender, Arc<AtomicU64>)],
        data: &FrameData,
    ) -> Vec<Uuid> {
        let mut closed_ids = Vec::new();
        for (id, sender, drop_count) in snapshot {
            match sender.try_send(data.clone()) {
                Ok(()) => {}
                Err(FrameTrySendError::Full(_)) => {
                    let prev = drop_count.fetch_add(1, Ordering::Relaxed);
                    if (prev + 1) % DROP_LOG_INTERVAL == 0 {
                        tracing::warn!(
                            "Subscriber {} dropped {} frames due to backpressure",
                            id,
                            prev + 1
                        );
                    }
                }
                Err(FrameTrySendError::Closed(_)) => {
                    closed_ids.push(*id);
                }
            }
        }
        closed_ids
    }

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

    pub(crate) async fn receive_frame_data(
        data: Option<FrameData>,
        frame_senders: &Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
        generation: &Arc<AtomicU64>,
        cached_snapshot: &mut Vec<(Uuid, FrameDataSender, Arc<AtomicU64>)>,
        cached_gen: &mut u64,
        statistics_data: &Arc<Mutex<StatisticsStream>>,
    ) {
        if let Some(val) = data {
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

            let closed_ids = Self::fan_out_frame(cached_snapshot, &val);

            if !closed_ids.is_empty() {
                let closed_count = closed_ids.len();
                for id in &closed_ids {
                    frame_senders.lock().await.remove(id);
                    tracing::debug!("Removed closed frame subscriber: {}", id);
                }
                generation.fetch_add(1, Ordering::Release);
                *cached_gen = cached_gen.wrapping_add(u64::MAX);

                let mut stats = statistics_data.lock().await;
                for id in &closed_ids {
                    stats.subscribers.remove(id);
                }
                stats.subscriber_count = stats.subscriber_count.saturating_sub(closed_count);
            }
        }
    }

    fn receive_frame_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: FrameDataReceiver,
        frame_senders: Arc<Mutex<HashMap<Uuid, SubscriberDropCounter>>>,
        generation: Arc<AtomicU64>,
        event_sender: Option<mpsc::UnboundedSender<TransceiverEvent>>,
        statistics_data: Arc<Mutex<StatisticsStream>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut cached_snapshot: Vec<(Uuid, FrameDataSender, Arc<AtomicU64>)> = Vec::new();
            let mut cached_gen: u64 = u64::MAX;

            loop {
                tokio::select! {
                    data = receiver.recv() => {
                        if data.is_none() {
                            tracing::warn!("Frame data receiver closed (publisher dropped)");
                            request_synthetic_unpublish(event_sender.as_ref());
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
                    _ = exit.recv() => {
                        break;
                    }
                }
            }
        })
    }

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

                let mut stats = statistics_data.lock().await;
                for id in &closed_ids {
                    stats.subscribers.remove(id);
                }
                stats.subscriber_count = stats.subscriber_count.saturating_sub(closed_count);
            }
        }
    }

    fn receive_packet_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: PacketDataReceiver,
        packet_senders: Arc<Mutex<HashMap<Uuid, PacketSubscriberDropCounter>>>,
        generation: Arc<AtomicU64>,
        event_sender: Option<mpsc::UnboundedSender<TransceiverEvent>>,
        statistics_data: Arc<Mutex<StatisticsStream>>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut cached_snapshot: Vec<(Uuid, PacketDataSender, Arc<AtomicU64>)> = Vec::new();
            let mut cached_gen: u64 = u64::MAX;

            loop {
                tokio::select! {
                    data = receiver.recv() => {
                        if data.is_none() {
                            tracing::warn!("Packet data receiver closed (publisher dropped)");
                            request_synthetic_unpublish(event_sender.as_ref());
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
                    _ = exit.recv() => {
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
                    data = receiver.recv() => {
                        if data.is_none() {
                            break;
                        }
                        Self::receive_statistics_data(data, &statistics_data).await;
                    }
                    _ = exit_receive.recv() => {
                        break;
                    }
                }
            }
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn receive_event_loop(
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
                            tracing::warn!(
                                "receive_event_loop send_prior_data err (skipping subscriber): {err}"
                            );
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
                        {
                            frame_senders.lock().await.remove(&info.id);
                            let mut packet_senders = packet_senders.lock().await;
                            packet_senders.remove(&info.id);
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

    pub(crate) async fn run(
        self,
        event_sender: mpsc::UnboundedSender<TransceiverEvent>,
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
                    .map_err(|error| map_task_join_error("frame loop", &error))
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
                    .map_err(|error| map_task_join_error("packet loop", &error))
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
                .map_err(|error| map_task_join_error("statistics loop", &error))
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
            .map_err(|error| map_task_join_error("event loop", &error));
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
                        first_error = Some(map_task_join_error("transceiver child task", &error));
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
    pub(crate) fn get_statistics_data_sender(&self) -> StatisticDataSender {
        self.statistic_data_sender.clone()
    }
}
