#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use synctv_core::models::{MediaId, RoomId, UserId};
use synctv_core::service::{
    auth::JwtService,
    publish_key::{PublishKeyService, RedisJtiStore},
    JtiStore,
};
use synctv_core_testing::{
    redis_connection_manager, start_redis_with_client, test_redis_key_prefix,
};

fn test_jwt_service() -> JwtService {
    JwtService::new("test-secret-key-for-publish-key-tests-long-enough-1234567890").unwrap()
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn redis_jti_store_deduplicates_across_service_instances() {
    let (_container, client) = start_redis_with_client().await;
    let conn = redis_connection_manager(&client).await;
    let prefix = test_redis_key_prefix("jti-store");
    let store1 = RedisJtiStore::new(conn.clone(), prefix.clone(), 300);
    let store2 = RedisJtiStore::new(conn, prefix, 300);

    assert!(store1.try_claim("cross_jti", 300).await.unwrap());
    assert!(!store2.try_claim("cross_jti", 300).await.unwrap());
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
    let key = service
        .generate_publish_key(&room_id, &media_id, &user_id)
        .unwrap();

    let claims = service.validate_publish_key(&key.token).await.unwrap();
    assert_eq!(claims.room_id, room_id.to_string());
    assert_eq!(claims.media_id, media_id.to_string());
    assert_eq!(claims.user_id, user_id.to_string());

    let replay = service.validate_publish_key(&key.token).await;
    assert!(
        matches!(replay, Err(synctv_core::Error::Authentication(ref message)) if message.contains("single-use")),
        "replayed publish key should fail with single-use error, got: {replay:?}"
    );
}
