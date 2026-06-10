//! Playback state machine position tests.

use chrono::Utc;
use synctv_core::{
    models::{Media, MediaId},
    repository::{MediaRepository, UserRepository},
};
use synctv_core_testing::{create_test_pool, TestResultExt};

mod playback_state_machine_support;

use playback_state_machine_support::{make_room_service, make_user, set_current_test_media};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_preserved_on_pause() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_pos_pause"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Position Pause Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();
    set_current_test_media(&pool, room.id, owner.id, "Pause Position Video").await;
    playback_service
        .seek(room.id, owner.id, 120.0)
        .await
        .checked("operation should succeed");
    playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");

    let state = playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .checked("operation should succeed");

    assert!(state.position >= 119.0);

    room_service.playback_service().shutdown().await;
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_reset_on_media_switch() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_pos_switch"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Position Switch Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id,
        creator_id: Some(owner.id),
        name: "Test Video".to_string(),
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
    media_repo
        .create(&media)
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();
    set_current_test_media(&pool, room.id, owner.id, "Previous Video").await;
    playback_service
        .seek(room.id, owner.id, 150.0)
        .await
        .checked("operation should succeed");

    let state = playback_service
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .checked("operation should succeed");

    assert!((state.position - 0.0).abs() < f64::EPSILON);
    assert!(state.is_playing);

    room_service.playback_service().shutdown().await;
    pool.close().await;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_preserves_progress_for_current_media() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo
        .create(&make_user("sm_pos_reset_resume"))
        .await
        .checked("operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Position Reset Resume Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .checked("operation should succeed");

    let playback_service = room_service.playback_service();
    let media = set_current_test_media(&pool, room.id, owner.id, "Reset Resume Video").await;
    playback_service
        .seek(room.id, owner.id, 90.0)
        .await
        .checked("operation should succeed");
    playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .checked("operation should succeed");

    tokio::time::sleep(std::time::Duration::from_millis(25)).await;

    let reset_state = playback_service
        .reset(room.id, owner.id)
        .await
        .checked("operation should succeed");
    assert!(reset_state.playing_media_id.is_none());
    assert!((reset_state.position - 0.0).abs() < f64::EPSILON);

    let resumed_state = playback_service
        .switch(room.id, owner.id, Some(media.id), None, Vec::new())
        .await
        .checked("operation should succeed");
    assert_eq!(resumed_state.playing_media_id, Some(media.id));
    assert!(
        resumed_state.position >= 90.0,
        "switching back should resume from the calibrated stop position, got {}",
        resumed_state.position
    );

    room_service.playback_service().shutdown().await;
    pool.close().await;
}
