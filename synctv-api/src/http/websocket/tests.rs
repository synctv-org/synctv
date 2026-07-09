use super::*;
use std::sync::Arc;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{RoomId, UserId},
    service::{
        BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RoomService, UserService,
        UserServiceRuntimeOptions, UserValidationResult, UserValidator,
    },
};
use synctv_realtime::sync::{ConnectionLimits, ConnectionManager};

type TestResult<T = ()> = anyhow::Result<T>;

fn test_error(message: impl Into<String>) -> anyhow::Error {
    anyhow::anyhow!(message.into())
}

fn app_ok<T>(result: Result<T, AppError>) -> TestResult<T> {
    result.map_err(|error| test_error(format!("{error:?}")))
}

fn app_err<T>(result: Result<T, AppError>) -> TestResult<AppError> {
    match result {
        Ok(_) => Err(test_error("expected HTTP app error")),
        Err(error) => Ok(error),
    }
}

struct AllowAllTicketValidator;

#[async_trait::async_trait]
impl UserValidator for AllowAllTicketValidator {
    async fn validate_for_ticket(
        &self,
        _user_id: &UserId,
    ) -> synctv_core::Result<UserValidationResult> {
        Ok(UserValidationResult {
            password_version: 0,
        })
    }
}

fn test_user_service(pool: &sqlx::PgPool) -> TestResult<UserService> {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!")?;
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));

    Ok(UserService::new_with_runtime(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test".to_string()),
        UserServiceRuntimeOptions {
            password_registration_policy_override: Some(synctv_core::service::RegistrationPolicy {
                enabled: true,
                need_review: false,
            }),
            ..synctv_core::service::UserServiceRuntimeOptions::test_defaults()
        },
    ))
}

fn test_room_service(pool: &sqlx::PgPool) -> TestResult<RoomService> {
    RoomService::new_for_tests(pool.clone(), test_user_service(pool)?)
        .map_err(|error| test_error(error.to_string()))
}

async fn register_test_user(
    user_service: &UserService,
    username: &str,
    email: &str,
) -> TestResult<synctv_core::models::User> {
    Ok(synctv_core_testing::opaque_register_user(
        user_service,
        username,
        Some(email.to_string()),
        "Password123!",
    )
    .await
    .map_err(|error| test_error(error.to_string()))?
    .0)
}

#[test]
fn test_ws_query_no_auth() {
    let query = WsQuery {
        ticket: String::new(),
        ..Default::default()
    };
    assert!(query.ticket.is_empty());
}

#[test]
fn test_ws_query_with_ticket() {
    let query = WsQuery {
        ticket: "ticket_abc".to_string(),
        ..Default::default()
    };
    assert_eq!(query.ticket, "ticket_abc");
}

#[test]
fn test_realtime_transport_format_defaults_to_json() -> TestResult {
    assert_eq!(
        app_ok(RealtimeTransportFormat::parse(None))?,
        RealtimeTransportFormat::Json
    );
    assert_eq!(
        app_ok(RealtimeTransportFormat::parse(Some("")))?,
        RealtimeTransportFormat::Json
    );
    Ok(())
}

#[test]
fn test_realtime_transport_format_accepts_protobuf() -> TestResult {
    assert_eq!(
        app_ok(RealtimeTransportFormat::parse(Some("protobuf")))?,
        RealtimeTransportFormat::Protobuf
    );
    Ok(())
}

#[test]
fn test_realtime_transport_format_rejects_unknown_values() -> TestResult {
    let err = app_err(RealtimeTransportFormat::parse(Some("xml")))?;
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(err.message().contains("Invalid format"));

    let err = app_err(RealtimeTransportFormat::parse(Some("proto")))?;
    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(err.message().contains("Invalid format"));
    Ok(())
}

#[test]
fn test_websocket_request_metadata_uses_forwarded_ip_for_trusted_proxy() -> TestResult {
    let mut config = crate::ApiRuntimeSettings::default();
    config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

    let mut headers = HeaderMap::new();
    headers.insert("x-forwarded-for", "203.0.113.50".parse()?);

    let metadata = app_ok(websocket_request_metadata(
        &config,
        &headers,
        Some("127.0.0.1".parse()?),
    ))?;

    assert_eq!(metadata.client_ip, Some("203.0.113.50".parse()?));
    Ok(())
}

#[test]
fn test_websocket_request_metadata_rejects_non_utf8_user_agent() -> TestResult {
    let config = crate::ApiRuntimeSettings::default();
    let mut headers = HeaderMap::new();
    headers.insert(
        header::USER_AGENT,
        axum::http::HeaderValue::from_bytes(&[0xff])?,
    );

    let err = app_err(websocket_request_metadata(&config, &headers, None))?;

    assert_eq!(err.status(), StatusCode::BAD_REQUEST);
    assert!(err.message().contains("Invalid user-agent header"));
    Ok(())
}

#[test]
fn test_websocket_content_filter_reuses_shared_filter() -> TestResult {
    let shared = Arc::new(ContentFilter::new_with_config(
        17,
        Some(vec!["blocked".to_string()]),
        false,
    ));
    let selected = websocket_content_filter(&shared);
    assert!(
        Arc::ptr_eq(&selected, &shared),
        "websocket path must reuse the shared ContentFilter instance"
    );
    assert_eq!(selected.max_chat_length, 17);
    assert_eq!(
        selected
            .filter_chat("<b>hi</b>")
            .map_err(|error| test_error(error.to_string()))?,
        "<b>hi</b>",
        "websocket path must reuse the shared filter config instead of default strip_html=true"
    );
    Ok(())
}

#[test]
fn test_ws_query_deserialization_empty() -> TestResult {
    let json = "{}";
    let query: WsQuery = serde_json::from_str(json)?;
    assert!(query.ticket.is_empty());
    Ok(())
}

#[test]
fn test_ws_query_deserialization_with_ticket() -> TestResult {
    let json = r#"{"ticket":"my_ticket"}"#;
    let query: WsQuery = serde_json::from_str(json)?;
    assert_eq!(query.ticket, "my_ticket");
    Ok(())
}

#[test]
fn test_ws_query_deserialization_ignores_extra_fields() -> TestResult {
    let json = r#"{"ticket":"tix","extra":"ignored"}"#;
    let query: WsQuery = serde_json::from_str(json)?;
    assert_eq!(query.ticket, "tix");
    Ok(())
}

#[test]
fn test_resource_event_is_critical() {
    let message = ServerMessage {
        message: Some(
            synctv_proto::client::server_message::Message::ResourceEvent(
                synctv_proto::client::ResourceEvent {
                    observe_id: "playlist_items".to_string(),
                    payload: Some(synctv_proto::client::resource_event::Payload::ChangedOnly(
                        synctv_proto::client::ResourceEventOnly {},
                    )),
                    event_cursor: None,
                },
            ),
        ),
    };

    assert!(is_critical_message(&message));
    assert_eq!(message_type_name(&message), "ResourceEvent");
}

#[test]
fn test_notification_requires_state_resync() {
    let message = ServerMessage {
        message: Some(synctv_proto::client::server_message::Message::Notification(
            synctv_proto::client::UserNotification {
                notification_id: "ntf_test".to_string(),
                notification_type: synctv_proto::client::NotificationType::SystemAnnouncement
                    as i32,
                title: "Title".to_string(),
                content: "Content".to_string(),
                data: Some(synctv_proto::client::NotificationData::default()),
                timestamp: 1,
            },
        )),
    };

    assert!(requires_state_resync(&message));
    assert_eq!(message_type_name(&message), "Notification");
}

#[test]
fn test_auth_priority_header_present_in_header_map() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("Authorization", "Bearer some_jwt_token".parse()?);

    let auth_header = headers.get("Authorization");
    assert!(auth_header.is_some());
    let auth_str = auth_header
        .ok_or_else(|| test_error("Authorization header should be present"))?
        .to_str()?;
    assert!(auth_str.starts_with("Bearer "));
    let token = auth_str
        .strip_prefix("Bearer ")
        .ok_or_else(|| test_error("Authorization header should use Bearer scheme"))?;
    assert_eq!(token, "some_jwt_token");
    Ok(())
}

#[test]
fn test_auth_priority_no_bearer_prefix_in_header() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert("Authorization", "Basic dXNlcjpwYXNz".parse()?);

    let auth_header = headers
        .get("Authorization")
        .ok_or_else(|| test_error("Authorization header should be present"))?;
    let auth_str = auth_header.to_str()?;
    assert!(auth_str.strip_prefix("Bearer ").is_none());
    Ok(())
}

#[test]
fn test_auth_priority_invalid_utf8_header_is_not_ignored() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        axum::http::HeaderValue::from_bytes(b"Bearer \xFFinvalid")?,
    );

    let err = app_err(extract_authorization_bearer_token(&headers))?;
    assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert!(err.message().contains("non-UTF-8"));
    Ok(())
}

#[test]
fn test_auth_priority_no_header_falls_through_to_ticket() {
    let headers = HeaderMap::new();
    let query = WsQuery {
        ticket: "ticket_abc".to_string(),
        ..Default::default()
    };

    assert!(headers.get("Authorization").is_none());
    assert!(!query.ticket.is_empty());
}

#[test]
fn test_auth_priority_no_auth_at_all() {
    let headers = HeaderMap::new();
    let query = WsQuery {
        ticket: String::new(),
        ..Default::default()
    };

    assert!(headers.get("Authorization").is_none());
    assert!(query.ticket.is_empty());
}

#[test]
fn test_unauthorized_error_for_missing_auth() {
    let err = AppError::unauthorized(
        "Missing authentication: provide token via Authorization header or ?ticket=",
    );
    assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert!(err.message().contains("Missing authentication"));
}

#[test]
fn test_forbidden_error_for_non_member() {
    let err = AppError::forbidden("Not a member of this room");
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
}

#[test]
fn test_unauthorized_error_for_revoked_token() {
    let err = AppError::unauthorized("Token has been revoked");
    assert_eq!(err.status(), axum::http::StatusCode::UNAUTHORIZED);
    assert_eq!(err.message(), "Token has been revoked");
}

#[test]
fn test_validate_websocket_origin_allows_missing_origin_for_non_browser_clients() -> TestResult {
    let headers = HeaderMap::new();
    let config = crate::ApiRuntimeSettings::default();
    app_ok(validate_websocket_origin(
        &headers,
        &[],
        None,
        &config.server,
    ))?;
    Ok(())
}

#[test]
fn test_validate_websocket_origin_allows_same_origin_host_when_explicitly_allowlisted() -> TestResult
{
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "app.example.com".parse()?);
    headers.insert(header::ORIGIN, "https://app.example.com".parse()?);

    app_ok(validate_websocket_origin(
        &headers,
        &["https://app.example.com".to_string()],
        None,
        &crate::ApiRuntimeSettings::default().server,
    ))?;
    Ok(())
}

#[test]
fn test_validate_websocket_origin_allows_same_origin_host_without_explicit_allowlist() -> TestResult
{
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "app.example.com".parse()?);
    headers.insert(header::ORIGIN, "https://app.example.com".parse()?);

    app_ok(validate_websocket_origin(
        &headers,
        &[],
        None,
        &crate::ApiRuntimeSettings::default().server,
    ))?;
    Ok(())
}

#[test]
fn test_validate_websocket_origin_allows_explicitly_configured_cross_origin() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "api.example.com".parse()?);
    headers.insert(header::ORIGIN, "https://app.example.com".parse()?);

    app_ok(validate_websocket_origin(
        &headers,
        &["https://app.example.com".to_string()],
        None,
        &crate::ApiRuntimeSettings::default().server,
    ))?;
    Ok(())
}

#[test]
fn test_validate_websocket_origin_rejects_same_host_with_mismatched_scheme() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "app.example.com".parse()?);
    headers.insert(header::ORIGIN, "http://app.example.com".parse()?);
    headers.insert("x-forwarded-proto", "https".parse()?);
    let mut config = crate::ApiRuntimeSettings::default();
    config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

    let err = app_err(validate_websocket_origin(
        &headers,
        &[],
        Some("127.0.0.1".parse()?),
        &config.server,
    ))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    Ok(())
}

#[test]
fn test_validate_websocket_origin_rejects_non_utf8_host() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        axum::http::HeaderValue::from_bytes(b"app.example.com\xff")?,
    );
    headers.insert(header::ORIGIN, "https://app.example.com".parse()?);

    let err = app_err(validate_websocket_origin(
        &headers,
        &[],
        None,
        &crate::ApiRuntimeSettings::default().server,
    ))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    assert!(err.message().contains("Host"));
    Ok(())
}

#[test]
fn test_validate_websocket_origin_rejects_malformed_host_port() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "app.example.com:bad".parse()?);
    headers.insert(header::ORIGIN, "https://app.example.com".parse()?);

    let err = app_err(validate_websocket_origin(
        &headers,
        &[],
        None,
        &crate::ApiRuntimeSettings::default().server,
    ))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    assert!(err.message().contains("Host"));
    Ok(())
}

#[test]
fn test_split_host_and_port_rejects_malformed_ipv6_host_header() -> TestResult {
    let err = app_err(split_host_and_port("[::1:8080"))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    assert!(err.message().contains("Host"));
    Ok(())
}

#[test]
fn test_validate_websocket_origin_rejects_non_utf8_forwarded_proto_from_trusted_proxy() -> TestResult
{
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "app.example.com".parse()?);
    headers.insert(header::ORIGIN, "https://app.example.com".parse()?);
    headers.insert(
        "x-forwarded-proto",
        axum::http::HeaderValue::from_bytes(b"https\xff")?,
    );
    let mut config = crate::ApiRuntimeSettings::default();
    config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

    let err = app_err(validate_websocket_origin(
        &headers,
        &[],
        Some("127.0.0.1".parse()?),
        &config.server,
    ))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    assert!(err.message().contains("x-forwarded-proto"));
    Ok(())
}

#[test]
fn test_validate_websocket_origin_ignores_forwarded_proto_from_untrusted_peer() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "app.example.com".parse()?);
    headers.insert(header::ORIGIN, "http://app.example.com".parse()?);
    headers.insert("x-forwarded-proto", "https".parse()?);
    let mut config = crate::ApiRuntimeSettings::default();
    config.server.trusted_proxies = vec!["127.0.0.1".to_string()];

    app_ok(validate_websocket_origin(
        &headers,
        &[],
        Some("198.51.100.10".parse()?),
        &config.server,
    ))?;
    Ok(())
}

#[test]
fn test_validate_websocket_origin_uses_direct_peer_for_trusted_proxy_forwarded_proto() -> TestResult
{
    let mut config = crate::ApiRuntimeSettings::default();
    config.server.trusted_proxies = vec!["10.0.0.0/8".to_string()];

    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "app.example.com".parse()?);
    headers.insert(header::ORIGIN, "http://app.example.com".parse()?);
    headers.insert("x-forwarded-for", "203.0.113.10".parse()?);
    headers.insert("x-forwarded-proto", "https".parse()?);

    let direct_peer_ip = "10.2.3.4".parse()?;
    let resolved_client_ip = synctv_adapter::client_ip::extract_client_ip_from_headers(
        |ip| config.server.is_trusted_proxy(ip),
        direct_peer_ip,
        &headers,
    )?;
    assert_eq!(
        resolved_client_ip,
        "203.0.113.10".parse::<std::net::IpAddr>()?
    );

    let err = app_err(validate_websocket_origin(
        &headers,
        &[],
        Some(direct_peer_ip),
        &config.server,
    ))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);

    app_ok(validate_websocket_origin(
        &headers,
        &[],
        Some(resolved_client_ip),
        &config.server,
    ))?;
    Ok(())
}

#[test]
fn test_split_host_and_port_supports_ipv6_host_header() -> TestResult {
    let (host, port) = app_ok(split_host_and_port("[::1]:8080"))?;
    assert_eq!(host, "::1");
    assert_eq!(port, Some(8080));
    Ok(())
}

#[test]
fn test_validate_websocket_origin_rejects_unconfigured_cross_origin() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "api.example.com".parse()?);
    headers.insert(header::ORIGIN, "https://evil.example.com".parse()?);

    let err = app_err(validate_websocket_origin(
        &headers,
        &[],
        None,
        &crate::ApiRuntimeSettings::default().server,
    ))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    assert!(err.message().contains("Origin"));
    Ok(())
}

#[test]
fn test_validate_websocket_origin_rejects_null_origin() -> TestResult {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "api.example.com".parse()?);
    headers.insert(header::ORIGIN, "null".parse()?);

    let err = app_err(validate_websocket_origin(
        &headers,
        &[],
        None,
        &crate::ApiRuntimeSettings::default().server,
    ))?;
    assert_eq!(err.status(), axum::http::StatusCode::FORBIDDEN);
    Ok(())
}

#[test]
fn test_validate_websocket_runtime_dependency_flags_require_realtime_and_chat_services(
) -> TestResult {
    let err = app_err(validate_websocket_runtime_dependency_flags(false))?;
    assert_eq!(err.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);

    app_ok(validate_websocket_runtime_dependency_flags(true))?;
    Ok(())
}

#[test]
fn test_websocket_handshake_timeout_matches_global_http_timeout_budget() {
    assert_eq!(
        WEBSOCKET_HANDSHAKE_TIMEOUT,
        Duration::from_secs(30),
        "websocket handshake timeout should match the HTTP request timeout budget"
    );
}

#[tokio::test(start_paused = true)]
async fn test_websocket_handshake_timeout_returns_request_timeout_error() -> TestResult {
    let handshake = async { std::future::pending::<Result<(), AppError>>().await };

    let timeout_task =
        tokio::spawn(async move { run_websocket_handshake_with_timeout(handshake).await });

    tokio::time::advance(WEBSOCKET_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;

    let err = app_err(timeout_task.await?)?;

    assert_eq!(err.status(), StatusCode::REQUEST_TIMEOUT);
    assert_eq!(err.message(), "WebSocket handshake timed out");
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn test_handshake_timeout_releases_reserved_capacity_without_marking_presence() -> TestResult
{
    let manager = Arc::new(ConnectionManager::new(ConnectionLimits {
        max_per_room: 1,
        max_per_user: 1,
        ..ConnectionLimits::default()
    }));
    let user_id = UserId::expect_positive(130_001);
    let room_id = RoomId::expect_positive(130_002);
    let reservation = HandshakeReservation { room_id, user_id };

    manager
        .reserve_user_slot(&user_id)
        .map_err(|error| test_error(error.clone()))?;
    manager
        .reserve_room_slot(&room_id)
        .map_err(|error| test_error(error.clone()))?;

    assert!(
        manager.get_connection_id(&room_id, &user_id).is_none(),
        "reserved handshakes must not appear as active presence"
    );
    assert!(
        manager.reserve_user_slot(&user_id).is_err(),
        "user handshake reservations should remain full while the reservation is active"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_err(),
        "room handshake reservations should remain full while the reservation is active"
    );

    let handshake_manager = manager.clone();
    let handshake = async move {
        let _cleanup = ReservationCleanupGuard::new(handshake_manager, reservation);
        std::future::pending::<Result<(), AppError>>().await
    };

    let timeout_task =
        tokio::spawn(async move { run_websocket_handshake_with_timeout(handshake).await });

    tokio::time::advance(WEBSOCKET_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;

    let err = app_err(timeout_task.await?)?;
    assert_eq!(err.status(), StatusCode::REQUEST_TIMEOUT);

    assert!(
        manager.reserve_user_slot(&user_id).is_ok(),
        "timeout cleanup should free user reservation capacity"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_ok(),
        "timeout cleanup should free room reservation capacity"
    );
    Ok(())
}

#[test]
fn test_room_not_found_maps_to_not_found_error_during_websocket_prepare() {
    let err = AppError::from(synctv_core::Error::NotFound("Room not found".to_string()));
    assert_eq!(err.status(), StatusCode::NOT_FOUND);
    assert_eq!(err.message(), "Room not found");
}

#[tokio::test]
async fn test_load_websocket_username_fails_closed_on_storage_error() {
    let state = crate::http::tests::test_app_state();
    state.user_service.pool().close().await;
    let user_id = UserId::expect_positive(130_003);

    let err = load_websocket_username(&state, &user_id)
        .await
        .expect_err("username lookup infrastructure failures must fail closed");

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message().contains("temporarily unavailable"),
        "username lookup outages should surface as retryable handshake failures"
    );
}

#[test]
fn test_map_security_pipeline_error_maps_backend_outages_to_service_unavailable() {
    let err = map_security_pipeline_error(synctv_core::Error::ServiceUnavailable(
        "Authentication service temporarily unavailable".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message().contains("temporarily unavailable"),
        "websocket auth backend outages should remain retryable"
    );
}

#[test]
fn test_map_websocket_ticket_validation_error_preserves_backend_outages() {
    let err = map_websocket_ticket_validation_error(synctv_core::Error::ServiceUnavailable(
        "Authentication service temporarily unavailable".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message().contains("temporarily unavailable"),
        "ticket validation outages should not be collapsed into invalid-ticket 401s"
    );
}

#[test]
fn test_map_websocket_ticket_validation_error_keeps_invalid_ticket_as_401() {
    let err = map_websocket_ticket_validation_error(synctv_core::Error::Authentication(
        "Invalid or expired ticket".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(err.message(), "Invalid or expired ticket");
}

#[test]
fn test_map_websocket_ticket_validation_error_keeps_room_mismatch_as_403() {
    let err = map_websocket_ticket_validation_error(synctv_core::Error::Authorization(
        "Ticket not valid for this room".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::FORBIDDEN);
    assert_eq!(err.message(), "Ticket not valid for this room");
}

#[test]
fn test_map_websocket_ticket_validation_api_error_keeps_ticket_semantics() {
    let invalid = map_websocket_ticket_validation_api_error(synctv_core::Error::Authentication(
        "invalid".to_string(),
    ));
    assert!(
        matches!(invalid, ApiError::Authentication(message) if message == "Invalid or expired ticket")
    );

    let mismatch = map_websocket_ticket_validation_api_error(synctv_core::Error::Authorization(
        "Ticket not valid for this room".to_string(),
    ));
    assert!(
        matches!(mismatch, ApiError::Authorization(message) if message == "Ticket not valid for this room")
    );

    let outage = map_websocket_ticket_validation_api_error(synctv_core::Error::ServiceUnavailable(
        "Authentication service temporarily unavailable".to_string(),
    ));
    assert!(
        matches!(outage, ApiError::ServiceUnavailable(message) if message.contains("temporarily unavailable"))
    );
}

#[test]
fn test_map_websocket_membership_probe_error_preserves_backend_outages() {
    let err = map_websocket_membership_probe_error(synctv_core::Error::ServiceUnavailable(
        "membership backend temporarily unavailable".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message().contains("temporarily unavailable"),
        "websocket membership probe outages should remain retryable"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_validate_websocket_room_membership_rejects_room_with_inactive_creator() -> TestResult
{
    let (_container, pool) = synctv_core_testing::create_test_pool().await;
    let room_service = test_room_service(&pool)?;
    let user_service = room_service.user_service().clone();

    let owner = register_test_user(
        &user_service,
        "ws-owner-inactive",
        "ws-owner-inactive@test.invalid",
    )
    .await?;
    let member = register_test_user(
        &user_service,
        "ws-member-inactive-owner",
        "ws-member-inactive-owner@test.invalid",
    )
    .await?;

    let room = room_service
        .create_room(
            "ws-room-inactive-owner".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .map_err(|error| test_error(error.to_string()))?
        .0;
    room_service
        .join_room(room.id, member.id, None)
        .await
        .map_err(|error| test_error(error.to_string()))?;

    synctv_core::repository::UserRepository::new(pool.clone())
        .ban(&owner.id, None, Some("websocket test".to_string()))
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let Err(err) = validate_websocket_room_membership(&room_service, &room, &member.id).await
    else {
        return Err(test_error(
            "expected inactive creator to reject websocket prepare",
        ));
    };

    assert_eq!(err.status(), StatusCode::FORBIDDEN);
    assert!(
        err.message().contains("creator is not active"),
        "expected creator-inactive error, got: {}",
        err.message()
    );

    pool.close().await;
    Ok(())
}

#[test]
fn test_map_websocket_pre_join_error_maps_typed_rate_limit_prefix() {
    let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
        "realtime room capacity exceeded".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(err.message(), "realtime room capacity exceeded");
}

#[test]
fn test_map_websocket_pre_join_error_maps_raw_capacity_error() {
    let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
        "Room at capacity (42 connections, max: 40)".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(err.message(), "Room at capacity (42 connections, max: 40)");
}

#[test]
fn test_map_websocket_pre_join_error_maps_raw_user_capacity_error() {
    let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
        "Too many connections for this user across all replicas (max 3)".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        err.message(),
        "Too many connections for this user across all replicas (max 3)"
    );
}

#[test]
fn test_map_websocket_pre_join_error_maps_raw_total_capacity_error() {
    let err = map_websocket_pre_join_error(RealtimeJoinError::RateLimited(
        "Server at capacity across all replicas (42 connections)".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        err.message(),
        "Server at capacity across all replicas (42 connections)"
    );
}

#[test]
fn test_map_websocket_pre_join_error_maps_typed_service_unavailable_prefix() {
    let err = map_websocket_pre_join_error(RealtimeJoinError::ServiceUnavailable(
        "distributed room capacity check unavailable".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message()
            .contains("distributed room capacity check unavailable"),
        "typed service-unavailable pre-join failures should preserve retryable cause"
    );
}

#[test]
fn test_map_websocket_pre_join_error_maps_raw_degraded_cluster_error() {
    let err = map_websocket_pre_join_error(
        RealtimeJoinError::ServiceUnavailable(
            "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
                .to_string(),
        ),
    );

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message().contains("cluster Redis is degraded"),
        "raw degraded-cluster pre-join failures should preserve retryable cause"
    );
}

#[test]
fn test_map_websocket_pre_join_error_maps_raw_degraded_user_check_error() {
    let err = map_websocket_pre_join_error(
        RealtimeJoinError::ServiceUnavailable(
            "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
                .to_string(),
        ),
    );

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message().contains("cluster Redis is degraded"),
        "raw degraded user-check failures should preserve retryable cause"
    );
}

#[test]
fn test_map_websocket_pre_join_error_maps_raw_degraded_total_check_error() {
    let err = map_websocket_pre_join_error(
        RealtimeJoinError::ServiceUnavailable(
            "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
                .to_string(),
        ),
    );

    assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(
        err.message().contains("cluster Redis is degraded"),
        "raw degraded total-check failures should preserve retryable cause"
    );
}

#[test]
fn test_map_websocket_pre_join_error_maps_business_denial_to_forbidden() {
    let err = map_websocket_pre_join_error(RealtimeJoinError::PermissionDenied(
        "User is no longer allowed to use real-time messaging".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        err.message(),
        "User is no longer allowed to use real-time messaging"
    );
}

#[test]
fn test_map_websocket_pre_join_error_hides_unexpected_internal_details() {
    let err = map_websocket_pre_join_error(RealtimeJoinError::Internal(
        "cluster subscription cache blew up".to_string(),
    ));

    assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(err.message(), "Internal error");
}

#[tokio::test]
async fn test_failed_upgrade_cleanup_releases_reserved_capacity_without_presence() -> TestResult {
    let manager = Arc::new(ConnectionManager::new(ConnectionLimits {
        max_per_room: 1,
        max_per_user: 1,
        ..ConnectionLimits::default()
    }));
    let user_id = UserId::expect_positive(130_004);
    let room_id = RoomId::expect_positive(130_005);
    let reservation = HandshakeReservation { room_id, user_id };

    manager
        .reserve_user_slot(&user_id)
        .map_err(|error| test_error(error.clone()))?;
    manager
        .reserve_room_slot(&room_id)
        .map_err(|error| test_error(error.clone()))?;

    assert!(
        manager.get_connection_id(&room_id, &user_id).is_none(),
        "failed upgrades must not leave a visible active connection"
    );
    assert!(
        manager.reserve_user_slot(&user_id).is_err(),
        "user handshake reservations should remain full while the upgrade reservation is active"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_err(),
        "room handshake reservations should remain full while the upgrade reservation is active"
    );

    let cleanup = build_failed_upgrade_cleanup(manager.clone(), reservation);
    cleanup(axum::Error::new(std::io::Error::other("upgrade failed")));

    assert!(
        manager.reserve_user_slot(&user_id).is_ok(),
        "cleanup should free user reservation capacity"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_ok(),
        "cleanup should free room reservation capacity"
    );
    Ok(())
}

#[tokio::test]
async fn test_failed_upgrade_cleanup_leaves_consumed_ticket_spent() -> TestResult {
    let state = crate::http::tests::test_app_state();
    let ws_ticket_service = state.ws_ticket_service.clone();
    let user_id = UserId::expect_positive(130_006);
    let room_id = RoomId::expect_positive(130_007);
    let reservation = HandshakeReservation { room_id, user_id };

    state
        .router_options
        .connection_manager
        .reserve_user_slot(&user_id)
        .map_err(|error| test_error(error.clone()))?;
    state
        .router_options
        .connection_manager
        .reserve_room_slot(&room_id)
        .map_err(|error| test_error(error.clone()))?;

    let ticket = ws_ticket_service
        .create_ticket(&user_id, &room_id, 0)
        .await
        .map_err(|error| test_error(error.to_string()))?;
    let pending = ws_ticket_service
        .validate_checked(&ticket, &room_id, &AllowAllTicketValidator)
        .await
        .map_err(|error| test_error(error.to_string()))?;
    let prepared = PreparedWebSocketUpgrade {
        room_id,
        auth: HandshakeAuthContext {
            user_id,
            principal: RealtimePrincipal::user(user_id, "ticket-user".to_string()),
            ticket_commit: Some(TicketAuthCommit {
                ticket: ticket.clone(),
                pending,
            }),
        },
        username: "ticket-user".to_string(),
        connection_id: "conn-ticket-restore".to_string(),
        reservation: reservation.clone(),
    };
    let handshake_control = ExecutionControl::default();

    commit_websocket_upgrade(&state, prepared, &handshake_control)
        .await
        .map_err(|error| test_error(format!("{error:?}")))?;

    let cleanup =
        build_failed_upgrade_cleanup(state.router_options.connection_manager.clone(), reservation);
    cleanup(axum::Error::new(std::io::Error::other("upgrade failed")));

    let validated = ws_ticket_service
        .validate_and_consume(&ticket, &room_id)
        .await;
    assert!(
        validated.is_err(),
        "failed upgrade cleanup must not resurrect a one-time ticket after the HTTP handshake succeeded"
    );
    Ok(())
}

#[tokio::test]
async fn test_commit_websocket_upgrade_releases_reservation_when_ticket_claim_fails() -> TestResult
{
    let state = crate::http::tests::test_app_state();
    let ws_ticket_service = state.ws_ticket_service.clone();
    let user_id = UserId::expect_positive(130_008);
    let room_id = RoomId::expect_positive(130_009);

    let reservation = reserve_websocket_upgrade_slots(
        state.router_options.connection_manager.as_ref(),
        &room_id,
        &user_id,
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    let ticket = ws_ticket_service
        .create_ticket(&user_id, &room_id, 0)
        .await
        .map_err(|error| test_error(error.to_string()))?;
    let pending = ws_ticket_service
        .validate_checked(&ticket, &room_id, &AllowAllTicketValidator)
        .await
        .map_err(|error| test_error(error.to_string()))?;

    ws_ticket_service
        .consume_prevalidated(&ticket, &room_id, &pending)
        .await
        .map_err(|error| test_error(error.to_string()))?;

    let prepared = PreparedWebSocketUpgrade {
        room_id,
        auth: HandshakeAuthContext {
            user_id,
            principal: RealtimePrincipal::user(user_id, "ticket-user".to_string()),
            ticket_commit: Some(TicketAuthCommit { ticket, pending }),
        },
        username: "ticket-user".to_string(),
        connection_id: "conn-ticket-claim-fail".to_string(),
        reservation,
    };
    let handshake_control = ExecutionControl::default();

    let error = app_err(commit_websocket_upgrade(&state, prepared, &handshake_control).await)?;
    assert_eq!(error.status(), StatusCode::UNAUTHORIZED);

    state
        .router_options
        .connection_manager
        .reserve_user_slot(&user_id)
        .map_err(|error| test_error(error.clone()))?;
    state
        .router_options
        .connection_manager
        .reserve_room_slot(&room_id)
        .map_err(|error| test_error(error.clone()))?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn test_commit_websocket_upgrade_releases_reservation_when_timeout_cancels_commit(
) -> TestResult {
    let state = crate::http::tests::test_app_state();
    let timeout_state = state.clone();
    let user_id = UserId::expect_positive(130_010);
    let room_id = RoomId::expect_positive(130_011);
    let reservation = reserve_websocket_upgrade_slots(
        state.router_options.connection_manager.as_ref(),
        &room_id,
        &user_id,
    )
    .map_err(|error| test_error(format!("{error:?}")))?;

    let prepared = PreparedWebSocketUpgrade {
        room_id,
        auth: HandshakeAuthContext {
            user_id,
            principal: RealtimePrincipal::user(user_id, "ticket-user".to_string()),
            ticket_commit: None,
        },
        username: "ticket-user".to_string(),
        connection_id: "conn-ticket-timeout".to_string(),
        reservation,
    };
    let handshake_control = ExecutionControl::default();

    let timeout_task = tokio::spawn(async move {
        run_websocket_handshake_with_timeout(async move {
            let prepared =
                commit_websocket_upgrade(&timeout_state, prepared, &handshake_control).await?;
            drop(prepared);
            std::future::pending::<Result<PreparedWebSocketUpgrade, AppError>>().await
        })
        .await
    });

    tokio::time::advance(WEBSOCKET_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;

    let err = app_err(timeout_task.await?)?;
    assert_eq!(err.status(), StatusCode::REQUEST_TIMEOUT);

    state
        .router_options
        .connection_manager
        .reserve_user_slot(&user_id)
        .map_err(|error| test_error(error.clone()))?;
    state
        .router_options
        .connection_manager
        .reserve_room_slot(&room_id)
        .map_err(|error| test_error(error.clone()))?;
    Ok(())
}

#[tokio::test]
async fn test_reservation_stays_full_until_connection_pre_join_succeeds() -> TestResult {
    let manager = Arc::new(ConnectionManager::new(ConnectionLimits {
        max_per_room: 1,
        max_per_user: 1,
        ..ConnectionLimits::default()
    }));
    let user_id = UserId::expect_positive(130_012);
    let room_id = RoomId::expect_positive(130_013);
    let reservation = HandshakeReservation { room_id, user_id };
    let connection_id = "conn-pre-join-transfer".to_string();

    manager
        .reserve_user_slot(&user_id)
        .map_err(|error| test_error(error.clone()))?;
    manager
        .reserve_room_slot(&room_id)
        .map_err(|error| test_error(error.clone()))?;

    assert!(
        manager.reserve_user_slot(&user_id).is_err(),
        "user capacity must remain full while only the handshake reservation exists"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_err(),
        "room capacity must remain full while only the handshake reservation exists"
    );

    manager
        .register(connection_id.clone(), user_id)
        .await
        .map_err(|error| test_error(error.clone()))?;
    manager
        .join_room(&connection_id, room_id)
        .await
        .map_err(|error| test_error(error.clone()))?;

    assert!(
        manager.reserve_user_slot(&user_id).is_err(),
        "active registration must keep user capacity full before reservation release"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_err(),
        "active room membership must keep room capacity full before reservation release"
    );

    reservation.release(manager.as_ref());

    assert!(
        manager.reserve_user_slot(&user_id).is_err(),
        "releasing the handshake reservation must not free capacity still used by the active connection"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_err(),
        "releasing the handshake reservation must not free room capacity still used by the active connection"
    );

    manager.unregister(&connection_id).await;

    assert!(
        manager.reserve_user_slot(&user_id).is_ok(),
        "capacity should reopen only after the active connection leaves"
    );
    assert!(
        manager.reserve_room_slot(&room_id).is_ok(),
        "room capacity should reopen only after the active connection leaves"
    );
    Ok(())
}

#[test]
fn test_state_resync_messages_disconnect_slow_client_immediately() -> TestResult {
    use crate::impls::messaging::MessageSender;
    use synctv_proto::client::{server_message::Message, ServerMessage, UserNotification};

    let (critical_tx, _critical_rx) = tokio::sync::mpsc::channel(1);
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let sender =
        WebSocketMessageSender::new(tx.clone(), critical_tx, RealtimeTransportFormat::Protobuf);

    tx.try_send(axum::extract::ws::Message::Text("occupied".into()))
        .map_err(|error| test_error(error.to_string()))?;

    let result = sender.send(ServerMessage {
        message: Some(Message::Notification(UserNotification {
            notification_id: "ntf_test".to_string(),
            notification_type: synctv_proto::client::NotificationType::SystemAnnouncement as i32,
            title: "Title".to_string(),
            content: "Content".to_string(),
            data: Some(synctv_proto::client::NotificationData::default()),
            timestamp: 1,
        })),
    });

    let err = match result {
        Ok(()) => return Err(test_error("expected slow client disconnect error")),
        Err(error) => error,
    };
    assert!(err.contains("stateful message"));
    assert!(err.contains("Notification"));
    Ok(())
}

#[test]
fn test_critical_messages_bypass_full_normal_queue() -> TestResult {
    use crate::impls::messaging::MessageSender;
    use synctv_proto::client::{server_message::Message, ErrorMessage, ServerMessage};

    let (critical_tx, mut critical_rx) = tokio::sync::mpsc::channel(1);
    let (tx, _rx) = tokio::sync::mpsc::channel(1);
    let sender =
        WebSocketMessageSender::new(tx.clone(), critical_tx, RealtimeTransportFormat::Protobuf);

    tx.try_send(axum::extract::ws::Message::Text("occupied".into()))
        .map_err(|error| test_error(error.to_string()))?;

    let result = sender.send(ServerMessage {
        message: Some(Message::Error(ErrorMessage {
            message: "critical".to_string(),
            code: synctv_proto::common::ErrorCode::Forbidden as i32,
            detail: String::new(),
        })),
    });

    assert!(
        result.is_ok(),
        "critical websocket messages must still enqueue when the normal queue is full"
    );
    assert!(
        critical_rx.try_recv().is_ok(),
        "critical message should be queued on the dedicated critical channel"
    );
    Ok(())
}

#[tokio::test]
async fn test_forward_websocket_messages_disconnects_connection_on_sink_failure() -> TestResult {
    use axum::Error;
    use futures::task::{Context, Poll};
    use std::pin::Pin;

    struct FailingSink;

    impl futures::Sink<axum::extract::ws::Message> for FailingSink {
        type Error = Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(
            self: Pin<&mut Self>,
            _item: axum::extract::ws::Message,
        ) -> Result<(), Self::Error> {
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Err(Error::new(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "synthetic sink failure",
            ))))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    let connection_id = "conn-forward-failure".to_string();
    let user_id = UserId::expect_positive(130_014);
    let manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    manager
        .register(connection_id.clone(), user_id)
        .await
        .map_err(|error| test_error(error.clone()))?;

    let mut disconnect_rx = manager.subscribe_disconnect();
    let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (critical_tx, critical_rx) = tokio::sync::mpsc::channel(1);
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    tx.send(axum::extract::ws::Message::Text("payload".into()))
        .await
        .map_err(|error| test_error(error.to_string()))?;
    drop(tx);
    drop(critical_tx);

    forward_websocket_messages(
        critical_rx,
        rx,
        FailingSink,
        is_alive.clone(),
        manager,
        connection_id.clone(),
    )
    .await;

    assert!(
        !is_alive.load(std::sync::atomic::Ordering::Relaxed),
        "sink failure must mark the connection dead immediately"
    );

    let signal = tokio::time::timeout(std::time::Duration::from_secs(1), disconnect_rx.recv())
        .await?
        .map_err(|error| test_error(error.to_string()))?;

    match signal {
        synctv_realtime::sync::DisconnectSignal::Connection(id) => {
            assert_eq!(id, connection_id);
        }
        other => {
            return Err(test_error(format!(
                "expected connection disconnect signal, got {other:?}"
            )));
        }
    }
    Ok(())
}

#[tokio::test]
async fn test_forward_websocket_messages_prioritizes_critical_queue() -> TestResult {
    use axum::Error;
    use futures::task::{Context, Poll};
    use std::pin::Pin;

    #[derive(Default)]
    struct RecordingSink {
        sent: Vec<String>,
    }

    impl futures::Sink<axum::extract::ws::Message> for RecordingSink {
        type Error = Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(
            mut self: Pin<&mut Self>,
            item: axum::extract::ws::Message,
        ) -> Result<(), Self::Error> {
            let label = match item {
                axum::extract::ws::Message::Text(text) => text.to_string(),
                other => format!("{other:?}"),
            };
            self.sent.push(label);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    let manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (critical_tx, critical_rx) = tokio::sync::mpsc::channel(2);
    let (tx, rx) = tokio::sync::mpsc::channel(2);

    tx.send(axum::extract::ws::Message::Text("normal".into()))
        .await
        .map_err(|error| test_error(error.to_string()))?;
    critical_tx
        .send(axum::extract::ws::Message::Text("critical".into()))
        .await
        .map_err(|error| test_error(error.to_string()))?;
    drop(tx);
    drop(critical_tx);

    let mut sink = RecordingSink::default();
    forward_websocket_messages(
        critical_rx,
        rx,
        &mut sink,
        is_alive,
        manager,
        "conn-priority".to_string(),
    )
    .await;

    assert_eq!(
        sink.sent,
        vec!["critical".to_string(), "normal".to_string()],
        "critical websocket queue must be drained before best-effort backlog"
    );
    Ok(())
}

#[tokio::test]
async fn test_forward_websocket_messages_prevents_normal_queue_starvation() -> TestResult {
    use axum::Error;
    use futures::task::{Context, Poll};
    use std::pin::Pin;

    #[derive(Default)]
    struct RecordingSink {
        sent: Vec<String>,
    }

    impl futures::Sink<axum::extract::ws::Message> for RecordingSink {
        type Error = Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn start_send(
            mut self: Pin<&mut Self>,
            item: axum::extract::ws::Message,
        ) -> Result<(), Self::Error> {
            let label = match item {
                axum::extract::ws::Message::Text(text) => text.to_string(),
                other => format!("{other:?}"),
            };
            self.sent.push(label);
            Ok(())
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            Poll::Ready(Ok(()))
        }
    }

    let manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    let is_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (critical_tx, critical_rx) = tokio::sync::mpsc::channel(8);
    let (tx, rx) = tokio::sync::mpsc::channel(8);

    for idx in 0..3 {
        critical_tx
            .send(axum::extract::ws::Message::Text(
                format!("critical-{idx}").into(),
            ))
            .await
            .map_err(|error| test_error(error.to_string()))?;
    }
    tx.send(axum::extract::ws::Message::Text("normal".into()))
        .await
        .map_err(|error| test_error(error.to_string()))?;
    for idx in 3..6 {
        critical_tx
            .send(axum::extract::ws::Message::Text(
                format!("critical-{idx}").into(),
            ))
            .await
            .map_err(|error| test_error(error.to_string()))?;
    }
    drop(tx);
    drop(critical_tx);

    let mut sink = RecordingSink::default();
    forward_websocket_messages(
        critical_rx,
        rx,
        &mut sink,
        is_alive,
        manager,
        "conn-fairness".to_string(),
    )
    .await;

    let normal_index = sink
        .sent
        .iter()
        .position(|message| message == "normal")
        .ok_or_else(|| test_error("normal queue message must be forwarded"))?;

    assert!(
        normal_index < 4,
        "normal queue should not starve behind sustained critical traffic: {:?}",
        sink.sent
    );
    Ok(())
}
