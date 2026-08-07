//! Playback state machine transition tests.

use std::sync::Arc;

use synctv_core::repository::UserRepository;
use synctv_core_testing::{create_test_pool, TestResultExt};

mod playback_state_machine_support;

use playback_state_machine_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_initial_state_is_stopped() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_initial"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Initial State Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();
    let state = playback_service
        .get_state(&room.id)
        .await
        .checked("operation should succeed");

    assert!(!state.is_playing);
    assert!((state.position - 0.0).abs() < f64::EPSILON);
    assert!((state.speed - 1.0).abs() < f64::EPSILON);
    assert!(state.playing_media_id.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_stopped_to_playing_transition() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_play"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Play Transition Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let state = room_service
        .playback_service()
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");

    assert!(state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_playing_to_paused_transition() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_pause"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Pause Transition Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();
    playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");

    let state = playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .checked("operation should succeed");

    assert!(!state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_paused_to_playing_transition() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_resume"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Resume Transition Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();
    playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");
    playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .checked("operation should succeed");

    let state = playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");

    assert!(state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_state_transition_matrix_all_valid() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_matrix"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Matrix Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();

    let state = playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .checked("operation should succeed");
    assert!(!state.is_playing);

    let state = playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");
    assert!(state.is_playing);

    let state = playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");
    assert!(state.is_playing);

    let state = playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .checked("operation should succeed");
    assert!(!state.is_playing);

    let state = playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .checked("operation should succeed");
    assert!(!state.is_playing);

    let state = playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");
    assert!(state.is_playing);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_rapid_state_transitions() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo
        .create(&make_user("sm_toggle"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Toggle Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();

    for i in 0..10 {
        let playing = i % 2 == 0;
        let state = playback_service
            .set_playing(room.id, owner.id, playing)
            .await
            .checked("operation should succeed");

        assert_eq!(state.is_playing, playing);
    }

    let final_state = playback_service
        .get_state(&room.id)
        .await
        .checked("operation should succeed");
    assert!(!final_state.is_playing);
}
