mod channels;
pub mod define;
pub mod errors;
mod reliability;
pub mod statistics;
pub mod stream;
mod transceiver;
pub mod utils;

pub use reliability::{
    send_event_with_backpressure_timeout, send_event_with_backpressure_timeout_for,
    spawn_event_delivery_with_backpressure_timeout,
    spawn_event_delivery_with_backpressure_timeout_for, subscribe_with_rollback_on_timeout,
    SubscribeWithRollbackError,
};
#[cfg(test)]
use transceiver::ReceiveEventLoopContext;
use transceiver::StreamDataTransceiver;
use {
    channels::{build_publisher_data_channel, build_subscriber_data_channel},
    define::{
        BroadcastEvent, BroadcastEventSender, DataReceiver, DataSender, PublisherActivityCallback,
        StatisticDataSender, StreamHubEvent, StreamHubEventReceiver, StreamHubEventSender,
        SubEventExecuteResultSender, SubscriberInfo, TStreamHandler, TransceiverEvent,
        TransceiverEventSender,
    },
    errors::{StreamHubError, StreamHubErrorValue},
    std::any::Any,
    std::collections::HashMap,
    std::sync::Arc,
    stream::StreamIdentifier,
    tokio::sync::{broadcast, mpsc},
    utils::Uuid,
};

fn panic_payload_to_string(panic_payload: &(dyn Any + Send)) -> String {
    if let Some(message) = panic_payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = panic_payload.downcast_ref::<String>() {
        message.clone()
    } else {
        format!(
            "non-string panic payload of type {}",
            std::any::type_name_of_val(panic_payload)
        )
    }
}

pub struct StreamsHub {
    //stream identifier to transceiver event sender
    streams: HashMap<StreamIdentifier, ActiveStream>,
    //event is consumed in Stream hub, produced from other protocol sessions
    hub_event_receiver: StreamHubEventReceiver,
    //event is produced from other protocol sessions
    hub_event_sender: StreamHubEventSender,
    //broadcast publish/unpublish events to subscribers (HLS remuxer, publisher manager, etc.)
    client_event_sender: BroadcastEventSender,
    publisher_activity_callback: Option<PublisherActivityCallback>,
}

struct ActiveStream {
    generation_id: Uuid,
    event_sender: TransceiverEventSender,
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
            publisher_activity_callback: None,
        }
    }

    /// Record media activity at publisher ingress, before subscriber fan-out.
    #[must_use]
    pub fn with_publisher_activity_callback(mut self, callback: PublisherActivityCallback) -> Self {
        self.publisher_activity_callback = Some(callback);
        self
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
                let msg = panic_payload_to_string(panic_payload.as_ref());
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
                    let (frame_sender, packet_sender, receiver) =
                        build_publisher_data_channel(&info.pub_data_type);

                    let result = match self.publish(
                        identifier.clone(),
                        info.id,
                        info.pub_type,
                        receiver,
                        stream_handler,
                    ) {
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
                    generation_id,
                } => {
                    if let Err(err) = self.unpublish(&identifier, generation_id) {
                        match err.value {
                            StreamHubErrorValue::NoAppName => {
                                tracing::debug!(
                                    "event_loop Unpublish ignored for already-removed stream: {identifier}"
                                );
                            }
                            _ => {
                                tracing::error!(
                                    "event_loop Unpublish err: {err} with identifier: {identifier}"
                                );
                            }
                        }
                    }
                }
                StreamHubEvent::ForceUnPublish { identifier } => {
                    if let Err(err) = self.force_unpublish(&identifier) {
                        if let StreamHubErrorValue::NoAppName = err.value {
                            tracing::debug!(
                                "event_loop ForceUnPublish ignored for already-removed stream: {identifier}"
                            );
                        } else {
                            tracing::error!(
                            "event_loop ForceUnPublish err: {err} with identifier: {identifier}"
                        );
                        }
                    }
                }
                StreamHubEvent::Subscribe {
                    identifier,
                    info,
                    result_sender,
                } => {
                    self.handle_subscribe_event(identifier, info, None, result_sender)
                        .await;
                }
                StreamHubEvent::SubscribeWithGeneration {
                    identifier,
                    info,
                    expected_generation_id,
                    result_sender,
                } => {
                    self.handle_subscribe_event(
                        identifier,
                        info,
                        Some(expected_generation_id),
                        result_sender,
                    )
                    .await;
                }
                StreamHubEvent::UnSubscribe { identifier, info } => {
                    if let Err(err) = self.unsubscribe(&identifier, info) {
                        match err.value {
                            StreamHubErrorValue::NoAppName => {
                                tracing::debug!(
                                    "event_loop UnSubscribe ignored for already-removed stream: {identifier}"
                                );
                            }
                            _ => {
                                tracing::warn!(
                                    "event_loop UnSubscribe err: {err} with identifier: {identifier}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    async fn handle_subscribe_event(
        &mut self,
        identifier: StreamIdentifier,
        info: SubscriberInfo,
        expected_generation_id: Option<Uuid>,
        result_sender: SubEventExecuteResultSender,
    ) {
        if let Some(expected_generation_id) = expected_generation_id {
            let generation_matches = self
                .streams
                .get(&identifier)
                .is_some_and(|stream| stream.generation_id == expected_generation_id);
            if !generation_matches {
                let error = StreamHubError {
                    value: StreamHubErrorValue::InternalTaskError(
                        "publisher generation changed before subscription".to_string(),
                    ),
                };
                let _ = result_sender.send(Err(error));
                return;
            }
        }

        let info_clone = info.clone();
        let (sender, receiver) = build_subscriber_data_channel(&info);
        let rv = match self.subscribe(&identifier, info_clone, sender).await {
            Ok(statistic_data_sender) => Ok((receiver, Some(statistic_data_sender))),
            Err(err) => {
                tracing::error!("event_loop Subscribe error: {err}");
                Err(err)
            }
        };

        if result_sender.send(rv).is_err() {
            // The RPC may have timed out or been cancelled after the
            // transceiver inserted the subscriber. Roll it back immediately.
            if let Err(error) = self.unsubscribe(&identifier, info) {
                tracing::debug!("Subscribe rollback failed for {identifier}: {error}");
            }
        }
    }

    pub async fn subscribe(
        &mut self,
        identifier: &StreamIdentifier,
        sub_info: SubscriberInfo,
        sender: DataSender,
    ) -> Result<StatisticDataSender, StreamHubError> {
        if let Some(active_stream) = self.streams.get_mut(identifier) {
            let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
            let event = TransceiverEvent::Subscribe {
                sender,
                info: sub_info,
                result_sender,
            };
            tracing::info!("subscribe stream: {identifier}");
            active_stream
                .event_sender
                .send(event)
                .map_err(|_| StreamHubError {
                    value: StreamHubErrorValue::SendError,
                })?;

            return result_receiver.await?;
        }

        Err(StreamHubError {
            value: StreamHubErrorValue::NoAppOrStreamName,
        })
    }

    pub fn unsubscribe(
        &mut self,
        identifier: &StreamIdentifier,
        sub_info: SubscriberInfo,
    ) -> Result<(), StreamHubError> {
        if let Some(active_stream) = self.streams.get_mut(identifier) {
            tracing::info!("unsubscribe stream: {identifier}");
            let event = TransceiverEvent::UnSubscribe { info: sub_info };
            active_stream
                .event_sender
                .send(event)
                .map_err(|_| StreamHubError {
                    value: StreamHubErrorValue::SendError,
                })?;
        } else {
            tracing::debug!("unsubscribe ignored for missing stream: {identifier}");
            return Err(StreamHubError {
                value: StreamHubErrorValue::NoAppName,
            });
        }

        Ok(())
    }

    //publish a stream
    pub fn publish(
        &mut self,
        identifier: StreamIdentifier,
        generation_id: Uuid,
        pub_type: define::PublishType,
        receiver: DataReceiver,
        handler: Arc<dyn TStreamHandler>,
    ) -> Result<StatisticDataSender, StreamHubError> {
        // Reserve the stream slot before spawning the transceiver.
        let entry = match self.streams.entry(identifier.clone()) {
            std::collections::hash_map::Entry::Occupied(_) => {
                return Err(StreamHubError {
                    value: StreamHubErrorValue::Exists,
                });
            }
            std::collections::hash_map::Entry::Vacant(e) => e,
        };

        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let mut transceiver =
            StreamDataTransceiver::new(receiver, event_receiver, identifier.clone(), handler);
        if pub_type == define::PublishType::RtmpPush {
            if let Some(callback) = self.publisher_activity_callback.clone() {
                transceiver = transceiver.with_publisher_activity_callback(generation_id, callback);
            }
        }

        let statistic_data_sender = transceiver.get_statistics_data_sender();
        let identifier_clone = identifier.clone();
        let event_sender_clone = event_sender.clone();
        let hub_sender_for_cleanup = self.hub_event_sender.clone();

        // Wraps the task so that if the transceiver panics or errors out, an UnPublish
        // event is sent to clean up the dead entry from the `streams` HashMap.
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
                        tracing::info!("transceiver run success, identifier: {identifier_clone}");
                        true
                    }
                    Ok(Err(err)) => {
                        tracing::error!(
                            "transceiver run error, identifier: {identifier_clone}, error: {err}",
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
                            generation_id,
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

        entry.insert(ActiveStream {
            generation_id,
            event_sender,
        });

        // Always broadcast publish event to listeners (HLS remuxer, publisher manager, etc.)
        let client_event = BroadcastEvent::Publish {
            identifier,
            pub_type,
            generation_id,
        };
        if let Err(err) = self.client_event_sender.send(client_event) {
            tracing::debug!("broadcast Publish event: no receivers ({err})");
        }

        Ok(statistic_data_sender)
    }

    fn unpublish(
        &mut self,
        identifier: &StreamIdentifier,
        generation_id: Uuid,
    ) -> Result<(), StreamHubError> {
        let Some(active_stream) = self.streams.get(identifier) else {
            return Err(StreamHubError {
                value: StreamHubErrorValue::NoAppName,
            });
        };
        if active_stream.generation_id != generation_id {
            tracing::debug!(
                %identifier,
                stale_generation_id = %generation_id,
                current_generation_id = %active_stream.generation_id,
                "ignoring stale publisher cleanup"
            );
            return Ok(());
        }
        match self.streams.remove(identifier) {
            Some(active_stream) => {
                let event = TransceiverEvent::UnPublish {};
                if active_stream.event_sender.send(event).is_err() {
                    tracing::warn!(
                        "unpublish: channel already closed for {identifier}; \
                         transceiver has already exited"
                    );
                }
                tracing::info!("unpublish remove stream, stream identifier: {identifier}");

                // Broadcast unpublish event to listeners
                let client_event = BroadcastEvent::UnPublish {
                    identifier: identifier.clone(),
                    generation_id,
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

    fn force_unpublish(&mut self, identifier: &StreamIdentifier) -> Result<(), StreamHubError> {
        let generation_id = self
            .streams
            .get(identifier)
            .map(|stream| stream.generation_id)
            .ok_or(StreamHubError {
                value: StreamHubErrorValue::NoAppName,
            })?;
        self.unpublish(identifier, generation_id)
    }
}

#[cfg(test)]
mod tests {
    use super::channels::build_subscriber_data_channel;
    use super::*;
    use crate::streamhub::define::{
        FrameData, FrameDataReceiver, FrameDataSender, NotifyInfo, StatisticData, SubDataType,
        SubscribeType,
    };
    use crate::streamhub::statistics::StatisticsStream;
    use crate::streamhub::transceiver::{SubscriberDropCounter, PUBLISHER_ACTIVITY_INTERVAL};
    use crate::streamhub::utils::Uuid;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::{broadcast, oneshot, Mutex};

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

    struct FailingPriorDataHandler;

    #[async_trait]
    impl TStreamHandler for FailingPriorDataHandler {
        async fn send_prior_data(
            &self,
            _sender: DataSender,
            _sub_type: SubscribeType,
        ) -> Result<(), StreamHubError> {
            Err(StreamHubError {
                value: StreamHubErrorValue::SubscriberClosed,
            })
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

    fn test_subscriber_with_type(sub_type: SubscribeType) -> SubscriberInfo {
        SubscriberInfo {
            sub_type,
            ..test_subscriber()
        }
    }

    #[test]
    fn test_panic_payload_to_string_handles_string_payloads() {
        let str_payload: Box<dyn std::any::Any + Send> = Box::new("streamhub panic");
        assert_eq!(
            panic_payload_to_string(str_payload.as_ref()),
            "streamhub panic"
        );

        let string_payload: Box<dyn std::any::Any + Send> = Box::new("owned panic".to_string());
        assert_eq!(
            panic_payload_to_string(string_payload.as_ref()),
            "owned panic"
        );
    }

    #[test]
    fn test_panic_payload_to_string_handles_non_string_payloads() {
        let payload: Box<dyn std::any::Any + Send> = Box::new(42_u8);
        let message = panic_payload_to_string(payload.as_ref());

        assert!(
            message.contains("non-string panic payload"),
            "unexpected panic payload message: {message}"
        );
    }

    #[test]
    fn test_build_subscriber_data_channel_keeps_rtmp_pull_bounded() {
        let (sender, receiver) =
            build_subscriber_data_channel(&test_subscriber_with_type(SubscribeType::RtmpPull));

        assert!(matches!(
            sender,
            DataSender::Frame {
                sender: FrameDataSender::Bounded(_) | FrameDataSender::Budgeted(_)
            }
        ));
        assert!(matches!(
            receiver.frame_receiver,
            Some(
                define::FrameDataReceiver::Bounded(_) | define::FrameDataReceiver::Budgeted { .. }
            )
        ));
    }

    #[test]
    fn test_build_subscriber_data_channel_bounds_internal_remuxers() {
        for sub_type in [
            SubscribeType::RtmpRemux2HttpFlv,
            SubscribeType::RtmpRemux2Hls,
            SubscribeType::RtmpRelay,
        ] {
            let (sender, receiver) =
                build_subscriber_data_channel(&test_subscriber_with_type(sub_type.clone()));
            assert!(
                matches!(
                    sender,
                    DataSender::Frame {
                        sender: FrameDataSender::Bounded(_) | FrameDataSender::Budgeted(_)
                    }
                ),
                "internal subscriber {sub_type:?} should use bounded sender"
            );
            assert!(
                matches!(
                    receiver.frame_receiver,
                    Some(
                        define::FrameDataReceiver::Bounded(_)
                            | define::FrameDataReceiver::Budgeted { .. }
                    )
                ),
                "internal subscriber {sub_type:?} should use bounded receiver"
            );
        }
    }

    #[tokio::test]
    async fn test_receive_event_loop_exits_when_event_channel_closes() {
        let (exit_tx, _) = broadcast::channel(1);
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let (stat_tx, _stat_rx) = mpsc::unbounded_channel();

        let handle = StreamDataTransceiver::receive_event_loop(ReceiveEventLoopContext {
            stream_handler: Arc::new(NoopHandler),
            exit: exit_tx,
            receiver: event_rx,
            packet_senders: Arc::new(Mutex::new(HashMap::new())),
            frame_senders: Arc::new(Mutex::new(HashMap::new())),
            frame_generation: Arc::new(AtomicU64::new(0)),
            packet_generation: Arc::new(AtomicU64::new(0)),
            statistic_sender: stat_tx,
            statistics_data: Arc::new(Mutex::new(StatisticsStream::new(test_identifier()))),
        });

        drop(event_tx);

        let join_result = tokio::time::timeout(Duration::from_millis(200), handle)
            .await
            .expect("event loop should exit promptly when channel closes");

        assert!(join_result.is_ok(), "event loop task should not panic");
    }

    #[tokio::test]
    async fn test_subscribe_returns_prior_data_error_without_timeout() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let mut hub = StreamsHub::new(hub_sender, hub_receiver);
        let identifier = test_identifier();

        let (_frame_sender, frame_receiver) = mpsc::channel(8);
        let receiver = DataReceiver {
            frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
            packet_receiver: None,
        };

        hub.publish(
            identifier.clone(),
            Uuid::new(),
            define::PublishType::RtmpPush,
            receiver,
            Arc::new(FailingPriorDataHandler),
        )
        .expect("publish should succeed");

        let (sub_sender, _sub_receiver) = mpsc::channel(8);
        let result = tokio::time::timeout(
            Duration::from_millis(200),
            hub.subscribe(
                &identifier,
                test_subscriber(),
                DataSender::Frame {
                    sender: FrameDataSender::bounded(sub_sender),
                },
            ),
        )
        .await
        .expect("subscribe should complete without caller-side timeout");

        assert!(
            matches!(
                result,
                Err(StreamHubError {
                    value: StreamHubErrorValue::SubscriberClosed
                })
            ),
            "prior-data failure must be returned to the subscriber"
        );
    }

    #[tokio::test]
    async fn test_transceiver_run_propagates_event_loop_panic() {
        let (event_tx, event_rx) = mpsc::unbounded_channel();
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
                    sender: FrameDataSender::bounded(frame_sender),
                },
                info: test_subscriber(),
                result_sender,
            })
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
                generation_id: Uuid::new(),
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
                generation_id: Uuid::new(),
            },
        )
        .await
        .expect_err("closed channel should surface send error");

        assert!(matches!(err.value, StreamHubErrorValue::EventChannelClosed));
    }

    #[tokio::test(start_paused = true)]
    async fn test_send_event_with_backpressure_timeout_errors_when_full_until_timeout() {
        let (sender, _receiver) = mpsc::channel(1);
        sender
            .try_send(StreamHubEvent::UnPublish {
                identifier: test_identifier(),
                generation_id: Uuid::new(),
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

        tokio::time::advance(Duration::from_secs(2)).await;
        tokio::task::yield_now().await;

        let err = send_task
            .await
            .expect("send task should join")
            .expect_err("full channel should time out");
        assert!(matches!(err.value, StreamHubErrorValue::EventSendTimeout));
    }

    #[tokio::test]
    async fn test_event_loop_tolerates_duplicate_unpublish_cleanup() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let mut hub = StreamsHub::new(hub_sender.clone(), hub_receiver);
        let identifier = test_identifier();
        let generation_id = Uuid::new();

        let (_frame_sender, frame_receiver) = mpsc::channel(8);
        let receiver = DataReceiver {
            frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
            packet_receiver: None,
        };

        hub.publish(
            identifier.clone(),
            generation_id,
            define::PublishType::RtmpPush,
            receiver,
            Arc::new(NoopHandler),
        )
        .expect("publish should succeed");

        hub_sender
            .send(StreamHubEvent::UnPublish {
                identifier: identifier.clone(),
                generation_id,
            })
            .await
            .expect("first unpublish should enqueue");
        hub_sender
            .send(StreamHubEvent::UnPublish {
                identifier: identifier.clone(),
                generation_id,
            })
            .await
            .expect("second unpublish should enqueue");

        let event_loop = tokio::spawn(async move { hub.event_loop().await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        event_loop.abort();
        let err = event_loop
            .await
            .expect_err("event loop task should be cancelled after abort");
        assert!(err.is_cancelled(), "unexpected join error: {err}");
    }

    #[tokio::test]
    async fn generation_bound_subscribe_rejects_republished_stream() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let mut hub = StreamsHub::new(hub_sender.clone(), hub_receiver);
        let identifier = test_identifier();
        let current_generation = Uuid::new();
        let stale_generation = Uuid::new();
        let (_frame_sender, frame_receiver) = mpsc::channel(8);
        let receiver = DataReceiver {
            frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
            packet_receiver: None,
        };
        hub.publish(
            identifier.clone(),
            current_generation,
            define::PublishType::RtmpPush,
            receiver,
            Arc::new(NoopHandler),
        )
        .expect("publish should succeed");

        let event_loop = tokio::spawn(async move { hub.event_loop().await });
        let (result_sender, result_receiver) = oneshot::channel();
        hub_sender
            .send(StreamHubEvent::SubscribeWithGeneration {
                identifier,
                info: test_subscriber(),
                expected_generation_id: stale_generation,
                result_sender,
            })
            .await
            .expect("subscribe should enqueue");

        let result = tokio::time::timeout(Duration::from_secs(1), result_receiver)
            .await
            .expect("generation mismatch should return promptly")
            .expect("event loop should send a result");
        assert!(result.is_err());

        event_loop.abort();
        let _ = event_loop.await;
    }

    #[tokio::test]
    async fn test_event_loop_tolerates_duplicate_unsubscribe_cleanup() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let mut hub = StreamsHub::new(hub_sender.clone(), hub_receiver);
        let identifier = test_identifier();
        let generation_id = Uuid::new();
        let subscriber = test_subscriber();
        let subscriber_id = subscriber.id;

        let (_frame_sender, frame_receiver) = mpsc::channel(8);
        let receiver = DataReceiver {
            frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
            packet_receiver: None,
        };

        hub.publish(
            identifier.clone(),
            generation_id,
            define::PublishType::RtmpPush,
            receiver,
            Arc::new(NoopHandler),
        )
        .expect("publish should succeed");

        let (sub_sender, _sub_receiver) = mpsc::channel(8);
        let stat_sender = hub
            .subscribe(
                &identifier,
                subscriber.clone(),
                DataSender::Frame {
                    sender: FrameDataSender::bounded(sub_sender),
                },
            )
            .await
            .expect("subscribe should succeed");

        stat_sender
            .send(StatisticData::Subscriber {
                id: subscriber_id,
                remote_addr: "127.0.0.1:0".to_string(),
                start_time: chrono::Local::now(),
                sub_type: subscriber.sub_type.clone(),
            })
            .expect("subscriber statistic should enqueue");

        hub.unpublish(&identifier, generation_id)
            .expect("explicit unpublish should succeed");

        hub_sender
            .send(StreamHubEvent::UnSubscribe {
                identifier,
                info: subscriber,
            })
            .await
            .expect("late unsubscribe should enqueue");

        let event_loop = tokio::spawn(async move { hub.event_loop().await });

        tokio::time::sleep(Duration::from_millis(50)).await;
        event_loop.abort();
        let err = event_loop
            .await
            .expect_err("event loop task should be cancelled after abort");
        assert!(err.is_cancelled(), "unexpected join error: {err}");
    }

    #[tokio::test]
    async fn test_receive_frame_data_removes_closed_subscriber_statistics() {
        let subscriber = test_subscriber();
        let subscriber_id = subscriber.id;
        let (sender, receiver) = mpsc::channel(1);
        drop(receiver);

        let frame_senders = Arc::new(Mutex::new(HashMap::from([(
            subscriber_id,
            SubscriberDropCounter {
                sender: FrameDataSender::bounded(sender),
                drop_count: Arc::new(AtomicU64::new(0)),
            },
        )])));
        let generation = Arc::new(AtomicU64::new(1));
        let mut cached_snapshot = Vec::new();
        let mut cached_gen = 0;
        let statistics_data = Arc::new(Mutex::new(StatisticsStream::new(test_identifier())));
        {
            let mut stats = statistics_data.lock().await;
            stats.subscriber_count = 1;
            stats.subscribers.insert(
                subscriber_id,
                statistics::StatisticSubscriber {
                    id: subscriber_id,
                    start_time: chrono::Local::now(),
                    remote_address: subscriber.notify_info.remote_addr.clone(),
                    sub_type: subscriber.sub_type.clone(),
                    send_bytes: 0,
                    send_bitrate: 0,
                    total_send_bytes: 0,
                },
            );
        }

        let context = transceiver::FrameDataLoopContext {
            stream_handler: Arc::new(NoopHandler) as Arc<dyn define::TStreamHandler>,
            frame_senders: Arc::clone(&frame_senders),
            generation: Arc::clone(&generation),
            statistics_data: Arc::clone(&statistics_data),
            publisher_activity: None,
        };
        StreamDataTransceiver::receive_frame_data(
            Some(FrameData::Video {
                timestamp: 0,
                data: bytes::Bytes::from_static(b"frame"),
            }),
            &context,
            &mut cached_snapshot,
            &mut cached_gen,
        )
        .await
        .unwrap();

        assert!(
            frame_senders.lock().await.is_empty(),
            "closed subscriber sender should be removed from fan-out map"
        );

        let stats = statistics_data.lock().await;
        assert_eq!(
            stats.subscriber_count, 0,
            "closed subscriber should no longer count toward subscriber_count"
        );
        assert!(
            !stats.subscribers.contains_key(&subscriber_id),
            "closed subscriber statistics entry must be removed to avoid zombie stats"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn publisher_activity_is_recorded_without_subscribers_and_throttled() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let activity_count = Arc::new(AtomicUsize::new(0));
        let callback_count = Arc::clone(&activity_count);
        let callback: define::PublisherActivityCallback = Arc::new(move |app, stream, _| {
            assert_eq!(app, "live");
            assert_eq!(stream, "panic-test");
            callback_count.fetch_add(1, Ordering::AcqRel);
        });
        let mut hub =
            StreamsHub::new(hub_sender, hub_receiver).with_publisher_activity_callback(callback);
        let identifier = test_identifier();
        let (frame_sender, frame_receiver) = mpsc::channel(8);

        hub.publish(
            identifier,
            Uuid::new(),
            define::PublishType::RtmpPush,
            DataReceiver {
                frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
                packet_receiver: None,
            },
            Arc::new(NoopHandler),
        )
        .expect("publish should succeed");

        frame_sender
            .send(FrameData::Video {
                timestamp: 0,
                data: bytes::Bytes::from_static(b"frame-0"),
            })
            .await
            .expect("first frame should enqueue");
        tokio::task::yield_now().await;
        assert_eq!(activity_count.load(Ordering::Acquire), 1);

        frame_sender
            .send(FrameData::Audio {
                timestamp: 1,
                data: bytes::Bytes::from_static(b"audio-0"),
            })
            .await
            .expect("second frame should enqueue");
        tokio::task::yield_now().await;
        assert_eq!(activity_count.load(Ordering::Acquire), 1);

        tokio::time::advance(PUBLISHER_ACTIVITY_INTERVAL).await;
        frame_sender
            .send(FrameData::Video {
                timestamp: 10_000,
                data: bytes::Bytes::from_static(b"frame-1"),
            })
            .await
            .expect("post-throttle frame should enqueue");
        tokio::task::yield_now().await;
        assert_eq!(activity_count.load(Ordering::Acquire), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn publisher_activity_survives_hls_subscriber_failure() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let activity_count = Arc::new(AtomicUsize::new(0));
        let callback_count = Arc::clone(&activity_count);
        let callback: define::PublisherActivityCallback = Arc::new(move |_, _, _| {
            callback_count.fetch_add(1, Ordering::AcqRel);
        });
        let mut hub =
            StreamsHub::new(hub_sender, hub_receiver).with_publisher_activity_callback(callback);
        let identifier = test_identifier();
        let generation_id = Uuid::new();
        let (frame_sender, frame_receiver) = mpsc::channel(8);

        hub.publish(
            identifier.clone(),
            generation_id,
            define::PublishType::RtmpPush,
            DataReceiver {
                frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
                packet_receiver: None,
            },
            Arc::new(NoopHandler),
        )
        .expect("publish should succeed");

        let hls_subscriber = test_subscriber_with_type(SubscribeType::RtmpRemux2Hls);
        let (hls_sender, hls_receiver) = mpsc::channel(1);
        hub.subscribe(
            &identifier,
            hls_subscriber,
            DataSender::Frame {
                sender: FrameDataSender::bounded(hls_sender),
            },
        )
        .await
        .expect("HLS subscriber should attach");
        drop(hls_receiver);

        frame_sender
            .send(FrameData::Video {
                timestamp: 0,
                data: bytes::Bytes::from_static(b"frame-before-remux-failure"),
            })
            .await
            .expect("frame should enqueue after HLS receiver closes");
        tokio::task::yield_now().await;
        assert_eq!(activity_count.load(Ordering::Acquire), 1);
        assert_eq!(
            hub.streams
                .get(&identifier)
                .map(|stream| stream.generation_id),
            Some(generation_id)
        );

        tokio::time::advance(PUBLISHER_ACTIVITY_INTERVAL).await;
        frame_sender
            .send(FrameData::Video {
                timestamp: 10_000,
                data: bytes::Bytes::from_static(b"frame-after-remux-failure"),
            })
            .await
            .expect("publisher should keep accepting frames");
        tokio::task::yield_now().await;
        assert_eq!(activity_count.load(Ordering::Acquire), 2);
    }

    #[tokio::test]
    async fn stale_unpublish_does_not_remove_republished_owner() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let mut hub = StreamsHub::new(hub_sender, hub_receiver);
        let identifier = test_identifier();
        let old_generation_id = Uuid::new();
        let new_generation_id = Uuid::new();
        let (old_frame_sender, old_frame_receiver) = mpsc::channel(1);

        hub.publish(
            identifier.clone(),
            old_generation_id,
            define::PublishType::RtmpPush,
            DataReceiver {
                frame_receiver: Some(FrameDataReceiver::bounded(old_frame_receiver)),
                packet_receiver: None,
            },
            Arc::new(NoopHandler),
        )
        .expect("old publisher should publish");
        hub.unpublish(&identifier, old_generation_id)
            .expect("old publisher should unpublish");

        let (new_frame_sender, new_frame_receiver) = mpsc::channel(1);
        hub.publish(
            identifier.clone(),
            new_generation_id,
            define::PublishType::RtmpPush,
            DataReceiver {
                frame_receiver: Some(FrameDataReceiver::bounded(new_frame_receiver)),
                packet_receiver: None,
            },
            Arc::new(NoopHandler),
        )
        .expect("replacement publisher should publish");

        hub.unpublish(&identifier, old_generation_id)
            .expect("stale cleanup should be ignored");
        assert_eq!(
            hub.streams
                .get(&identifier)
                .map(|stream| stream.generation_id),
            Some(new_generation_id)
        );

        drop(old_frame_sender);
        drop(new_frame_sender);
    }

    #[tokio::test]
    async fn force_unpublish_resolves_and_removes_current_owner() {
        let (hub_sender, hub_receiver) = mpsc::channel(8);
        let mut hub = StreamsHub::new(hub_sender, hub_receiver);
        let mut events = hub.get_client_event_consumer();
        let identifier = test_identifier();
        let generation_id = Uuid::new();
        let (frame_sender, frame_receiver) = mpsc::channel(1);

        hub.publish(
            identifier.clone(),
            generation_id,
            define::PublishType::RtmpPush,
            DataReceiver {
                frame_receiver: Some(FrameDataReceiver::bounded(frame_receiver)),
                packet_receiver: None,
            },
            Arc::new(NoopHandler),
        )
        .expect("publisher should publish");
        hub.force_unpublish(&identifier)
            .expect("administrative unpublish should resolve the active owner");

        assert!(!hub.streams.contains_key(&identifier));
        assert!(matches!(
            events.recv().await,
            Ok(BroadcastEvent::Publish { .. })
        ));
        assert!(matches!(
            events.recv().await,
            Ok(BroadcastEvent::UnPublish {
                generation_id: event_id,
                ..
            }) if event_id == generation_id
        ));
        drop(frame_sender);
    }
}
