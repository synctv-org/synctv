use super::streaming::{
    await_grpc_receive_or_response_close, GrpcReceiveOutcome, MESSAGE_STREAM_BUFFER_SIZE,
};
use super::*;
use synctv_core::models::UserId;
use synctv_proto::client::{ClientMessage, ServerMessage};

type TestResult<T = ()> = anyhow::Result<T>;

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn metadata_error_code(status: &Status) -> Option<&str> {
    status
        .metadata()
        .get(crate::grpc_support::ERROR_CODE_METADATA_KEY)
        .and_then(|value| value.to_str().ok())
}

#[test]
fn test_map_api_error_not_found() {
    let err = crate::impls::ApiError::NotFound("room not found".to_string());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert!(status.message().contains("not found"));
}

#[test]
fn test_map_api_error_unauthenticated() {
    let err = crate::impls::ApiError::Authentication("invalid token".to_string());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[test]
fn test_map_api_error_permission_denied() {
    let err = crate::impls::ApiError::Authorization("forbidden".to_string());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::PermissionDenied);
}

#[test]
fn test_map_api_error_already_exists() {
    let err = crate::impls::ApiError::AlreadyExists("user exists".to_string());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::AlreadyExists);
}

#[test]
fn test_map_api_error_invalid_argument() {
    let err = crate::impls::ApiError::InvalidInput("bad input".to_string());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::InvalidArgument);
}

#[test]
fn test_map_api_error_internal_hides_details() {
    let err = crate::impls::ApiError::Internal("secret DB password=abc123".to_string());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::Internal);
    // Internal errors should NOT leak implementation details
    assert_eq!(status.message(), "Internal error");
    assert!(!status.message().contains("password"));
    assert!(!status.message().contains("secret"));
}

#[test]
fn test_create_publish_key_grpc_maps_service_unavailable() {
    let err = crate::impls::ApiError::ServiceUnavailable("publish key backend unavailable".into());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.message(), "publish key backend unavailable");
}

#[test]
fn test_get_stream_info_grpc_maps_not_found() {
    let err = crate::impls::ApiError::NotFound("stream not found".into());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(status.message(), "stream not found");
}

#[test]
fn test_list_room_streams_grpc_maps_service_unavailable() {
    let err = crate::impls::ApiError::ServiceUnavailable("livestream registry unavailable".into());
    let status = map_api_error(err);
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.message(), "livestream registry unavailable");
}

#[test]
fn test_request_password_reset_grpc_maps_service_unavailable() {
    let err =
        crate::impls::ApiError::ServiceUnavailable("password reset backend unavailable".into());
    let status = map_email_flow_error(err);
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.message(), "password reset backend unavailable");
}

#[test]
fn test_email_api_missing_maps_to_service_unavailable() {
    let err = ClientServiceImpl::email_api_unavailable_error();
    assert!(matches!(
        err.classify(),
        crate::impls::ErrorKind::ServiceUnavailable
    ));
    assert_eq!(
        err.message(),
        synctv_common::messages::EMAIL_SERVICE_UNAVAILABLE
    );
}

#[test]
fn test_message_stream_user_lookup_backend_outage_stays_unavailable() {
    let status = map_message_stream_user_lookup_error(synctv_core::Error::ServiceUnavailable(
        "user backend unavailable".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::Unavailable);
    assert_eq!(status.message(), "user backend unavailable");
    assert_eq!(metadata_error_code(&status), Some("9002"));
}

#[test]
fn test_message_stream_room_lookup_not_found_stays_not_found() {
    let status = map_message_stream_room_lookup_error(synctv_core::Error::NotFound(
        "Room not found".to_string(),
    ));
    assert_eq!(status.code(), tonic::Code::NotFound);
    assert_eq!(status.message(), "Room not found");
    assert_eq!(metadata_error_code(&status), Some("2000"));
}

#[test]
fn test_message_stream_direct_admission_errors_include_application_code() {
    let invalid = invalid_argument_status("Missing x-room-id header");
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert_eq!(metadata_error_code(&invalid), Some("3000"));

    let unauthenticated = unauthenticated_status("Invalid authorization header");
    assert_eq!(unauthenticated.code(), tonic::Code::Unauthenticated);
    assert_eq!(metadata_error_code(&unauthenticated), Some("1000"));

    let denied = permission_denied_status("This room has been banned");
    assert_eq!(denied.code(), tonic::Code::PermissionDenied);
    assert_eq!(metadata_error_code(&denied), Some("4000"));

    let unavailable =
        unavailable_status("Real-time messaging requires realtime manager (Redis not configured)");
    assert_eq!(unavailable.code(), tonic::Code::Unavailable);
    assert_eq!(metadata_error_code(&unavailable), Some("9002"));
}

#[test]
fn test_map_api_error_all_variants() {
    let variants: Vec<(crate::impls::ApiError, tonic::Code)> = vec![
        (
            crate::impls::ApiError::NotFound("x".into()),
            tonic::Code::NotFound,
        ),
        (
            crate::impls::ApiError::Authentication("x".into()),
            tonic::Code::Unauthenticated,
        ),
        (
            crate::impls::ApiError::Authorization("x".into()),
            tonic::Code::PermissionDenied,
        ),
        (
            crate::impls::ApiError::AlreadyExists("x".into()),
            tonic::Code::AlreadyExists,
        ),
        (
            crate::impls::ApiError::InvalidInput("x".into()),
            tonic::Code::InvalidArgument,
        ),
        (
            crate::impls::ApiError::ServiceUnavailable("x".into()),
            tonic::Code::Unavailable,
        ),
        (
            crate::impls::ApiError::Internal("x".into()),
            tonic::Code::Internal,
        ),
    ];
    for (err, expected_code) in variants {
        let status = map_api_error(err);
        assert_eq!(status.code(), expected_code);
    }
}

#[test]
fn test_grpc_message_sender_send_success() {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<ServerMessage>(10);
    let sender = GrpcMessageSender::new(tx);

    let msg = ServerMessage::default();
    let result = MessageSender::send(&sender, msg);
    assert!(result.is_ok());

    // Verify message was received
    let received = rx.try_recv();
    assert!(received.is_ok());
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
fn test_message_stream_buffer_size_reasonable() {
    // Buffer should be at least 10 and at most 1000
    const { assert!(MESSAGE_STREAM_BUFFER_SIZE >= 10) };
    const { assert!(MESSAGE_STREAM_BUFFER_SIZE <= 1000) };
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
