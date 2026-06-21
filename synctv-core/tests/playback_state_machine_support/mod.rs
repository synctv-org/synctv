use std::sync::Arc;

use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    models::{Media, MediaId, RoomId, SourceProvider, User, UserId, UserRole, UserStatus},
    repository::{MediaRepository, RoomPlaybackStateRepository},
    service::{
        auth::{BruteForceProtection, JwtService},
        InMemoryTokenBlacklistStore, RoomService, UserService,
    },
};
use synctv_core_testing::ok;

pub fn make_user_service(pool: &PgPool) -> UserService {
    let secret = "Test_Secret_Key_For_JWT_Tokens_32Bytes!!";
    let jwt_service = ok(JwtService::new(secret), "JWT service should build");
    let username_cache = UsernameCache::local_only("test:username:".to_string(), 100, 60);
    let token_blacklist = Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400));
    let key_builder = KeyBuilder::new("test");
    let brute_force = BruteForceProtection::in_memory("test".to_string());

    UserService::new_for_tests(
        pool,
        jwt_service,
        username_cache,
        token_blacklist,
        key_builder,
        brute_force,
    )
}

pub fn make_room_service(pool: PgPool) -> RoomService {
    let user_service = make_user_service(&pool);
    ok(
        RoomService::new_for_tests(pool, user_service),
        "room service should build",
    )
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

#[allow(
    dead_code,
    reason = "shared integration-test support is compiled independently per test target"
)]
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
        source_provider: SourceProvider::DirectUrl,
        source_config: synctv_core_testing::direct_url_media_source_config(
            "https://example.com/video.mp4",
        ),
        provider_instance_name: None,
        cover_file_reference_id: None,
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    let media = ok(
        MediaRepository::new(pool.clone()).create(&media).await,
        "test media should be created",
    );

    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = ok(
        playback_repo.create_or_get(&room_id).await,
        "playback state should be created",
    );
    state.playing_media_id = Some(media.id);
    state.playing_playlist_id = None;
    state.target.clear();
    state.position = 0.0;
    state = ok(
        playback_repo.update(&state).await,
        "playback state should point at test media",
    );
    assert_eq!(state.playing_media_id, Some(media.id));

    media
}
