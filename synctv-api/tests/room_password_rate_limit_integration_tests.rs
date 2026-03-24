#![allow(clippy::unwrap_used)]

use chrono::Utc;
use std::sync::Arc;
use synctv_cluster::sync::{ConnectionLimits, ConnectionManager};
use synctv_core::{
    cache::{KeyBuilder, NoopCacheL2, UsernameCache},
    config::PasswordComplexityConfig,
    models::{SignupMethod, User, UserId, UserRole, UserStatus},
    repository::UserRepository,
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
    Config,
};

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

fn make_user_service(pool: sqlx::PgPool) -> UserService {
    let jwt_service = JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap();
    let username_cache =
        UsernameCache::new(Arc::new(NoopCacheL2), "test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    UserService::new(
        pool,
        jwt_service,
        username_cache,
        PasswordComplexityConfig::default(),
        token_blacklist,
        KeyBuilder::new("test"),
        BruteForceProtection::in_memory("test:user".to_string()),
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_client_api_room_password_success_resets_bruteforce_counter() {
    let (_postgres, pool) = synctv_core_testing::create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());

    let user_service = Arc::new(make_user_service(pool.clone()));
    let mut room_service = RoomService::new(pool.clone(), (*user_service).clone());
    room_service.set_brute_force_service(BruteForceProtection::in_memory(
        "test:room-password".to_string(),
    ));
    let room_service = Arc::new(room_service);

    let owner = user_repo.create(&make_user("room_owner")).await.unwrap();
    let (room, _member) = room_service
        .create_room(
            "Protected Room".to_string(),
            "Room with password".to_string(),
            owner.id,
            Some("CorrectPassword123".to_string()),
            None,
        )
        .await
        .unwrap();

    let connection_manager = Arc::new(ConnectionManager::new(ConnectionLimits::default()));
    connection_manager.start();

    let client_api = synctv_api::impls::ClientApiImpl::new(
        user_service,
        room_service,
        connection_manager,
        Arc::new(Config::default()),
        None,
        JwtService::new("Test_Secret_Key_For_JWT_Tokens_32Bytes!!").unwrap(),
        None,
        None,
        None,
    )
    .with_rate_limiter(
        synctv_core::service::rate_limit::RateLimiter::in_memory_only(
            "api:room-password:".to_string(),
        ),
    );

    for attempt in 0..4 {
        let resp = client_api
            .check_room_password(
                room.id.as_str(),
                synctv_api::proto::client::CheckRoomPasswordRequest {
                    room_id: room.id.to_string(),
                    password: "WrongPassword".to_string(),
                },
                "192.168.1.100",
            )
            .await
            .unwrap_or_else(|_| panic!("failed attempt {} should not be blocked", attempt + 1));
        assert!(!resp.valid);
    }

    let success = client_api
        .check_room_password(
            room.id.as_str(),
            synctv_api::proto::client::CheckRoomPasswordRequest {
                room_id: room.id.to_string(),
                password: "CorrectPassword123".to_string(),
            },
            "192.168.1.100",
        )
        .await
        .expect("successful password check should pass");
    assert!(success.valid);

    let after_reset = client_api
        .check_room_password(
            room.id.as_str(),
            synctv_api::proto::client::CheckRoomPasswordRequest {
                room_id: room.id.to_string(),
                password: "WrongPassword".to_string(),
            },
            "192.168.1.100",
        )
        .await
        .expect("successful check must reset room password brute-force counter");
    assert!(!after_reset.valid);
}
