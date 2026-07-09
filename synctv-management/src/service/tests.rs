use super::{parse_shutdown_mode, stop_server_event_stream};
use crate::lifecycle::{LifecycleStage, ManagementLifecycleController, ShutdownMode};
use crate::proto::ShutdownMode as ProtoShutdownMode;
use futures::TryStreamExt;
use std::borrow::Cow;
use synctv_adapter::error::{ClassifiedError, ErrorKind};
use synctv_core::service::{
    evict_expired_response_from_nodes, purge_response_from_nodes, validate_slice_cache_selection,
    SliceCacheEvictExpiredNodeResult, SliceCacheNodeFailure, SliceCachePurgeNodeResult,
};
use tonic::Code;

struct TestRuntimeError {
    kind: ErrorKind,
    message: &'static str,
}

impl ClassifiedError for TestRuntimeError {
    fn classify(&self) -> ErrorKind {
        self.kind
    }

    fn message(&self) -> Cow<'_, str> {
        Cow::Borrowed(self.message)
    }
}

#[test]
fn map_api_error_preserves_service_unavailable() {
    let status = super::map_api_error(&TestRuntimeError {
        kind: ErrorKind::ServiceUnavailable,
        message: "live streaming backend unavailable",
    });

    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.message(), "live streaming backend unavailable");
}

#[test]
fn map_api_error_hides_internal_details() {
    let status = super::map_api_error(&TestRuntimeError {
        kind: ErrorKind::Internal,
        message: "redis://user:secret@localhost:6379 failure",
    });

    assert_eq!(status.code(), tonic::Code::Internal);
    assert_eq!(status.message(), "Internal error");
    assert!(!status.message().contains("secret"));
}

#[test]
fn slice_cache_target_validation_rejects_conflicting_node_selection() {
    let error = validate_slice_cache_selection(Some("node-a"), true)
        .expect_err("node_id and all_nodes must be mutually exclusive");

    assert_eq!(
        error.to_string(),
        "nodeId and allNodes are mutually exclusive"
    );
}

#[test]
fn slice_cache_target_validation_trims_node_id() -> Result<(), Box<dyn std::error::Error>> {
    let target = validate_slice_cache_selection(Some("  node-a  "), false)?;

    assert_eq!(target.as_deref(), Some("node-a"));
    Ok(())
}

#[test]
fn purge_slice_cache_response_aggregates_nodes_and_failures() {
    let response = purge_response_from_nodes(
        vec![
            SliceCachePurgeNodeResult {
                node_id: "node-a".to_string(),
                success: true,
                removed_entries: 2,
                freed_bytes: 128,
                stats: None,
            },
            SliceCachePurgeNodeResult {
                node_id: "node-b".to_string(),
                success: true,
                removed_entries: 3,
                freed_bytes: 256,
                stats: None,
            },
        ],
        vec![SliceCacheNodeFailure {
            node_id: "node-c".to_string(),
            error: "timeout".to_string(),
        }],
    );

    assert!(!response.success);
    assert_eq!(response.removed_entries, 5);
    assert_eq!(response.freed_bytes, 384);
    assert!(response.stats.is_none());
    assert_eq!(response.nodes.len(), 2);
    assert_eq!(response.failures.len(), 1);
}

#[test]
fn evict_expired_slice_cache_response_aggregates_nodes_and_failures() {
    let response = evict_expired_response_from_nodes(
        vec![
            SliceCacheEvictExpiredNodeResult {
                node_id: "node-a".to_string(),
                success: true,
                removed_expired_entries: 4,
                stats: None,
            },
            SliceCacheEvictExpiredNodeResult {
                node_id: "node-b".to_string(),
                success: false,
                removed_expired_entries: 1,
                stats: None,
            },
        ],
        Vec::new(),
    );

    assert!(!response.success);
    assert_eq!(response.removed_expired_entries, 5);
    assert!(response.stats.is_none());
    assert_eq!(response.nodes.len(), 2);
    assert!(response.failures.is_empty());
}

#[test]
fn parse_shutdown_mode_accepts_defined_values() -> Result<(), tonic::Status> {
    assert_eq!(
        parse_shutdown_mode(ProtoShutdownMode::Unspecified as i32)?,
        ShutdownMode::Graceful
    );
    assert_eq!(
        parse_shutdown_mode(ProtoShutdownMode::Graceful as i32)?,
        ShutdownMode::Graceful
    );
    assert_eq!(
        parse_shutdown_mode(ProtoShutdownMode::Force as i32)?,
        ShutdownMode::Force
    );
    Ok(())
}

#[test]
fn parse_shutdown_mode_rejects_unknown_value() {
    let status = parse_shutdown_mode(99).expect_err("unknown mode should fail");

    assert_eq!(status.code(), Code::InvalidArgument);
    assert_eq!(status.message(), "invalid shutdown mode: 99");
}

#[tokio::test]
async fn stop_server_stream_ends_at_finalizing_without_duplicate_shutdown_requested(
) -> Result<(), tonic::Status> {
    let controller = ManagementLifecycleController::new();
    let subscription = controller.subscribe();
    let requested_event = controller.request_shutdown(ShutdownMode::Graceful);
    controller.publish_finalizing();
    controller.publish_completed();

    let events = stop_server_event_stream(
        subscription.snapshot,
        requested_event,
        subscription.receiver,
    )
    .try_collect::<Vec<_>>()
    .await?;

    let stages = events.iter().map(|event| event.stage).collect::<Vec<_>>();
    assert_eq!(
        stages,
        vec![
            LifecycleStage::Ready.as_proto(),
            LifecycleStage::ShutdownRequested.as_proto(),
            LifecycleStage::Finalizing.as_proto(),
        ]
    );
    assert!(
        events.last().is_some_and(|event| event.terminal),
        "finalizing must terminate the stop stream before server shutdown waits on the RPC"
    );
    Ok(())
}
