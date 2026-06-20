use super::{
    define::{self, StreamHubEvent, StreamHubEventSender, SubscriberInfo},
    errors::{StreamHubError, StreamHubErrorValue},
    stream::StreamIdentifier,
};

const EVENT_SEND_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(10);
const EVENT_SEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

pub async fn send_event_with_backpressure_timeout(
    sender: &StreamHubEventSender,
    event: StreamHubEvent,
) -> Result<(), StreamHubError> {
    send_event_with_backpressure_timeout_for(sender, event, EVENT_SEND_TIMEOUT).await
}

pub async fn send_event_with_backpressure_timeout_for(
    sender: &StreamHubEventSender,
    event: StreamHubEvent,
    timeout: std::time::Duration,
) -> Result<(), StreamHubError> {
    match sender.try_send(event) {
        Ok(()) => Ok(()),
        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => Err(StreamHubError {
            value: StreamHubErrorValue::EventChannelClosed,
        }),
        Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
            let send_future = async {
                let mut pending = event;
                loop {
                    match sender.try_send(pending) {
                        Ok(()) => return Ok(()),
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            return Err(StreamHubError {
                                value: StreamHubErrorValue::EventChannelClosed,
                            });
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(event)) => {
                            pending = event;
                            tokio::time::sleep(EVENT_SEND_RETRY_DELAY).await;
                        }
                    }
                }
            };

            match tokio::time::timeout(timeout, send_future).await {
                Ok(result) => result,
                Err(_) => Err(StreamHubError {
                    value: StreamHubErrorValue::EventSendTimeout,
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
) -> Result<(define::DataReceiver, Option<define::StatisticDataSender>), SubscribeWithRollbackError>
{
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

    if let Ok(result) = tokio::time::timeout(timeout, result_receiver).await {
        result
            .map_err(|_| StreamHubError {
                value: StreamHubErrorValue::ResultReceiverDropped,
            })?
            .map_err(SubscribeWithRollbackError::StreamHub)
    } else {
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

pub fn spawn_event_delivery_with_backpressure_timeout(
    sender: StreamHubEventSender,
    event: StreamHubEvent,
) {
    spawn_event_delivery_with_backpressure_timeout_for(sender, event, EVENT_SEND_TIMEOUT);
}

pub fn spawn_event_delivery_with_backpressure_timeout_for(
    sender: StreamHubEventSender,
    event: StreamHubEvent,
    timeout: std::time::Duration,
) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                if let Err(err) =
                    send_event_with_backpressure_timeout_for(&sender, event, timeout).await
                {
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
