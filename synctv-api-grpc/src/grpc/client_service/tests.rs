use super::streaming::{await_grpc_receive_or_response_close, GrpcReceiveOutcome};
use super::*;
use synctv_core::models::UserId;
use synctv_proto::client::{ClientMessage, ServerMessage};
use tonic_types::StatusExt;

type TestResult<T = ()> = anyhow::Result<T>;

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn detail_error_code(status: &Status) -> Option<String> {
    status
        .get_error_details()
        .error_info()
        .and_then(|detail| detail.metadata.get("errorCode").cloned())
}

#[test]
fn test_map_api_error_internal_hides_details() {
    let err = synctv_api_common::impls::ApiError::Internal("secret DB password=abc123".to_string());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::Internal);
    // Internal errors should NOT leak implementation details
    assert_eq!(status.message(), "Internal error");
    assert!(!status.message().contains("password"));
    assert!(!status.message().contains("secret"));
}

#[test]
fn test_email_api_missing_maps_to_service_unavailable() {
    let err = ClientServiceImpl::email_api_unavailable_error();
    assert!(matches!(
        err.classify(),
        synctv_api_common::impls::ErrorKind::ServiceUnavailable
    ));
    assert_eq!(
        err.message(),
        synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE
    );
}

#[test]
fn test_message_stream_user_lookup_backend_outage_stays_unavailable() {
    let status = map_api_error(synctv_api_common::impls::ApiError::from(
        synctv_core::Error::ServiceUnavailable("user backend unavailable".to_string()),
    ));
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.message(), "user backend unavailable");
    assert_eq!(detail_error_code(&status).as_deref(), Some("9002"));
}

#[test]
fn test_message_stream_room_lookup_not_found_stays_not_found() {
    let status = map_api_error(synctv_api_common::impls::ApiError::from(
        synctv_core::Error::NotFound("Room not found".to_string()),
    ));
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(status.message(), "Room not found");
    assert_eq!(detail_error_code(&status).as_deref(), Some("2000"));
}

#[test]
fn test_message_stream_direct_admission_errors_include_application_code() {
    let invalid = invalid_argument_status("Missing x-room-id header");
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert_eq!(detail_error_code(&invalid).as_deref(), Some("3000"));

    let unauthenticated = unauthenticated_status("Invalid authorization header");
    assert_eq!(unauthenticated.code(), tonic::Code::Unauthenticated);
    assert_eq!(detail_error_code(&unauthenticated).as_deref(), Some("1000"));

    let denied = permission_denied_status("This room has been banned");
    assert_eq!(denied.code(), tonic::Code::PermissionDenied);
    assert_eq!(detail_error_code(&denied).as_deref(), Some("4000"));

    let unavailable =
        unavailable_status("Real-time messaging requires realtime manager (Redis not configured)");
    assert_eq!(unavailable.code(), tonic::Code::Unavailable);
    assert_eq!(detail_error_code(&unavailable).as_deref(), Some("9002"));
}

#[test]
fn test_map_api_error_all_variants() {
    let variants: Vec<(synctv_api_common::impls::ApiError, tonic::Code)> = vec![
        (
            synctv_api_common::impls::ApiError::NotFound("x".into()),
            tonic::Code::NotFound,
        ),
        (
            synctv_api_common::impls::ApiError::Authentication("x".into()),
            tonic::Code::Unauthenticated,
        ),
        (
            synctv_api_common::impls::ApiError::Authorization("x".into()),
            tonic::Code::PermissionDenied,
        ),
        (
            synctv_api_common::impls::ApiError::AlreadyExists("x".into()),
            tonic::Code::AlreadyExists,
        ),
        (
            synctv_api_common::impls::ApiError::InvalidInput("x".into()),
            tonic::Code::InvalidArgument,
        ),
        (
            synctv_api_common::impls::ApiError::ServiceUnavailable("x".into()),
            tonic::Code::Unavailable,
        ),
        (
            synctv_api_common::impls::ApiError::Internal("x".into()),
            tonic::Code::Internal,
        ),
    ];
    for (err, expected_code) in variants {
        let status = map_api_error(err);
        assert_eq!(status.code(), expected_code);
    }
}

#[test]
fn test_grpc_message_sender_channel_closed() -> TestResult {
    let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(10);
    let sender = GrpcMessageSender::new(tx);
    drop(rx);

    let msg = ServerMessage::default();
    let result = MessageSender::send(&sender, msg);
    match result {
        Ok(()) => return Err(test_error("closed channel should fail")),
        Err(error) => assert!(error.contains("disconnected")),
    }
    Ok(())
}

#[test]
fn test_grpc_message_sender_channel_full() -> TestResult {
    let (tx, _rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
    let sender = GrpcMessageSender::new(tx);

    let msg1 = ServerMessage::default();
    assert!(MessageSender::send(&sender, msg1).is_ok());

    let msg2 = ServerMessage::default();
    let result = MessageSender::send(&sender, msg2);
    match result {
        Ok(()) => return Err(test_error("full channel should fail")),
        Err(error) => assert!(error.contains("full")),
    }
    Ok(())
}

#[test]
fn test_grpc_message_sender_is_alive_until_receiver_closes() {
    let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
    let sender = GrpcMessageSender::new(tx);

    assert!(
        sender.is_alive(),
        "open response channel must be reported alive"
    );
    drop(rx);
    assert!(
        !sender.is_alive(),
        "closed response channel must be reported dead immediately"
    );
}

#[tokio::test]
async fn test_await_grpc_receive_or_response_close_notices_closed_response_stream() {
    let (tx, rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
    drop(rx);

    let outcome = await_grpc_receive_or_response_close(
        std::future::pending::<Result<Option<ClientMessage>, tonic::Status>>(),
        tx,
    )
    .await;

    assert!(matches!(outcome, GrpcReceiveOutcome::ResponseStreamClosed));
}

#[tokio::test]
async fn test_await_grpc_receive_or_response_close_prefers_received_message() -> TestResult {
    let (tx, _rx) = tokio::sync::mpsc::channel::<ServerMessage>(1);
    let expected = ClientMessage::default();

    let outcome = await_grpc_receive_or_response_close(
        std::future::ready(Ok::<_, tonic::Status>(Some(expected.clone()))),
        tx,
    )
    .await;

    match outcome {
        GrpcReceiveOutcome::Message(Ok(Some(actual))) => assert_eq!(actual, expected),
        other => {
            return Err(test_error(format!(
                "expected received message outcome, got {other:?}"
            )));
        }
    }
    Ok(())
}

#[test]
fn test_map_message_stream_join_error_maps_capacity_to_resource_exhausted() {
    let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
        "realtime room capacity exceeded".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert_eq!(status.message(), "realtime room capacity exceeded");
}

#[test]
fn test_map_message_stream_join_error_maps_raw_capacity_error() {
    let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
        "Room at capacity (42 connections, max: 40)".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert_eq!(
        status.message(),
        "Room at capacity (42 connections, max: 40)"
    );
}

#[test]
fn test_map_message_stream_join_error_maps_raw_user_capacity_error() {
    let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
        "Too many connections for this user across all replicas (max 3)".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert_eq!(
        status.message(),
        "Too many connections for this user across all replicas (max 3)"
    );
}

#[test]
fn test_map_message_stream_join_error_maps_raw_total_capacity_error() {
    let status = map_message_stream_join_error(RealtimeJoinError::RateLimited(
        "Server at capacity across all replicas (42 connections)".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::ResourceExhausted);
    assert_eq!(
        status.message(),
        "Server at capacity across all replicas (42 connections)"
    );
}

#[test]
fn test_map_message_stream_join_error_maps_invalid_watch_cursor() {
    let status = map_message_stream_join_error(RealtimeJoinError::InvalidInput(
        "Invalid chat event cursor".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
    assert_eq!(status.message(), "Invalid chat event cursor");
}

#[test]
fn test_realtime_room_access_error_rejects_banned_room() -> TestResult {
    let mut room = Room::new("test-room".to_string(), UserId::new());
    room.ban();

    let status =
        realtime_room_access_error(&room).ok_or_else(|| test_error("banned room must fail"))?;
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    assert!(status.message().contains("banned"));
    Ok(())
}

#[test]
fn test_realtime_room_access_error_rejects_closed_room() -> TestResult {
    let mut room = Room::new("test-room".to_string(), UserId::new());
    room.status = synctv_core::models::RoomStatus::Closed;

    let status =
        realtime_room_access_error(&room).ok_or_else(|| test_error("closed room must fail"))?;
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    assert!(status.message().contains("not accepting new connections"));
    Ok(())
}

#[test]
fn test_realtime_room_access_error_allows_active_room() {
    let room = Room::new("test-room".to_string(), UserId::new());
    assert!(realtime_room_access_error(&room).is_none());
}

#[test]
fn test_map_message_stream_membership_error_backend_outage_stays_unavailable() {
    let status = map_message_stream_membership_error(synctv_core::Error::ServiceUnavailable(
        "membership backend unavailable".to_string(),
    ));

    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.message(), "membership backend unavailable");
}

#[test]
fn test_map_message_stream_membership_error_authorization_stays_permission_denied() {
    let status = map_message_stream_membership_error(synctv_core::Error::Authorization(
        "Not a member of this room".to_string(),
    ));

    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    assert_eq!(status.message(), "Forbidden: Not a member of this room");
}

#[test]
fn test_map_message_stream_join_error_maps_distributed_degradation_to_unavailable() {
    let status = map_message_stream_join_error(RealtimeJoinError::ServiceUnavailable(
        "distributed room capacity check unavailable".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(
        status.message(),
        "distributed room capacity check unavailable"
    );
}

#[test]
fn test_map_message_stream_join_error_maps_raw_degraded_cluster_error() {
    let status = map_message_stream_join_error(
        RealtimeJoinError::ServiceUnavailable(
            "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
                .to_string(),
        ),
    );
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(
        status.message(),
        "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
    );
}

#[test]
fn test_map_message_stream_join_error_maps_raw_degraded_user_check_error() {
    let status = map_message_stream_join_error(
        RealtimeJoinError::ServiceUnavailable(
            "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
                .to_string(),
        ),
    );
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(
        status.message(),
        "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
    );
}

#[test]
fn test_map_message_stream_join_error_maps_raw_degraded_total_check_error() {
    let status = map_message_stream_join_error(
        RealtimeJoinError::ServiceUnavailable(
            "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
                .to_string(),
        ),
    );
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(
        status.message(),
        "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
    );
}

#[test]
fn test_map_message_stream_join_error_maps_business_denial_to_permission_denied() {
    let status = map_message_stream_join_error(RealtimeJoinError::PermissionDenied(
        "User is no longer allowed to use real-time messaging".to_string(),
    ));

    assert_eq!(status.code(), tonic::Code::PermissionDenied);
    assert_eq!(
        status.message(),
        "User is no longer allowed to use real-time messaging"
    );
}

#[test]
fn test_map_message_stream_join_error_hides_unexpected_internal_details() {
    let status = map_message_stream_join_error(RealtimeJoinError::Internal(
        "Connection 'conn123' is already registered".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::Internal);
    assert_eq!(status.message(), "Internal error");
}
