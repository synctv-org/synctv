use std::sync::Arc;

use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::service::{JtiStore, JwtService, PublishKeyService, RedisJtiStore};
use synctv_core_testing::{
    ok, redis_connection_manager, start_redis_with_client, test_redis_key_prefix,
};

fn test_jwt_service() -> JwtService {
    ok(
        JwtService::new("test-secret-key-for-publish-key-tests-long-enough-1234567890"),
        "JWT service should build",
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn redis_jti_store_deduplicates_across_service_instances() {
    let (_container, client) = start_redis_with_client().await;
    let conn = redis_connection_manager(&client).await;
    let prefix = test_redis_key_prefix("jti-store");
    let store1 = RedisJtiStore::new(conn.clone(), prefix.clone(), 300);
    let store2 = RedisJtiStore::new(conn, prefix, 300);

    assert!(ok(
        store1.try_claim("cross_jti", 300).await,
        "first JTI claim should succeed"
    ));
    assert!(!ok(
        store2.try_claim("cross_jti", 300).await,
        "second JTI claim should succeed"
    ));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn publish_key_service_uses_redis_single_use_state() {
    let (_container, client) = start_redis_with_client().await;
    let conn = redis_connection_manager(&client).await;
    let service = PublishKeyService::from_store(
        test_jwt_service(),
        24,
        Arc::new(RedisJtiStore::from_runtime(
            synctv_core::direct_runtime(conn),
            test_redis_key_prefix("publish-key"),
            24 * 3600,
        )),
    );

    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();
    let key = ok(
        service.generate_publish_key(&room_id, &media_id, &user_id),
        "publish key should generate",
    );

    let claims = ok(
        service.validate_publish_key(&key.token).await,
        "publish key should validate once",
    );
    assert_eq!(claims.room_id, room_id.to_string());
    assert_eq!(claims.media_id, media_id.to_string());
    assert_eq!(claims.user_id, user_id.to_string());

    let replay = service.validate_publish_key(&key.token).await;
    assert!(
        matches!(replay, Err(synctv_core::Error::Authentication(ref message)) if message.contains("single-use")),
        "replayed publish key should fail with single-use error, got: {replay:?}"
    );
}
