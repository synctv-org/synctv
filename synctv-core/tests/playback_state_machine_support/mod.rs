use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::{Media, MediaId, RoomId, User, UserId, UserRole, UserStatus},
    repository::{MediaRepository, RoomPlaybackStateRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
};

pub fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = JwtService::new(secret).expect("Failed to create JwtService");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let password_complexity = PasswordComplexityConfig::default();
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new(
        pool,
        jwt_service,
        username_cache,
        password_complexity,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

pub fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);
    RoomService::new(pool, user_service)
}

pub fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

#[allow(dead_code)]
pub async fn set_current_test_media(
    pool: &PgPool,
    room_id: RoomId,
    creator_id: UserId,
    name: &str,
) -> Media {
    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id,
        creator_id: Some(creator_id),
        name: name.to_string(),
        description: String::new(),
        position: 0.0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = MediaRepository::new(pool.clone())
        .create(&media)
        .await
        .expect("test media should be created");

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = playback_repo
        .create_or_get(&room_id)
        .await
        .expect("playback state should be created");
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target.clear();
    state.position = 0.0;
    state = playback_repo
        .update(&state)
        .await
        .expect("playback state should point at test media");
    assert_eq!(state.playing_media_id, Some(media.id));

    media
}
