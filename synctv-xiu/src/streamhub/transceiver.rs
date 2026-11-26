use super::{
    define::{
        DataReceiver, DataSender, FrameData, FrameDataReceiver, FrameDataSender, FrameTrySendError,
        PacketData, PacketDataReceiver, PacketDataSender, PublisherActivityCallback,
        StatisticDataReceiver, StatisticDataSender, TStreamHandler, TransceiverEvent,
        TransceiverEventExecuteResultSender, TransceiverEventReceiver,
    },
    errors::{StreamHubError, StreamHubErrorValue},
    statistics::{self, StatisticsStream},
    stream::StreamIdentifier,
    utils::Uuid,
};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::task::JoinSet;
use tokio::time::Instant;

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

/// The counter is shared with fan-out snapshots and identifies this registration.
pub(crate) struct SubscriberDropCounter<S> {
    pub(crate) sender: S,
    pub(crate) drop_count: Arc<AtomicU64>,
}

type SubscriberMap<S> = Arc<Mutex<HashMap<Uuid, SubscriberDropCounter<S>>>>;

pub(crate) struct ReceiveEventLoopContext {
    pub(crate) stream_handler: Arc<dyn TStreamHandler>,
    pub(crate) exit: broadcast::Sender<()>,
    pub(crate) receiver: TransceiverEventReceiver,
    pub(crate) packet_senders: SubscriberMap<PacketDataSender>,
    pub(crate) frame_senders: SubscriberMap<FrameDataSender>,
    pub(crate) frame_generation: Arc<AtomicU64>,
    pub(crate) packet_generation: Arc<AtomicU64>,
    pub(crate) statistic_sender: StatisticDataSender,
    pub(crate) statistics_data: Arc<Mutex<StatisticsStream>>,
}

pub(crate) struct FrameDataLoopContext {
    pub(crate) stream_handler: Arc<dyn TStreamHandler>,
    pub(crate) frame_senders: SubscriberMap<FrameDataSender>,
    pub(crate) generation: Arc<AtomicU64>,
    pub(crate) statistics_data: Arc<Mutex<StatisticsStream>>,
    pub(crate) publisher_activity: Option<Arc<PublisherActivityTracker>>,
}

/// How often to log per-subscriber drop warnings (every N drops).
const DROP_LOG_INTERVAL: u64 = 100;

fn request_synthetic_unpublish(event_sender: Option<&mpsc::UnboundedSender<TransceiverEvent>>) {
    let Some(sender) = event_sender else {
        return;
    };

    if let Err(error) = sender.send(TransceiverEvent::UnPublish {}) {
        tracing::error!(
            "synthetic unpublish request failed because the event channel closed: {error}"
        );
    }
}

fn send_subscribe_result(
    result_sender: TransceiverEventExecuteResultSender,
    result: Result<StatisticDataSender, StreamHubError>,
) {
    if result_sender.send(result).is_err() {
        tracing::debug!("subscriber dropped before subscribe result was delivered");
    }
}

async fn evict_closed_subscribers<S>(
    senders: &Mutex<HashMap<Uuid, SubscriberDropCounter<S>>>,
    generation: &AtomicU64,
    statistics: &Mutex<StatisticsStream>,
    closed: Vec<(Uuid, Arc<AtomicU64>)>,
) {
    if closed.is_empty() {
        return;
    }
    // All membership changes take the statistics lock before changing routing.
    let mut stats = statistics.lock().await;
    let mut senders = senders.lock().await;
    for (id, registration) in closed {
        if let std::collections::hash_map::Entry::Occupied(entry) = senders.entry(id)
            && Arc::ptr_eq(&entry.get().drop_count, &registration)
        {
            entry.remove();
            stats.remove_subscriber(&id);
            generation.fetch_add(1, Ordering::Release);
        }
    }
}

/// Fans publisher data out to subscribers and maintains stream statistics.
pub(crate) struct StreamDataTransceiver {
    data_receiver: DataReceiver,
    event_receiver: TransceiverEventReceiver,
    id_to_frame_sender: SubscriberMap<FrameDataSender>,
    id_to_packet_sender: SubscriberMap<PacketDataSender>,
    frame_generation: Arc<AtomicU64>,
    packet_generation: Arc<AtomicU64>,
    statistic_data_sender: StatisticDataSender,
    statistic_data_receiver: StatisticDataReceiver,
    statistic_data: Arc<Mutex<StatisticsStream>>,
    stream_handler: Arc<dyn TStreamHandler>,
    identifier: StreamIdentifier,
    publisher_activity: Option<Arc<PublisherActivityTracker>>,
}

pub(crate) const PUBLISHER_ACTIVITY_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(10);

pub(crate) struct PublisherActivityTracker {
    app_name: String,
    stream_name: String,
    generation_id: Uuid,
    callback: PublisherActivityCallback,
    last_recorded: parking_lot::Mutex<Option<Instant>>,
}

impl PublisherActivityTracker {
    fn new(
        identifier: &StreamIdentifier,
        generation_id: Uuid,
        callback: PublisherActivityCallback,
    ) -> Self {
        let StreamIdentifier::Rtmp {
            app_name,
            stream_name,
        } = identifier;
        Self {
            app_name: app_name.clone(),
            stream_name: stream_name.clone(),
            generation_id,
            callback,
            last_recorded: parking_lot::Mutex::new(None),
        }
    }

    fn record(&self) {
        let now = Instant::now();
        let mut last_recorded = self.last_recorded.lock();
        if last_recorded.is_some_and(|last| now.duration_since(last) < PUBLISHER_ACTIVITY_INTERVAL)
        {
            return;
        }
        *last_recorded = Some(now);
        drop(last_recorded);
        (self.callback)(&self.app_name, &self.stream_name, self.generation_id);
    }
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
            statistic_data: Arc::new(Mutex::new(StatisticsStream::new(identifier.clone()))),
            identifier,
            publisher_activity: None,
        }
    }

    pub(crate) fn with_publisher_activity_callback(
        mut self,
        generation_id: Uuid,
        callback: PublisherActivityCallback,
    ) -> Self {
        self.publisher_activity = Some(Arc::new(PublisherActivityTracker::new(
            &self.identifier,
            generation_id,
            callback,
        )));
        self
    }

    fn fan_out_frame(
        snapshot: &[(Uuid, FrameDataSender, Arc<AtomicU64>)],
        data: &FrameData,
    ) -> Vec<(Uuid, Arc<AtomicU64>)> {
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
                    closed_ids.push((*id, Arc::clone(drop_count)));
                }
            }
        }
        closed_ids
    }

    fn fan_out_packet(
        snapshot: &[(Uuid, PacketDataSender, Arc<AtomicU64>)],
        data: &PacketData,
    ) -> Vec<(Uuid, Arc<AtomicU64>)> {
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
                    closed_ids.push((*id, Arc::clone(drop_count)));
                }
            }
        }
        closed_ids
    }

    pub(crate) async fn receive_frame_data(
        data: Option<FrameData>,
        context: &FrameDataLoopContext,
        cached_snapshot: &mut Vec<(Uuid, FrameDataSender, Arc<AtomicU64>)>,
        cached_gen: &mut u64,
    ) -> Result<(), StreamHubError> {
        if let Some(val) = data {
            context.stream_handler.save_frame_data(&val)?;
            if matches!(val, FrameData::Audio { .. } | FrameData::Video { .. })
                && let Some(activity) = &context.publisher_activity
            {
                activity.record();
            }
            let current_gen = context.generation.load(Ordering::Acquire);
            if current_gen != *cached_gen {
                let guard = context.frame_senders.lock().await;
                *cached_snapshot = guard
                    .iter()
                    .map(|(id, sc)| (*id, sc.sender.clone(), Arc::clone(&sc.drop_count)))
                    .collect();
                drop(guard);
                *cached_gen = current_gen;
            }

            if cached_snapshot.is_empty() {
                return Ok(());
            }

            let closed_ids = Self::fan_out_frame(cached_snapshot, &val);

            evict_closed_subscribers(
                &context.frame_senders,
                &context.generation,
                &context.statistics_data,
                closed_ids,
            )
            .await;
        }
        Ok(())
    }

    fn receive_frame_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: FrameDataReceiver,
        context: FrameDataLoopContext,
        event_sender: Option<mpsc::UnboundedSender<TransceiverEvent>>,
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
                        if let Err(error) = Self::receive_frame_data(
                            data,
                            &context,
                            &mut cached_snapshot,
                            &mut cached_gen,
                        ).await {
                            tracing::warn!(%error, "publisher frame rejected by stream handler");
                            request_synthetic_unpublish(event_sender.as_ref());
                            break;
                        }
                    }
                    _ = exit.recv() => {
                        while let Ok(data) = receiver.try_recv() {
                            if let Err(error) = Self::receive_frame_data(
                                Some(data),
                                &context,
                                &mut cached_snapshot,
                                &mut cached_gen,
                            ).await {
                                tracing::warn!(%error, "buffered publisher frame rejected during shutdown");
                                break;
                            }
                        }
                        break;
                    }
                }
            }
        })
    }

    async fn receive_packet_data(
        data: Option<PacketData>,
        packet_senders: &SubscriberMap<PacketDataSender>,
        generation: &Arc<AtomicU64>,
        cached_snapshot: &mut Vec<(Uuid, PacketDataSender, Arc<AtomicU64>)>,
        cached_gen: &mut u64,
        statistics_data: &Arc<Mutex<StatisticsStream>>,
        publisher_activity: Option<&PublisherActivityTracker>,
    ) {
        if let Some(val) = data {
            if let Some(activity) = publisher_activity {
                activity.record();
            }
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

            evict_closed_subscribers(packet_senders, generation, statistics_data, closed_ids).await;
        }
    }

    fn receive_packet_data_loop(
        mut exit: broadcast::Receiver<()>,
        mut receiver: PacketDataReceiver,
        packet_senders: SubscriberMap<PacketDataSender>,
        generation: Arc<AtomicU64>,
        event_sender: Option<mpsc::UnboundedSender<TransceiverEvent>>,
        statistics_data: Arc<Mutex<StatisticsStream>>,
        publisher_activity: Option<Arc<PublisherActivityTracker>>,
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
                            publisher_activity.as_deref(),
                        ).await;
                    }
                    _ = exit.recv() => {
                        while let Ok(data) = receiver.try_recv() {
                            Self::receive_packet_data(
                                Some(data),
                                &packet_senders,
                                &generation,
                                &mut cached_snapshot,
                                &mut cached_gen,
                                &statistics_data,
                                publisher_activity.as_deref(),
                            ).await;
                        }
                        break;
                    }
                }
            }
        })
    }

    fn receive_statistics_data_loop(
        exit: broadcast::Receiver<()>,
        mut receiver: StatisticDataReceiver,
        statistics: Arc<Mutex<StatisticsStream>>,
    ) -> tokio::task::JoinHandle<()> {
        const BATCH_SIZE: usize = 64;
        tokio::spawn(async move {
            let mut calculator =
                statistics::StatisticsCalculate::new(Arc::clone(&statistics), exit);
            let collect = async {
                let mut batch = Vec::new();
                while receiver.recv_many(&mut batch, BATCH_SIZE).await > 0 {
                    let mut stats = statistics.lock().await;
                    for data in batch.drain(..) {
                        stats.record(data);
                    }
                }
            };
            tokio::select! {
                () = collect => {}
                () = calculator.start() => {}
            }
        })
    }

    pub(crate) fn receive_event_loop(
        context: ReceiveEventLoopContext,
    ) -> tokio::task::JoinHandle<()> {
        let ReceiveEventLoopContext {
            stream_handler,
            exit,
            mut receiver,
            packet_senders,
            frame_senders,
            frame_generation,
            packet_generation,
            statistic_sender,
            statistics_data,
        } = context;

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
                            .send_prior_data(sender.clone(), info.sub_type.clone())
                            .await
                        {
                            tracing::warn!(
                                "subscriber prior-data delivery failed; skipping subscriber: {err}"
                            );
                            send_subscribe_result(result_sender, Err(err));
                            continue;
                        }
                        let mut stats = statistics_data.lock().await;
                        match sender {
                            DataSender::Frame { sender } => {
                                if packet_senders.lock().await.remove(&info.id).is_some() {
                                    packet_generation.fetch_add(1, Ordering::Release);
                                }
                                frame_senders.lock().await.insert(
                                    info.id,
                                    SubscriberDropCounter {
                                        sender,
                                        drop_count: Arc::new(AtomicU64::new(0)),
                                    },
                                );
                                frame_generation.fetch_add(1, Ordering::Release);
                            }
                            DataSender::Packet { sender } => {
                                if frame_senders.lock().await.remove(&info.id).is_some() {
                                    frame_generation.fetch_add(1, Ordering::Release);
                                }
                                packet_senders.lock().await.insert(
                                    info.id,
                                    SubscriberDropCounter {
                                        sender,
                                        drop_count: Arc::new(AtomicU64::new(0)),
                                    },
                                );
                                packet_generation.fetch_add(1, Ordering::Release);
                            }
                        }
                        stats.register_subscriber(info);
                        drop(stats);
                        send_subscribe_result(result_sender, Ok(statistic_sender.clone()));
                    }
                    TransceiverEvent::UnSubscribe { info } => {
                        let mut stats = statistics_data.lock().await;
                        if frame_senders.lock().await.remove(&info.id).is_some() {
                            frame_generation.fetch_add(1, Ordering::Release);
                        }
                        if packet_senders.lock().await.remove(&info.id).is_some() {
                            packet_generation.fetch_add(1, Ordering::Release);
                        }
                        stats.remove_subscriber(&info.id);
                    }
                    TransceiverEvent::UnPublish {} => {
                        if let Err(err) = exit.send(()) {
                            tracing::debug!(
                                "transceiver shutdown broadcast had no receivers: {err}"
                            );
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
            let context = FrameDataLoopContext {
                stream_handler: Arc::clone(&self.stream_handler),
                frame_senders: Arc::clone(&self.id_to_frame_sender),
                generation: Arc::clone(&self.frame_generation),
                statistics_data: Arc::clone(&self.statistic_data),
                publisher_activity: self.publisher_activity.clone(),
            };
            let handle = Self::receive_frame_data_loop(
                tx.subscribe(),
                receiver,
                context,
                Some(event_sender.clone()),
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
                self.publisher_activity.clone(),
            );
            tasks.spawn(async move {
                handle
                    .await
                    .map_err(|error| map_task_join_error("packet loop", &error))
            });
        }

        let stats_handle = Self::receive_statistics_data_loop(
            tx.subscribe(),
            self.statistic_data_receiver,
            self.statistic_data.clone(),
        );
        tasks.spawn(async move {
            stats_handle
                .await
                .map_err(|error| map_task_join_error("statistics loop", &error))
        });

        let event_handle = Self::receive_event_loop(ReceiveEventLoopContext {
            stream_handler: self.stream_handler,
            exit: tx,
            receiver: self.event_receiver,
            packet_senders: self.id_to_packet_sender,
            frame_senders: self.id_to_frame_sender,
            frame_generation: self.frame_generation,
            packet_generation: self.packet_generation,
            statistic_sender: self.statistic_data_sender,
            statistics_data: self.statistic_data.clone(),
        });
        let event_result = event_handle
            .await
            .map_err(|error| map_task_join_error("event loop", &error));

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
