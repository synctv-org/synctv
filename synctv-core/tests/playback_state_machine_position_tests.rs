//! Playback state machine position tests.

#![allow(clippy::unwrap_used)]

use chrono::Utc;
use synctv_core::{
    models::{Media, MediaId},
    repository::{MediaRepository, UserRepository},
};
use synctv_core_testing::create_test_pool;

mod playback_state_machine_support;

use playback_state_machine_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_position_preserved_on_pause() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_pos_pause")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Position Pause Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 120.0)
        .await
        .unwrap();
    playback_service
        .set_playing(room.id.clone(), owner.id.clone(), true)
        .await
        .unwrap();

    let state = playback_service
        .set_playing(room.id.clone(), owner.id.clone(), false)
        .await
        .unwrap();

    assert!(state.current_time >= 119.0);

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

    let owner = user_repo.create(&make_user("sm_pos_switch")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Position Switch Room".to_string(),
            String::new(),
            owner.id.clone(),
            None,
            None,
        )
        .await
        .unwrap();

    let media = Media {
        id: MediaId::new(),
        playlist_id: None,
        room_id: room.id.clone(),
        creator_id: Some(owner.id.clone()),
        name: "Test Video".to_string(),
        position: 0,
        source_provider: "direct_url".to_string(),
        source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
        provider_instance_name: "direct_url".to_string(),
        added_at: Utc::now(),
        updated_at: Utc::now(),
        version: 0,
    };
    media_repo.create(&media).await.unwrap();

    let playback_service = room_service.playback_service();
    playback_service
        .seek(room.id.clone(), owner.id.clone(), 150.0)
        .await
        .unwrap();

    let state = playback_service
        .switch(
            room.id.clone(),
            owner.id.clone(),
            Some(media.id.clone()),
            None,
            Vec::new(),
        )
        .await
        .unwrap();

    assert!((state.current_time - 0.0).abs() < f64::EPSILON);
    assert!(state.is_playing);

    room_service.playback_service().shutdown().await;
    pool.close().await;
}
