use chrono::Utc;
use synctv_core::models::{
    EmbyPlaybackSession, FromProviderParams, Media, ProviderPlaybackSession,
    ProviderPlaybackSessionState, ProviderPlaybackStopReason, Room, RoomId, RoomStatus,
    SourceProvider, User, UserId, UserRole, UserStatus,
};
use synctv_core::repository::{
    MediaRepository, NewProviderPlaybackSession, ProviderPlaybackSessionRepository,
    RoomPlaybackStateRepository, RoomRepository, UserRepository,
};
use synctv_core_testing::{create_test_pool, ok};

fn user(username: &str) -> User {
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

fn room(owner: UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: "Provider playback session test".to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: owner,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn typed_session_survives_reactivation_and_cleanup_fencing() {
    let (_postgres, pool) = create_test_pool().await;
    let owner = ok(
        UserRepository::new(pool.clone())
            .create(&user("provider_session_owner"))
            .await,
        "owner should be created",
    );
    let room = ok(
        RoomRepository::new(pool.clone())
            .create(&room(owner.id))
            .await,
        "room should be created",
    );
    let playback_repo = RoomPlaybackStateRepository::new(pool.clone());
    let mut state = ok(
        playback_repo.create_or_get(&room.id).await,
        "playback state should be created",
    );
    let media = ok(
        MediaRepository::new(pool.clone())
            .create(&Media::from_provider_with_params(FromProviderParams {
                playlist_id: None,
                room_id: room.id,
                creator_id: Some(owner.id),
                name: "Provider session media".to_string(),
                description: String::new(),
                source_config: synctv_core_testing::direct_url_media_source_config(
                    "https://example.com/provider-session.mp4",
                ),
                source_provider: SourceProvider::DirectUrl,
                provider_instance_name: None,
                position: 0.0,
            }))
            .await,
        "media should be created",
    );
    state.playing_media_id = Some(media.id);
    state = ok(
        playback_repo.update(&state).await,
        "playback source should advance generation",
    );
    assert!(state.playback_generation > 0);
    let repo = ProviderPlaybackSessionRepository::new(pool);
    let new_session = || NewProviderPlaybackSession {
        room_id: room.id,
        playback_generation: state.playback_generation,
        provider_instance_name: Some("emby-main".to_string()),
        credential_owner_id: owner.id,
        resource_key: "emby:play-session-1".to_string(),
        resource_version: Some("version-1".to_string()),
        session: ProviderPlaybackSession::Emby(EmbyPlaybackSession {
            server_id: "emby-home".to_string(),
            item_id: "item-1".to_string(),
            play_session_id: "play-session-1".to_string(),
            media_source_id: None,
            playback_cache_key: "cache-1".to_string(),
            start_reported: false,
        }),
        paused: false,
    };
    let id = ok(
        repo.upsert(new_session()).await,
        "typed provider session should be persisted",
    );
    ok(
        repo.mark_emby_started(id).await,
        "Emby start marker should update typed session JSON",
    );

    let active = ok(
        repo.active_for_generation(room.id, state.playback_generation)
            .await,
        "active session should load",
    );
    assert!(matches!(
        active.as_slice(),
        [record]
            if matches!(
                &record.session,
                ProviderPlaybackSession::Emby(EmbyPlaybackSession {
                    start_reported: true,
                    ..
                })
            )
    ));

    ok(
        repo.request_generation_stop(
            room.id,
            state.playback_generation,
            12.5,
            ProviderPlaybackStopReason::Stopped,
        )
        .await,
        "generation should request cleanup",
    );
    let claimed = ok(repo.claim_cleanup(10).await, "cleanup should be claimed");
    assert!(matches!(
        claimed.as_slice(),
        [record]
            if record.id == id
                && record.state == ProviderPlaybackSessionState::StopRequested
                && record.stop_position == Some(12.5)
                && record.cleanup_fence == 1
    ));
    let stale_claim = &claimed[0];
    assert_eq!(
        ok(
            repo.upsert(new_session()).await,
            "claimed session should reactivate"
        ),
        id
    );
    assert!(!ok(
        repo.delete_claimed(stale_claim.id, stale_claim.cleanup_fence)
            .await,
        "stale cleanup delete should be fenced"
    ));
    assert!(!ok(
        repo.retry_claimed(stale_claim.id, stale_claim.cleanup_fence, 0)
            .await,
        "stale cleanup retry should be fenced"
    ));
    let active = ok(
        repo.active_for_generation(room.id, state.playback_generation)
            .await,
        "reactivated session should remain active",
    );
    assert!(matches!(
        active.as_slice(),
        [record]
            if record.id == id
                && record.state == ProviderPlaybackSessionState::Active
                && record.cleanup_fence == stale_claim.cleanup_fence + 1
    ));

    ok(
        repo.request_generation_stop(
            room.id,
            state.playback_generation,
            18.0,
            ProviderPlaybackStopReason::Stopped,
        )
        .await,
        "reactivated generation should request cleanup",
    );
    let claimed = ok(
        repo.claim_cleanup(10).await,
        "reactivated session cleanup should be claimed",
    );
    let record = &claimed[0];
    assert_eq!(record.cleanup_fence, stale_claim.cleanup_fence + 2);
    assert!(ok(
        repo.delete_claimed(record.id, record.cleanup_fence).await,
        "claimed session should be deleted"
    ));
}
