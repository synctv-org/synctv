use super::*;
use crate::models::RoomPermissionSet;
use crate::test_helpers::failing_redis_runtime;
use async_trait::async_trait;

fn create_test_user_id(id: i64) -> UserId {
    UserId::expect_positive(id)
}

fn create_test_room_id(id: i64) -> RoomId {
    RoomId::expect_positive(id)
}

fn ok<T, E: std::fmt::Display>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

fn joined<T>(result: std::result::Result<T, tokio::task::JoinError>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error}")),
    }
}

#[tokio::test]
async fn test_redis_ticket_store_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let store = RedisTicketStore::from_runtime(runtime.clone(), "synctv:");

    assert!(
        Arc::ptr_eq(&store.redis_runtime, &runtime),
        "Redis ticket store should retain the injected runtime object"
    );
}

#[tokio::test]
async fn test_ws_ticket_service_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let service = WsTicketService::with_redis_runtime(runtime, "synctv:", Some(30));

    assert!(service.supports_cluster_runtime());
    assert_eq!(service.ticket_ttl_secs(), 30);
}

#[tokio::test]
async fn test_ws_ticket_service_supports_service_trait_object() {
    let service: Arc<dyn WebSocketTicketService> = Arc::new(WsTicketService::local_only(Some(30)));
    let user_id = create_test_user_id(50_001);
    let room_id = create_test_room_id(50_002);

    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 7).await,
        "trait-object service should create ticket",
    );
    let validated = ok(
        service.validate_and_consume(&ticket, &room_id).await,
        "trait-object service should validate ticket",
    );

    assert_eq!(some(validated.user_id(), "user ticket"), user_id);
    assert_eq!(some(validated.password_version(), "user ticket"), 7);
    assert!(!service.supports_cluster_runtime());
    assert_eq!(service.ticket_ttl_secs(), 30);
}

#[tokio::test]
async fn test_web_socket_ticket_shared_state_builder_returns_live_service() {
    let profile = SharedStateProfile::for_cluster_runtime(None, "trait-test:", false);
    let service = ok(
        WsTicketService::from_shared_state_profile(&profile, Some(30)),
        "standalone mode should allow local ticket storage",
    );
    let user_id = create_test_user_id(50_003);
    let room_id = create_test_room_id(50_004);

    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 9).await,
        "shared-state builder should return a live ticket service",
    );
    let validated = ok(
        service.validate_and_consume(&ticket, &room_id).await,
        "ticket created via shared-state builder should validate",
    );

    assert_eq!(some(validated.user_id(), "user ticket"), user_id);
    assert_eq!(some(validated.password_version(), "user ticket"), 9);
    assert!(!service.supports_cluster_runtime());
}

#[test]
fn test_web_socket_ticket_shared_state_builder_requires_shared_runtime_in_cluster_mode() {
    let profile = SharedStateProfile::for_cluster_runtime(None, "trait-test:", true);
    let Err(error) = WsTicketService::from_shared_state_profile(&profile, None) else {
        std::panic::panic_any("cluster runtime must reject local WebSocket ticket storage");
    };

    assert!(
        error
            .to_string()
            .contains("distributed runtime requires shared WebSocket ticket storage"),
        "unexpected error: {error}"
    );
}

#[test]
fn test_ticket_generation() {
    let ticket1 = WsTicketService::generate_ticket();
    let ticket2 = WsTicketService::generate_ticket();

    assert_ne!(ticket1, ticket2);
    assert!(!ticket1.contains('+'));
    assert!(!ticket1.contains('/'));
    assert!(!ticket1.contains('='));
}

#[tokio::test]
async fn test_ticket_service_memory_mode() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_005);
    let room_id = create_test_room_id(50_006);

    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 0).await,
        "ticket should be created",
    );

    let validated = ok(
        service.validate_and_consume(&ticket, &room_id).await,
        "ticket should validate",
    );
    assert_eq!(some(validated.user_id(), "user ticket"), user_id);
    assert_eq!(some(validated.password_version(), "user ticket"), 0);
}

#[tokio::test]
async fn test_ticket_one_time_use_memory_mode() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_007);
    let room_id = create_test_room_id(50_008);

    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 0).await,
        "ticket should be created",
    );

    let result1 = service.validate_and_consume(&ticket, &room_id).await;
    assert!(result1.is_ok());

    let result2 = service.validate_and_consume(&ticket, &room_id).await;
    assert!(
        matches!(result2, Err(Error::Authentication(_))),
        "consumed ticket should be treated as failed authentication"
    );
}

#[tokio::test]
async fn test_guest_ticket_service_memory_mode() {
    let service = WsTicketService::with_memory(Some(30));
    let room_id = create_test_room_id(50_039);
    let permissions = RoomPermissionSet::default_guest();

    let ticket = ok(
        service
            .create_guest_ticket_with_control(
                CreateGuestTicketRequest {
                    room_id,
                    guest_id: "guest_1".to_string(),
                    display_name: "Guest One".to_string(),
                    session_id: "session_1".to_string(),
                    token_jti: "jti_1".to_string(),
                    room_guest_version: 3,
                    permissions,
                },
                None,
            )
            .await,
        "guest ticket should be created",
    );

    let validated = ok(
        service.validate_and_consume(&ticket, &room_id).await,
        "guest ticket should validate",
    );

    match validated {
        ValidatedTicket::Guest(guest) => {
            assert_eq!(guest.guest_id, "guest_1");
            assert_eq!(guest.display_name, "Guest One");
            assert_eq!(guest.session_id, "session_1");
            assert_eq!(guest.token_jti, "jti_1");
            assert_eq!(guest.room_guest_version, 3);
            assert_eq!(guest.permissions, permissions);
        }
        ValidatedTicket::User { .. } => {
            std::panic::panic_any("guest ticket returned user principal");
        }
    }
}

#[tokio::test]
async fn test_guest_ticket_checked_validation_skips_user_validator() {
    let service = WsTicketService::with_memory(Some(30));
    let room_id = create_test_room_id(50_040);
    let rejecting_validator = StaticUserValidator {
        result: Err("user validator must not run for guests"),
    };

    let ticket = ok(
        service
            .create_guest_ticket_with_control(
                CreateGuestTicketRequest {
                    room_id,
                    guest_id: "guest_2".to_string(),
                    display_name: "Guest Two".to_string(),
                    session_id: "session_2".to_string(),
                    token_jti: "jti_2".to_string(),
                    room_guest_version: 4,
                    permissions: RoomPermissionSet::default_guest(),
                },
                None,
            )
            .await,
        "guest ticket should be created",
    );

    let pending = ok(
        service
            .validate_checked(&ticket, &room_id, &rejecting_validator)
            .await,
        "guest ticket should prevalidate without user validator",
    );
    assert!(matches!(pending, PendingValidatedTicket::Guest { .. }));

    let committed = ok(
        service
            .consume_prevalidated(&ticket, &room_id, &pending)
            .await,
        "guest ticket should consume after prevalidation",
    );
    assert!(matches!(committed, ValidatedTicket::Guest(_)));

    let consumed_again = service.validate_and_consume(&ticket, &room_id).await;
    assert!(
        matches!(consumed_again, Err(Error::Authentication(_))),
        "guest ticket must remain one-time-use"
    );
}

#[tokio::test]
async fn test_ticket_room_mismatch_rejected() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_009);
    let room_a = create_test_room_id(50_010);
    let room_b = create_test_room_id(50_011);

    let ticket = ok(
        service.create_ticket(&user_id, &room_a, 0).await,
        "ticket should be created",
    );

    let result = service.validate_and_consume(&ticket, &room_b).await;
    assert!(
        matches!(result, Err(Error::Authorization(_))),
        "Ticket for room A should not be valid for room B"
    );
}

struct StaticUserValidator {
    result: std::result::Result<UserValidationResult, &'static str>,
}

#[async_trait]
impl UserValidator for StaticUserValidator {
    async fn validate_for_ticket(&self, _user_id: &UserId) -> Result<UserValidationResult> {
        self.result.clone().map_err(|message| match message {
            "temporarily unavailable" => Error::ServiceUnavailable(message.to_string()),
            _ => Error::Authorization(message.to_string()),
        })
    }
}

#[tokio::test]
async fn test_ticket_room_mismatch_does_not_consume_ticket() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_012);
    let room_a = create_test_room_id(50_013);
    let room_b = create_test_room_id(50_014);

    let ticket = ok(
        service.create_ticket(&user_id, &room_a, 7).await,
        "ticket should be created",
    );

    let wrong_room_result = service.validate_and_consume(&ticket, &room_b).await;
    assert!(
        matches!(wrong_room_result, Err(Error::Authorization(_))),
        "room mismatch should be rejected"
    );

    let correct_room_result = service.validate_and_consume(&ticket, &room_a).await;
    assert!(
        correct_room_result.is_ok(),
        "room mismatch must not consume the ticket"
    );
    let validated = ok(correct_room_result, "correct room should validate");
    assert_eq!(some(validated.user_id(), "user ticket"), user_id);
    assert_eq!(some(validated.password_version(), "user ticket"), 7);
}

#[tokio::test]
async fn test_ticket_checked_room_mismatch_rejected_without_consuming_ticket() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_015);
    let room_a = create_test_room_id(50_016);
    let room_b = create_test_room_id(50_017);
    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 8,
        }),
    };

    let ticket = ok(
        service.create_ticket(&user_id, &room_a, 8).await,
        "ticket should be created",
    );

    let wrong_room_result = service
        .validate_checked(&ticket, &room_b, &allow_validator)
        .await;
    assert!(
        matches!(wrong_room_result, Err(Error::Authorization(_))),
        "checked prevalidation must reject tickets issued for another room"
    );

    let correct_room_result = service
        .validate_and_consume_checked(&ticket, &room_a, &allow_validator)
        .await;
    assert!(
        correct_room_result.is_ok(),
        "checked room mismatch must not consume the ticket"
    );
    let validated = ok(correct_room_result, "correct room should validate");
    assert_eq!(some(validated.user_id(), "user ticket"), user_id);
    assert_eq!(some(validated.password_version(), "user ticket"), 8);
}

#[tokio::test]
async fn test_ticket_prevalidated_commit_rechecks_room_binding() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_018);
    let room_a = create_test_room_id(50_019);
    let room_b = create_test_room_id(50_020);
    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 9,
        }),
    };

    let ticket = ok(
        service.create_ticket(&user_id, &room_a, 9).await,
        "ticket should be created",
    );
    let pending = ok(
        service
            .validate_checked(&ticket, &room_a, &allow_validator)
            .await,
        "ticket should prevalidate for its issuing room",
    );

    let wrong_room_commit = service
        .consume_prevalidated(&ticket, &room_b, &pending)
        .await;
    assert!(
        matches!(wrong_room_commit, Err(Error::Authorization(_))),
        "prevalidated commit must reject a different room"
    );

    let correct_room_commit = ok(
        service
            .consume_prevalidated(&ticket, &room_a, &pending)
            .await,
        "failed wrong-room commit must leave ticket claimable for the right room",
    );
    assert_eq!(some(correct_room_commit.user_id(), "user ticket"), user_id);
    assert_eq!(
        some(correct_room_commit.password_version(), "user ticket"),
        9
    );
}

#[tokio::test]
async fn test_ticket_user_validation_failure_does_not_consume_ticket() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_021);
    let room_id = create_test_room_id(50_022);
    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 4).await,
        "ticket should be created",
    );

    let rejecting_validator = StaticUserValidator {
        result: Err("banned"),
    };
    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 4,
        }),
    };

    let first_result = service
        .validate_and_consume_checked(&ticket, &room_id, &rejecting_validator)
        .await;
    assert!(
        matches!(first_result, Err(Error::Authentication(_))),
        "user validation failure should reject the ticket"
    );

    let second_result = service
        .validate_and_consume_checked(&ticket, &room_id, &allow_validator)
        .await;
    assert!(
        second_result.is_ok(),
        "user validation rejection must not consume the ticket"
    );
    let validated = ok(second_result, "second validation should succeed");
    assert_eq!(some(validated.user_id(), "user ticket"), user_id);
    assert_eq!(some(validated.password_version(), "user ticket"), 4);
}

#[tokio::test]
async fn test_ticket_user_validation_backend_outage_is_preserved_and_does_not_consume_ticket() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_023);
    let room_id = create_test_room_id(50_024);
    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 4).await,
        "ticket should be created",
    );

    let failing_validator = StaticUserValidator {
        result: Err("temporarily unavailable"),
    };
    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 4,
        }),
    };

    let first_result = service
        .validate_checked(&ticket, &room_id, &failing_validator)
        .await;
    assert!(
        matches!(first_result, Err(Error::ServiceUnavailable(ref msg)) if msg.contains("temporarily unavailable")),
        "backend outages must stay retryable, got: {first_result:?}"
    );

    let second_result = service
        .validate_and_consume_checked(&ticket, &room_id, &allow_validator)
        .await;
    assert!(
        second_result.is_ok(),
        "backend outages must not consume the ticket"
    );
    let validated = ok(second_result, "second validation should succeed");
    assert_eq!(some(validated.user_id(), "user ticket"), user_id);
    assert_eq!(some(validated.password_version(), "user ticket"), 4);
}

#[tokio::test]
async fn test_ticket_checked_validation_is_still_one_time_use() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_025);
    let room_id = create_test_room_id(50_026);
    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 2).await,
        "ticket should be created",
    );

    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 2,
        }),
    };

    let first_result = service
        .validate_and_consume_checked(&ticket, &room_id, &allow_validator)
        .await;
    assert!(
        first_result.is_ok(),
        "first checked validation should succeed"
    );

    let second_result = service
        .validate_and_consume_checked(&ticket, &room_id, &allow_validator)
        .await;
    assert!(
        matches!(second_result, Err(Error::Authentication(_))),
        "checked validation must still enforce one-time use"
    );
}

#[tokio::test]
async fn test_ticket_prevalidation_does_not_consume_until_commit() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_027);
    let room_id = create_test_room_id(50_028);
    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 5).await,
        "ticket should be created",
    );

    let allow_validator = StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 5,
        }),
    };

    ok(
        service
            .validate_checked(&ticket, &room_id, &allow_validator)
            .await,
        "prevalidation should succeed",
    );

    let still_valid = ok(
        service.validate_and_consume(&ticket, &room_id).await,
        "prevalidation alone must not consume the ticket",
    );
    assert_eq!(some(still_valid.user_id(), "user ticket"), user_id);
    assert_eq!(some(still_valid.password_version(), "user ticket"), 5);

    let second_ticket = ok(
        service.create_ticket(&user_id, &room_id, 5).await,
        "second ticket should be created",
    );
    let pending = ok(
        service
            .validate_checked(&second_ticket, &room_id, &allow_validator)
            .await,
        "second prevalidation should succeed",
    );
    let committed = ok(
        service
            .consume_prevalidated(&second_ticket, &room_id, &pending)
            .await,
        "commit should consume the prevalidated ticket",
    );
    assert_eq!(some(committed.user_id(), "user ticket"), user_id);

    let consumed_again = service.validate_and_consume(&second_ticket, &room_id).await;
    assert!(
        matches!(consumed_again, Err(Error::Authentication(_))),
        "committed prevalidated ticket must become one-time-use"
    );
}

#[tokio::test]
async fn test_ticket_checked_validation_concurrent_consumption_only_succeeds_once() {
    let service = WsTicketService::with_memory(Some(30));
    let user_id = create_test_user_id(50_029);
    let room_id = create_test_room_id(50_030);
    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 2).await,
        "ticket should be created",
    );

    let validator = Arc::new(StaticUserValidator {
        result: Ok(UserValidationResult {
            password_version: 2,
        }),
    });

    let mut handles = Vec::new();
    for _ in 0..8 {
        let service = service.clone();
        let ticket = ticket.clone();
        let validator = validator.clone();
        handles.push(tokio::spawn(async move {
            service
                .validate_and_consume_checked(&ticket, &room_id, &*validator)
                .await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|result| joined(result, "task should join"))
        .collect();

    let successes = results.iter().filter(|result| result.is_ok()).count();
    let failures = results.iter().filter(|result| result.is_err()).count();

    assert_eq!(successes, 1, "exactly one checked consume should succeed");
    assert_eq!(failures, 7, "all remaining concurrent consumers must fail");
}

#[tokio::test]
async fn test_in_memory_claim_mismatch_does_not_consume_ticket() {
    let room_id = create_test_room_id(50_031);
    let ticket = "ticket-claim";
    let store = InMemoryTicketStore::new();
    let user_id = create_test_user_id(50_032);
    let original = WsTicketData::user(&user_id, &room_id, 7);
    ok(
        store.store(ticket, &original, 30).await,
        "ticket should store",
    );

    let mut mismatched = original.clone();
    mismatched.created_at = mismatched.created_at.saturating_add(1);

    let first_claim = ok(
        store.claim(ticket, &mismatched).await,
        "mismatched claim should complete",
    );
    assert!(
        !first_claim,
        "claim with mismatched ticket data must fail without consuming the ticket"
    );

    let second_claim = ok(
        store.claim(ticket, &original).await,
        "original claim should complete",
    );
    assert!(
        second_claim,
        "ticket must remain claimable after a failed compare-and-delete attempt"
    );
}

#[tokio::test]
async fn test_ticket_expiration_memory_mode() {
    let service = WsTicketService::with_memory(Some(1)); // 1 second TTL
    let user_id = create_test_user_id(50_033);
    let room_id = create_test_room_id(50_034);

    let ticket = ok(
        service.create_ticket(&user_id, &room_id, 0).await,
        "ticket should be created",
    );

    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

    let result = service.validate_and_consume(&ticket, &room_id).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_invalid_ticket_memory_mode() {
    let service = WsTicketService::with_memory(Some(30));
    let room_id = create_test_room_id(50_035);

    let result = service
        .validate_and_consume("invalid_ticket", &room_id)
        .await;
    assert!(result.is_err());
}

#[tokio::test(start_paused = true)]
async fn test_ws_ticket_redis_timeout_maps_to_timeout_error() {
    let timeout_future = run_ws_ticket_redis_op(
        crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        "store ticket",
        async { std::future::pending::<std::result::Result<(), redis::RedisError>>().await },
    );

    tokio::pin!(timeout_future);
    tokio::task::yield_now().await;
    tokio::time::advance(crate::resilience::timeout::REDIS_OPERATION_TIMEOUT).await;

    let err = timeout_future.await.expect_err("operation should time out");
    assert!(matches!(
        err,
        Error::Timeout(ref msg) if msg == "Redis timeout: store ticket"
    ));
}

#[tokio::test]
async fn test_ws_ticket_redis_error_maps_to_service_unavailable() {
    let err = run_ws_ticket_redis_op::<(), _>(
        crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        "store ticket",
        async {
            Err::<(), redis::RedisError>(redis::RedisError::from((
                redis::ErrorKind::Io,
                "connection reset by peer",
            )))
        },
    )
    .await
    .expect_err("redis transport failures should stay retryable");

    assert!(matches!(
        err,
        Error::ServiceUnavailable(ref msg)
            if msg == "WebSocket ticket service is temporarily unavailable. Please try again later."
    ));
}

#[test]
fn test_non_cluster_mode_allows_memory() {
    let service = WsTicketService::local_only(None);
    assert!(!service.supports_cluster_runtime());
}

// Cluster mode Redis dependency tests.

/// Test: backend selection without Redis uses memory.
#[test]
fn test_new_without_redis_uses_memory_backend() {
    let service = WsTicketService::local_only(Some(30));
    assert!(!service.supports_cluster_runtime());
}

#[test]
fn test_new_without_redis_preserves_custom_ttl() {
    let service = WsTicketService::local_only(Some(60));
    assert_eq!(service.ticket_ttl_secs(), 60);
}

/// Test: non-distributed mode without Redis works but logs warning.
/// Single-replica deployments should still function without Redis.
#[test]
fn test_non_cluster_mode_without_redis_succeeds() {
    let service = WsTicketService::local_only(Some(30));
    assert!(
        !service.supports_cluster_runtime(),
        "Non-distributed mode without Redis should use single-node ticket storage"
    );
}

/// Test: `from_store` allows custom backends for testing purposes.
#[test]
fn test_from_store_allows_custom_backend() {
    let store = Arc::new(InMemoryTicketStore::new());
    let service = WsTicketService::from_store(store, Some(45));

    assert!(!service.supports_cluster_runtime());
    assert_eq!(service.ticket_ttl_secs(), 45);
}
