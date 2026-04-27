//! Playback state machine concurrency and versioning tests.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use synctv_core::repository::UserRepository;
use synctv_core_testing::create_test_pool;

mod playback_state_machine_support;

use playback_state_machine_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_concurrent_play_pause_operations() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = Arc::new(make_room_service(pool.clone()));

    let owner = user_repo.create(&make_user("sm_concurrent")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Concurrent Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let mut handles = vec![];
    let barrier = Arc::new(tokio::sync::Barrier::new(10));

    for i in 0..10 {
        let rs = room_service.clone();
        let rid = room.id;
        let uid = owner.id;
        let b = barrier.clone();
        let playing = i % 2 == 0;

        handles.push(tokio::spawn(async move {
            b.wait().await;
            rs.playback_service().set_playing(rid, uid, playing).await
        }));
    }

    let results: Vec<_> = futures::future::join_all(handles).await;

    let mut success_count = 0;
    let mut error_count = 0;
    for result in &results {
        match result {
            Ok(Ok(_)) => success_count += 1,
            Ok(Err(_)) => error_count += 1,
            Err(e) => panic!("Task panicked: {e:?}"),
        }
    }

    println!("Concurrent play/pause: success={success_count}/10, errors={error_count}");
    assert!(success_count >= 3);

    let state = room_service
        .playback_service()
        .get_state(&room.id)
        .await
        .unwrap();
    let _ = state.is_playing;
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_version_increments_on_state_change() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let owner = user_repo.create(&make_user("sm_version")).await.unwrap();

    let (room, _) = room_service
        .create_room(
            "Version Room".to_string(),
            String::new(),
            owner.id,
            None,
            None,
        )
        .await
        .unwrap();

    let playback_service = room_service.playback_service();

    let state = playback_service.get_state(&room.id).await.unwrap();
    let initial_version = state.version;

    let state = playback_service
        .set_playing(room.id, owner.id, true)
        .await
        .unwrap();
    assert_eq!(state.version, initial_version + 1);

    let state = playback_service
        .set_playing(room.id, owner.id, false)
        .await
        .unwrap();
    assert_eq!(state.version, initial_version + 2);

    let state = playback_service
        .seek(room.id, owner.id, 50.0)
        .await
        .unwrap()
        .state;
    assert_eq!(state.version, initial_version + 3);

    let state = playback_service
        .change_speed(room.id, owner.id, 1.5)
        .await
        .unwrap();
    assert_eq!(state.version, initial_version + 4);
}
