use super::*;
use crate::service::auth::JwtService;
use crate::test_helpers::failing_redis_runtime;
use async_trait::async_trait;
use std::time::Duration;

fn create_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-publish-key-tests-long-enough-1234567890").unwrap()
}

#[tokio::test]
async fn test_redis_jti_store_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let store = RedisJtiStore::from_runtime(runtime.clone(), "synctv:".to_string(), 3600);

    assert!(
        Arc::ptr_eq(&store.redis_runtime, &runtime),
        "Redis JTI store should retain the injected runtime object"
    );
}

#[tokio::test]
async fn test_redis_jti_store_snapshot_timeout_fails_closed() {
    #[derive(Clone)]
    struct HangingRedisRuntime;

    #[async_trait]
    impl RedisConnectionRuntime for HangingRedisRuntime {
        async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
            tokio::time::sleep(Duration::from_mins(1)).await;
            panic!("snapshot timeout should cancel this future")
        }

        fn operation_timeout(&self) -> Duration {
            Duration::from_millis(1)
        }
    }

    let store = RedisJtiStore::from_runtime_fail_closed(
        Arc::new(HangingRedisRuntime),
        "synctv:".to_string(),
        3600,
    );

    let error = store
        .try_claim("jti-timeout", 60)
        .await
        .expect_err("fail-closed publish-key JTI store should reject Redis timeouts");

    assert!(
        matches!(error, Error::Timeout(ref msg) if msg == "Redis timeout: claim publish-key JTI"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn test_publish_key_service_supports_service_trait_object() {
    let service: Arc<dyn StreamingPublishKeyService> = Arc::new(
        PublishKeyService::new(create_jwt_service(), 24).expect("publish key service should build"),
    );
    let room_id = RoomId::expect_positive(40_001);
    let media_id = MediaId::expect_positive(40_002);
    let user_id = UserId::expect_positive(40_003);

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .expect("trait-object publish key service should generate key");
    let claims = service
        .validate_publish_key_for_stream_claims(&key.token, &room_id, &media_id)
        .await
        .expect("trait-object publish key service should validate key");

    assert_eq!(claims.room_id, room_id.to_string());
    assert_eq!(claims.media_id, media_id.to_string());
    assert_eq!(claims.user_id, user_id.to_string());
}

#[tokio::test]
async fn test_streaming_publish_key_service_from_shared_state_profile_returns_live_trait_object() {
    let jwt = create_jwt_service();
    let profile = SharedStateProfile::from_runtime(None, "trait-test:", false);
    let service = streaming_publish_key_service_from_shared_state_profile(jwt, 12, &profile)
        .expect("standalone mode should allow local publish-key service");
    let room_id = RoomId::expect_positive(40_004);
    let media_id = MediaId::expect_positive(40_005);
    let user_id = UserId::expect_positive(40_006);

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .expect("builder should return a live publish-key service");
    let claims = service
        .validate_publish_key_for_stream_claims(&key.token, &room_id, &media_id)
        .await
        .expect("generated key should validate through the trait object");

    assert_eq!(claims.room_id, room_id.to_string());
    assert_eq!(claims.media_id, media_id.to_string());
    assert_eq!(claims.user_id, user_id.to_string());
}

#[test]
fn test_streaming_publish_key_service_from_shared_state_profile_requires_shared_runtime_in_cluster_mode(
) {
    let jwt = create_jwt_service();
    let profile = SharedStateProfile::from_runtime(None, "trait-test:", true);
    let Err(error) = streaming_publish_key_service_from_shared_state_profile(jwt, 12, &profile)
    else {
        panic!("cluster runtime must reject local publish-key deduplication");
    };

    assert!(
        error
            .to_string()
            .contains("distributed runtime requires shared publish-key deduplication state"),
        "unexpected error: {error}"
    );
}

fn create_publish_key_service() -> PublishKeyService {
    let jwt = create_jwt_service();
    PublishKeyService::new(jwt, 24).expect("publish key service should build")
}

fn create_publish_key_service_with_ttl(ttl_hours: i64) -> PublishKeyService {
    let jwt = create_jwt_service();
    PublishKeyService::new(jwt, ttl_hours).expect("publish key service should build")
}

#[tokio::test]
async fn test_generate_publish_key_returns_valid_token() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    assert!(!key.token.is_empty());
    assert_eq!(key.room_id, room_id.to_string());
    assert_eq!(key.media_id, media_id.to_string());
    assert_eq!(key.user_id, user_id.to_string());
    assert!(key.expires_at > 0);
}

#[tokio::test]
async fn test_generate_publish_key_expiration_matches_ttl() {
    let service = create_publish_key_service_with_ttl(2);
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let now = unix_timestamp_now().unwrap();

    let expected_exp = now + (2 * 3600);
    let diff = (key.expires_at - expected_exp).abs();
    assert!(
        diff < 5,
        "Expiration time is off by more than 5 seconds: diff={diff}"
    );
}

#[tokio::test]
async fn test_validate_publish_key_valid_token() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let claims = service.validate_publish_key(&key.token).await.unwrap();

    assert_eq!(claims.room_id, room_id.to_string());
    assert_eq!(claims.media_id, media_id.to_string());
    assert_eq!(claims.user_id, user_id.to_string());
    assert!(claims.perm_live_control);
}

#[tokio::test]
async fn test_validate_publish_key_invalid_token() {
    let service = create_publish_key_service();
    let result = service.validate_publish_key("invalid.token.here").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_validate_publish_key_rejects_expired_token() {
    let jwt_service = create_jwt_service();
    let service =
        PublishKeyService::new(jwt_service.clone(), 24).expect("publish key service should build");
    let now = unix_timestamp_now().unwrap();

    let token = jwt_service
        .sign_custom(&serde_json::json!({
            "room_id": RoomId::new().to_string(),
            "media_id": MediaId::new().to_string(),
            "user_id": UserId::new().to_string(),
            "perm_live_control": true,
            "iat": now - 7200,
            "exp": now - 3600,
            "jti": "expired_publish_key_test",
        }))
        .unwrap();

    let result = service.validate_publish_key(&token).await;

    assert!(
        matches!(result, Err(Error::Authentication(ref message)) if message.to_ascii_lowercase().contains("expired")),
        "expired publish key should be rejected, got: {result:?}"
    );
}

#[tokio::test]
async fn test_validate_publish_key_wrong_secret() {
    let service1 = create_publish_key_service();
    let service2 = PublishKeyService::new(
        JwtService::new("different-secret-key-for-tests-abcdef-long-enough-1234567890").unwrap(),
        24,
    )
    .expect("publish key service should build");

    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service1
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let result = service2.validate_publish_key(&key.token).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_verify_publish_key_for_stream_matching() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let returned_user_id = service
        .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
        .await
        .unwrap();

    assert_eq!(returned_user_id, user_id);
}

#[tokio::test]
async fn test_verify_publish_key_for_stream_wrong_room() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();
    let wrong_room_id = RoomId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let result = service
        .verify_publish_key_for_stream(&key.token, &wrong_room_id, &media_id)
        .await;
    assert!(result.is_err());
    if let Err(Error::Authorization(msg)) = result {
        assert!(msg.contains("room mismatch"));
    } else {
        panic!("Expected Authorization error with room mismatch");
    }
}

#[tokio::test]
async fn test_verify_publish_key_for_stream_wrong_media() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();
    let wrong_media_id = MediaId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let result = service
        .verify_publish_key_for_stream(&key.token, &room_id, &wrong_media_id)
        .await;
    assert!(result.is_err());
    if let Err(Error::Authorization(msg)) = result {
        assert!(msg.contains("media mismatch"));
    } else {
        panic!("Expected Authorization error with media mismatch");
    }
}

#[tokio::test]
async fn test_verify_publish_key_room_mismatch_does_not_consume_token() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();
    let wrong_room_id = RoomId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let first_attempt = service
        .verify_publish_key_for_stream(&key.token, &wrong_room_id, &media_id)
        .await;
    assert!(
        matches!(first_attempt, Err(Error::Authorization(_))),
        "room mismatch should reject without consuming the token"
    );

    let second_attempt = service
        .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
        .await;
    assert!(
        second_attempt.is_ok(),
        "room mismatch must not consume an otherwise valid publish key"
    );
    assert_eq!(second_attempt.unwrap(), user_id);
}

#[tokio::test]
async fn test_verify_publish_key_media_mismatch_does_not_consume_token() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();
    let wrong_media_id = MediaId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let first_attempt = service
        .verify_publish_key_for_stream(&key.token, &room_id, &wrong_media_id)
        .await;
    assert!(
        matches!(first_attempt, Err(Error::Authorization(_))),
        "media mismatch should reject without consuming the token"
    );

    let second_attempt = service
        .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
        .await;
    assert!(
        second_attempt.is_ok(),
        "media mismatch must not consume an otherwise valid publish key"
    );
    assert_eq!(second_attempt.unwrap(), user_id);
}

#[test]
fn test_publish_claims_require_live_control_claim_name() {
    let old_claim = serde_json::json!({
        "room_id": "room123",
        "media_id": "media456",
        "user_id": "user789",
        "perm_start_live": true,
        "iat": 1000,
        "exp": 2000,
        "jti": "unique-id",
    });

    let error = serde_json::from_value::<PublishClaims>(old_claim)
        .expect_err("perm_start_live is not a supported publish claim");
    assert!(
        error.to_string().contains("perm_live_control"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn test_validate_publish_key_single_use() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let result = service.validate_publish_key(&key.token).await;
    assert!(result.is_ok());

    let result = service.validate_publish_key(&key.token).await;
    assert!(result.is_err());
    if let Err(Error::Authentication(msg)) = result {
        assert!(
            msg.contains("single-use"),
            "Expected single-use error, got: {msg}"
        );
    } else {
        panic!("Expected Authentication error for replay");
    }
}

#[tokio::test]
async fn test_generate_publish_key_unique_jti() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key1 = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();
    let key2 = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    assert_ne!(key1.token, key2.token);
}

#[tokio::test]
async fn test_in_memory_jti_store_is_local_only() {
    let store = InMemoryJtiStore::new(3600);
    assert!(!store.supports_cross_node_single_use());
    assert!(!store.fail_closed());
}

#[test]
fn test_publish_key_service_debug_reports_capabilities_not_backend_names() {
    let service =
        PublishKeyService::new(create_jwt_service(), 24).expect("publish key service should build");
    let debug = format!("{service:?}");

    assert!(debug.contains("cross_node_single_use: false"));
    assert!(debug.contains("fail_closed: false"));
    assert!(!debug.contains("memory"));
    assert!(!debug.contains("redis"));
    assert!(!debug.contains("backend"));
}

#[tokio::test]
async fn test_in_memory_jti_store_claim_and_reject() {
    let store = InMemoryJtiStore::new(3600);

    assert!(store.try_claim("jti-1", 3600).await.unwrap());
    assert!(!store.try_claim("jti-1", 3600).await.unwrap());
    assert!(store.try_claim("jti-2", 3600).await.unwrap());

    assert!(store.is_claimed("jti-1").await);
    assert!(store.is_claimed("jti-2").await);
    assert!(!store.is_claimed("jti-3").await);
}

#[tokio::test]
async fn test_publish_key_service_from_store_custom_backend() {
    let store = Arc::new(InMemoryJtiStore::new(3600));
    let jwt = create_jwt_service();
    let service = PublishKeyService::from_store(jwt, 12, store);

    let debug = format!("{service:?}");
    assert!(debug.contains("12"));
    assert!(debug.contains("cross_node_single_use: false"));
    assert!(debug.contains("fail_closed: false"));
    assert!(!debug.contains("memory"));
}

#[tokio::test]
async fn test_in_memory_jti_store_concurrent_try_claim_only_one_succeeds() {
    let store = Arc::new(InMemoryJtiStore::new(3600));
    let jti = "concurrent-jti-test";
    let num_tasks = 50;

    let mut handles = Vec::with_capacity(num_tasks);
    for _ in 0..num_tasks {
        let store = store.clone();
        let jti = jti.to_string();
        handles.push(tokio::spawn(async move {
            store.try_claim(&jti, 3600).await.unwrap()
        }));
    }

    let mut success_count = 0u32;
    for handle in handles {
        if handle.await.unwrap() {
            success_count += 1;
        }
    }

    assert_eq!(
        success_count, 1,
        "Exactly one concurrent try_claim should succeed, but {success_count} succeeded"
    );
}

#[tokio::test]
async fn test_validate_publish_key_rejects_banned_user() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let result = service
        .verify_publish_key_for_stream_checked(&key.token, &room_id, &media_id, |_uid| {
            Err(Error::Authorization("User is banned".to_string()))
        })
        .await;

    assert!(result.is_err(), "Should reject banned user");
    if let Err(Error::Authorization(msg)) = &result {
        assert!(
            msg.contains("banned"),
            "Error should mention ban; got: {msg}"
        );
    } else {
        panic!("Expected Authorization error, got: {result:?}");
    }
}

#[tokio::test]
async fn test_validate_publish_key_user_validator_failure_does_not_consume_token() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let first_attempt = service
        .verify_publish_key_for_stream_checked(&key.token, &room_id, &media_id, |_uid| {
            Err(Error::Authorization("user banned".to_string()))
        })
        .await;
    assert!(
        matches!(first_attempt, Err(Error::Authorization(_))),
        "validator failure should reject the token"
    );

    let second_attempt = service
        .verify_publish_key_for_stream_checked(&key.token, &room_id, &media_id, |_uid| Ok(()))
        .await;
    assert!(
        second_attempt.is_ok(),
        "validator failure should leave the token reusable"
    );
    assert_eq!(second_attempt.unwrap(), user_id);
}

#[tokio::test]
async fn test_validate_publish_key_accepts_active_user() {
    let service = create_publish_key_service();
    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let result = service
        .verify_publish_key_for_stream_checked(&key.token, &room_id, &media_id, |_uid| Ok(()))
        .await;

    assert!(result.is_ok(), "Should accept active user");
    assert_eq!(result.unwrap(), user_id);
}

#[tokio::test]
async fn test_fail_closed_jti_store_rejects_on_backend_failure() {
    let store = FailClosedJtiStore;
    let result = store.try_claim("some-jti", 3600).await;
    assert!(
        result.is_err(),
        "fail_closed store should return Err on backend failure"
    );
}

struct FailClosedJtiStore;

#[async_trait]
impl JtiStore for FailClosedJtiStore {
    async fn try_claim(&self, _jti: &str, _ttl_secs: u64) -> Result<bool> {
        Err(Error::Internal(
            "Redis unavailable and fail_closed is enabled".to_string(),
        ))
    }
    async fn is_claimed(&self, _jti: &str) -> bool {
        false
    }

    fn supports_cross_node_single_use(&self) -> bool {
        true
    }
}
