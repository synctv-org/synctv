//! Rung publication: forward a transcode worker's output frames into
//! StreamHub as a new stream and own its cluster-registry generation.
//!
//! A rung is indistinguishable from any other locally published stream once
//! this module is done: HTTP-FLV, HLS, and cross-node relay pick it up
//! through the normal paths.

use std::sync::Arc;
use std::time::Duration;

use synctv_xiu::rtmp::session::common::RtmpStreamHandler;
use synctv_xiu::streamhub::{
    define::{
        NotifyInfo, PubDataType, PublishType, PublisherInfo, StreamHubEvent, StreamHubEventSender,
    },
    send_event_with_backpressure_timeout_for, spawn_event_delivery_with_backpressure_timeout_for,
    stream::StreamIdentifier,
    utils::Uuid,
};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::relay::registry::HEARTBEAT_INTERVAL_SECS;
use crate::relay::registry_trait::{StreamGenerationRegistration, StreamRegistryTrait};

use super::worker::{TranscodeError, TranscodeOutputReceiver};

/// Rung registrations carry no user; the empty id skips reverse indexes,
/// mirroring external publishers.
const TRANSCODE_PUBLISHER_USER_ID: &str = "";
const STREAMHUB_EVENT_SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// One rung publication bound to a source stream's room.
pub struct RungPublisher {
    pub room_id: String,
    /// Stream name of the rung (`{mediaId}-{rung}`).
    pub rung_stream_name: String,
    pub generation_id: Uuid,
}

/// Publish and serve one rung until its output ends, it is cancelled, or it
/// loses registry ownership. Always tears the publication and registry entry
/// down before returning.
///
/// # Errors
///
/// Returns the rung outcome for scheduler bookkeeping; teardown failures are
/// logged, not propagated as job failures.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    publisher: &RungPublisher,
    mut output: TranscodeOutputReceiver,
    registry: Arc<dyn StreamRegistryTrait>,
    stream_hub_event_sender: StreamHubEventSender,
    local_node_id: &str,
    local_cluster_address: &str,
    cancel: &CancellationToken,
) -> Result<(), TranscodeError> {
    let identifier = StreamIdentifier::Rtmp {
        app_name: publisher.room_id.clone(),
        stream_name: publisher.rung_stream_name.clone(),
    };

    // 1. Publish into StreamHub; the handler replays cached GOPs to new
    //    subscribers like any RTMP publication.
    let (frame_sender, _packet_sender) =
        publish_to_stream_hub(&stream_hub_event_sender, &identifier, publisher).await?;

    // 2. Own the registry slot so this node serves the rung cluster-wide.
    let lease_epoch =
        register_rung(&registry, publisher, local_node_id, local_cluster_address).await?;

    info!(
        room_id = %publisher.room_id,
        rung = %publisher.rung_stream_name,
        generation_id = %publisher.generation_id,
        lease_epoch,
        "Transcode rung published"
    );

    // 3. Forward frames, keep the lease alive, and mark the generation
    //    playable once frames actually flow.
    let mut outcome = Ok(());
    let mut ready_marked = false;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    heartbeat.tick().await; // The first tick completes immediately.

    loop {
        tokio::select! {
            () = cancel.cancelled() => {
                info!(
                    room_id = %publisher.room_id,
                    rung = %publisher.rung_stream_name,
                    "Transcode rung cancelled"
                );
                break;
            }
            frame = output.recv() => {
                let Some(frame) = frame else {
                    // Worker output ended: the job is done, whatever the cause.
                    break;
                };
                if let Err(error) = frame_sender.send(frame).await {
                    warn!(
                        room_id = %publisher.room_id,
                        rung = %publisher.rung_stream_name,
                        error = ?error,
                        "Transcode rung stream hub channel closed"
                    );
                    break;
                }
                if !ready_marked {
                    ready_marked = true;
                    match registry
                        .mark_generation_ready(
                            &publisher.room_id,
                            &publisher.rung_stream_name,
                            &publisher.generation_id.to_string(),
                            lease_epoch,
                        )
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            warn!(
                                room_id = %publisher.room_id,
                                rung = %publisher.rung_stream_name,
                                "Transcode rung lost registry ownership before ready"
                            );
                            outcome = Err(TranscodeError::Failed(
                                "rung generation ownership changed before ready".to_string(),
                            ));
                            break;
                        }
                        Err(error) => {
                            warn!(
                                room_id = %publisher.room_id,
                                rung = %publisher.rung_stream_name,
                                %error,
                                "Failed to mark transcode rung ready"
                            );
                        }
                    }
                }
            }
            _ = heartbeat.tick() => {
                match registry
                    .refresh_generation_lease(
                        &publisher.room_id,
                        &publisher.rung_stream_name,
                        &publisher.generation_id.to_string(),
                        TRANSCODE_PUBLISHER_USER_ID,
                        local_node_id,
                        lease_epoch,
                    )
                    .await
                {
                    Ok(crate::relay::registry_trait::LeaseRefreshOutcome::Refreshed) => {}
                    Ok(outcome_kind) => {
                        warn!(
                            room_id = %publisher.room_id,
                            rung = %publisher.rung_stream_name,
                            ?outcome_kind,
                            "Transcode rung lost registry ownership"
                        );
                        outcome = Err(TranscodeError::Failed(
                            "rung generation lease lost".to_string(),
                        ));
                        break;
                    }
                    Err(error) => {
                        warn!(
                            room_id = %publisher.room_id,
                            rung = %publisher.rung_stream_name,
                            %error,
                            "Failed to refresh transcode rung lease"
                        );
                    }
                }
            }
        }
    }

    // 4. Teardown: release the registry slot (best effort under fencing)
    //    and unpublish from StreamHub.
    match registry
        .deactivate_generation_if_lease_matches(
            &publisher.room_id,
            &publisher.rung_stream_name,
            &publisher.generation_id.to_string(),
            lease_epoch,
        )
        .await
    {
        Ok(deactivated) => {
            if !deactivated {
                warn!(
                    room_id = %publisher.room_id,
                    rung = %publisher.rung_stream_name,
                    "Transcode rung registry entry already replaced"
                );
            }
        }
        Err(error) => {
            warn!(
                room_id = %publisher.room_id,
                rung = %publisher.rung_stream_name,
                %error,
                "Failed to deactivate transcode rung registry entry"
            );
        }
    }
    let unpublish_event = StreamHubEvent::UnPublish {
        identifier,
        generation_id: publisher.generation_id,
    };
    spawn_event_delivery_with_backpressure_timeout_for(
        stream_hub_event_sender,
        unpublish_event,
        STREAMHUB_EVENT_SEND_TIMEOUT,
    );

    outcome
}

async fn publish_to_stream_hub(
    stream_hub_event_sender: &StreamHubEventSender,
    identifier: &StreamIdentifier,
    publisher: &RungPublisher,
) -> Result<(synctv_xiu::streamhub::define::FrameDataSender, Option<()>), TranscodeError> {
    let publisher_info = PublisherInfo {
        id: publisher.generation_id,
        pub_type: PublishType::Transcode,
        pub_data_type: PubDataType::Frame,
        notify_info: NotifyInfo {
            request_url: format!(
                "transcode://{}/{}",
                publisher.room_id, publisher.rung_stream_name
            ),
            remote_addr: String::new(),
        },
    };
    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    let publish_event = StreamHubEvent::Publish {
        identifier: identifier.clone(),
        info: publisher_info,
        stream_handler: Arc::new(RtmpStreamHandler::new()),
        result_sender,
    };
    send_event_with_backpressure_timeout_for(
        stream_hub_event_sender,
        publish_event,
        STREAMHUB_EVENT_SEND_TIMEOUT,
    )
    .await
    .map_err(|error| {
        TranscodeError::Failed(format!("failed to send rung publish event: {error}"))
    })?;
    let result = result_receiver
        .await
        .map_err(|_| TranscodeError::Failed("rung publish result channel closed".to_string()))?
        .map_err(|error| TranscodeError::Failed(format!("rung publish rejected: {error}")))?;
    let frame_sender = result.0.ok_or_else(|| {
        TranscodeError::Failed("rung publish returned no frame sender".to_string())
    })?;
    Ok((frame_sender, None))
}

async fn register_rung(
    registry: &Arc<dyn StreamRegistryTrait>,
    publisher: &RungPublisher,
    local_node_id: &str,
    local_cluster_address: &str,
) -> Result<u64, TranscodeError> {
    let registered = registry
        .try_activate_generation_with_capabilities(StreamGenerationRegistration::new(
            &publisher.room_id,
            &publisher.rung_stream_name,
            local_node_id,
            TRANSCODE_PUBLISHER_USER_ID,
            local_cluster_address,
            &publisher.generation_id.to_string(),
        ))
        .await
        .map_err(|error| TranscodeError::Failed(format!("rung registration failed: {error}")))?;
    if !registered {
        return Err(TranscodeError::Failed(
            "another publisher already owns this rung".to_string(),
        ));
    }
    // Read back the fencing epoch for all subsequent lease-guarded calls.
    let generation = registry
        .get_active_generation(&publisher.room_id, &publisher.rung_stream_name)
        .await
        .map_err(|error| {
            TranscodeError::Failed(format!("rung registration read-back failed: {error}"))
        })?
        .ok_or_else(|| TranscodeError::Failed("rung registration disappeared".to_string()))?;
    if generation.node_id != local_node_id
        || generation.generation_id != publisher.generation_id.to_string()
    {
        return Err(TranscodeError::Failed(
            "rung registration ownership mismatch".to_string(),
        ));
    }
    Ok(generation.lease_epoch)
}
