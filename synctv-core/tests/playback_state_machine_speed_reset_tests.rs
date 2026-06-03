//! Playback state machine speed and reset tests.

#![allow(clippy::unwrap_used)]

use synctv_core::repository::UserRepository;
use synctv_core_testing::create_test_pool;

mod playback_state_machine_support;

use playback_state_machine_support::{make_room_service, make_user, set_current_test_media};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_speed_change_preserves_position() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_speed_pos")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Speed Position Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();
    set_current_test_media(&pool, room.id, owner.id, "Speed Position Video").await;
    playback_service
        .seek(room.id, owner.id, 100.0)
        .await
        .unwrap();
    playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .unwrap();

    let state = playback_service
        .change_speed(room.id, owner.id, 2.0)
        .await
        .unwrap();

    assert!(state.position >= 99.0);
    assert!((state.speed - 2.0).abs() < f64::EPSILON);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_speed_change_while_paused() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_speed_paused"))
        .await
        .unwrap();

    let (room, _) = room_service
        .create_room(
            "Speed Paused Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let state = room_service
        .playback_service()
        .change_speed(room.id, owner.id, 1.5)
        .await
        .unwrap();

    assert!(!state.is_playing);
    assert!((state.speed - 1.5).abs() < f64::EPSILON);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_returns_to_initial_state() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_reset")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Reset Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();
    set_current_test_media(&pool, room.id, owner.id, "Reset Video").await;
    playback_service
        .seek(room.id, owner.id, 200.0)
        .await
        .unwrap();
    playback_service
        .change_speed(room.id, owner.id, 2.0)
        .await
        .unwrap();
    playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .unwrap();

    let state = playback_service.reset(room.id, owner.id).await.unwrap();

    assert!(!state.is_playing);
    assert!((state.position - 0.0).abs() < f64::EPSILON);
    assert!((state.speed - 1.0).abs() < f64::EPSILON);
    assert!(state.playing_media_id.is_none());
    assert!(state.playing_playlist_id.is_none());
}
