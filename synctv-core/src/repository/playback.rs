use sqlx::PgPool;

use crate::{
    models::{RoomId, RoomPlaybackState},
    Error, Result,
};

/// Room playback state repository
#[derive(Clone)]
pub struct RoomPlaybackStateRepository {
    pool: PgPool,
}

impl RoomPlaybackStateRepository {
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Create or get playback state for room
    ///
    /// Uses `ON CONFLICT DO NOTHING` followed by a SELECT to avoid triggering
    /// the `updated_at` BEFORE UPDATE trigger on existing rows.
    pub async fn create_or_get(&self, room_id: &RoomId) -> Result<RoomPlaybackState> {
        let state = RoomPlaybackState::new(room_id.clone());

        // Attempt insert; if the row already exists, do nothing
        sqlx::query(
            "INSERT INTO room_playback_state (room_id, \"current_time\", speed, is_playing, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (room_id) DO NOTHING"
        )
        .bind(room_id.as_str())
        .bind(state.current_time)
        .bind(state.speed)
        .bind(state.is_playing)
        .bind(state.updated_at)
        .bind(state.version)
        .execute(&self.pool)
        .await?;

        // Fetch the row (either just inserted or already existing)
        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "SELECT room_id, playing_media_id, playing_playlist_id, relative_path, \"current_time\", speed, is_playing, updated_at, version
             FROM room_playback_state
             WHERE room_id = $1"
        )
        .bind(room_id.as_str())
        .fetch_one(&self.pool)
        .await?;

        Ok(result)
    }

    /// Create or get playback state using a provided transaction
    ///
    /// Uses `ON CONFLICT DO NOTHING` followed by a SELECT to avoid triggering
    /// the `updated_at` BEFORE UPDATE trigger on existing rows.
    ///
    /// Accepts `&mut PgConnection` so the connection can be reborrowed for
    /// the follow-up SELECT query within the same transaction.
    pub async fn create_or_get_with_executor(&self, room_id: &RoomId, conn: &mut sqlx::PgConnection) -> Result<RoomPlaybackState>
    {
        let state = RoomPlaybackState::new(room_id.clone());

        sqlx::query(
            "INSERT INTO room_playback_state (room_id, \"current_time\", speed, is_playing, updated_at, version)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (room_id) DO NOTHING"
        )
        .bind(room_id.as_str())
        .bind(state.current_time)
        .bind(state.speed)
        .bind(state.is_playing)
        .bind(state.updated_at)
        .bind(state.version)
        .execute(&mut *conn)
        .await?;

        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "SELECT room_id, playing_media_id, playing_playlist_id, relative_path, \"current_time\", speed, is_playing, updated_at, version
             FROM room_playback_state
             WHERE room_id = $1"
        )
        .bind(room_id.as_str())
        .fetch_one(&mut *conn)
        .await?;

        Ok(result)
    }

    /// Get playback state
    pub async fn get(&self, room_id: &RoomId) -> Result<Option<RoomPlaybackState>> {
        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "SELECT room_id, playing_media_id, playing_playlist_id, relative_path, \"current_time\", speed, is_playing, updated_at, version
             FROM room_playback_state
             WHERE room_id = $1",
        )
        .bind(room_id.as_str())
        .fetch_optional(&self.pool)
        .await?;

        Ok(result)
    }

    /// Update playback state with optimistic locking
    pub async fn update(&self, state: &RoomPlaybackState) -> Result<RoomPlaybackState> {
        let media_id_str = state.playing_media_id.as_ref().map(super::super::models::id::MediaId::as_str);
        let playlist_id_str = state.playing_playlist_id.as_ref().map(super::super::models::id::PlaylistId::as_str);

        let result = sqlx::query_as::<_, RoomPlaybackState>(
            "UPDATE room_playback_state
             SET playing_media_id = $2, playing_playlist_id = $3, relative_path = $4,
                 \"current_time\" = $5, speed = $6, is_playing = $7,
                 updated_at = NOW(), version = version + 1
             WHERE room_id = $1 AND version = $8
             RETURNING room_id, playing_media_id, playing_playlist_id, relative_path, \"current_time\", speed, is_playing, updated_at, version",
        )
        .bind(state.room_id.as_str())
        .bind(media_id_str)
        .bind(playlist_id_str)
        .bind(&state.relative_path)
        .bind(state.current_time)
        .bind(state.speed)
        .bind(state.is_playing)
        .bind(state.version)
        .fetch_optional(&self.pool)
        .await?;

        match result {
            Some(s) => Ok(s),
            None => Err(Error::OptimisticLockConflict),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unit test for RoomPlaybackState default values and initial state
    #[test]
    fn test_playback_state_new_default_values() {
        let room_id = RoomId::from_string("test_room_001".to_string());
        let state = RoomPlaybackState::new(room_id.clone());

        assert_eq!(state.room_id, room_id);
        assert!(state.playing_media_id.is_none());
        assert!(state.playing_playlist_id.is_none());
        assert!(state.relative_path.is_empty());
        assert!((state.current_time - 0.0).abs() < f64::EPSILON);
        assert!((state.speed - 1.0).abs() < f64::EPSILON);
        assert!(!state.is_playing);
        assert_eq!(state.version, 0);
    }

    /// Unit test for repository constructor
    #[test]
    fn test_repository_new() {
        // This test verifies that the repository can be created
        // We use mockall or a connection pool mock in real integration tests
        // Here we just verify the constructor is const and compiles correctly
        fn _assert_const_new(pool: PgPool) -> RoomPlaybackStateRepository {
            RoomPlaybackStateRepository::new(pool)
        }
        // Compilation test only - cannot create PgPool without database
    }

    /// Integration test: Create and get playback state
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_create_or_get_playback_state() {
        use crate::test_helpers::{RoomFixture, UserFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(infra.pool.clone());

        // Create owner user first
        let owner = UserFixture::new().with_username("playback_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        // Create room
        let room = RoomFixture::new()
            .with_name("Playback Test Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playback state
        let state = playback_repo.create_or_get(&room.id).await.unwrap();
        assert_eq!(state.room_id, room.id);
        assert!(state.playing_media_id.is_none());
        assert!(!state.is_playing);
        assert_eq!(state.version, 0);

        // Get existing playback state (should return same state)
        let state2 = playback_repo.create_or_get(&room.id).await.unwrap();
        assert_eq!(state2.room_id, room.id);
        assert_eq!(state2.version, 0); // version should still be 0
    }

    /// Integration test: Get non-existent playback state returns None
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_get_nonexistent_playback_state() {
        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let playback_repo = RoomPlaybackStateRepository::new(infra.pool.clone());

        let room_id = RoomId::from_string("nonexistent_room".to_string());
        let result = playback_repo.get(&room_id).await.unwrap();
        assert!(result.is_none());
    }

    /// Integration test: Update playback state
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_update_playback_state() {
        use crate::test_helpers::{RoomFixture, UserFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;
        use crate::repository::playlist::PlaylistRepository;
        use crate::repository::media::MediaRepository;
        use crate::models::Media;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playlist_repo = PlaylistRepository::new(infra.pool.clone());
        let media_repo = MediaRepository::new(infra.pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("playback_update_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Playback Update Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playlist hierarchy (root + child with name)
        let (_, playlist) = crate::test_helpers::create_media_playlist_hierarchy(
            &playlist_repo,
            room.id.clone(),
            "Playback Playlist",
        ).await;

        // Create media for playback reference (required by FK constraint)
        let media = Media::from_provider(
            playlist.id.clone(),
            room.id.clone(),
            Some(owner.id.clone()),
            "Test Video".to_string(),
            serde_json::json!({"url": "https://example.com/video.mp4"}),
            "direct_url",
            "default".to_string(),
            0,
        );
        let media = media_repo.create(&media).await.unwrap();

        // Create playback state
        let mut state = playback_repo.create_or_get(&room.id).await.unwrap();

        // Update state with valid media_id reference
        state.current_time = 120.5;
        state.speed = 1.5;
        state.is_playing = true;
        state.playing_media_id = Some(media.id.clone());

        let updated = playback_repo.update(&state).await.unwrap();
        assert!((updated.current_time - 120.5).abs() < f64::EPSILON);
        assert!((updated.speed - 1.5).abs() < f64::EPSILON);
        assert!(updated.is_playing);
        assert!(updated.playing_media_id.is_some());
        assert_eq!(updated.version, state.version + 1); // version should increment
    }

    /// Integration test: Optimistic lock conflict detection
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_optimistic_lock_conflict() {
        use crate::test_helpers::{RoomFixture, UserFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("lock_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Lock Test Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playback state
        let state = playback_repo.create_or_get(&room.id).await.unwrap();

        // First update succeeds
        let mut state1 = state.clone();
        state1.current_time = 50.0;
        let updated1 = playback_repo.update(&state1).await.unwrap();
        assert_eq!(updated1.version, 1);

        // Second update with stale version fails (optimistic lock conflict)
        let mut state2 = state.clone(); // Still has version 0
        state2.current_time = 100.0;
        let result = playback_repo.update(&state2).await;
        assert!(matches!(result, Err(crate::Error::OptimisticLockConflict)));
    }

    /// Integration test: Version increments on each update
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_version_increments_on_update() {
        use crate::test_helpers::{RoomFixture, UserFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("version_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Version Test Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playback state
        let mut state = playback_repo.create_or_get(&room.id).await.unwrap();
        assert_eq!(state.version, 0);

        // Multiple updates
        for i in 1..=5 {
            state.current_time = i as f64 * 10.0;
            state = playback_repo.update(&state).await.unwrap();
            assert_eq!(state.version, i);
        }
    }

    /// Integration test: Boundary conditions for current_time and speed
    #[tokio::test]
    #[ignore = "Requires Docker"]
    async fn test_boundary_conditions() {
        use crate::test_helpers::{RoomFixture, UserFixture};
        use crate::repository::user::UserRepository;
        use crate::repository::room::RoomRepository;

        let infra = crate::test_helpers::containers::TestInfra::postgres_only().await;
        let user_repo = UserRepository::new(infra.pool.clone());
        let room_repo = RoomRepository::new(infra.pool.clone());
        let playback_repo = RoomPlaybackStateRepository::new(infra.pool.clone());

        // Create owner and room
        let owner = UserFixture::new().with_username("boundary_owner").build();
        let owner = user_repo.create(&owner).await.unwrap();

        let room = RoomFixture::new()
            .with_name("Boundary Test Room")
            .with_owner(owner.id.clone())
            .build();
        let room = room_repo.create(&room).await.unwrap();

        // Create playback state
        let mut state = playback_repo.create_or_get(&room.id).await.unwrap();

        // Test zero current_time
        state.current_time = 0.0;
        state = playback_repo.update(&state).await.unwrap();
        assert!((state.current_time - 0.0).abs() < f64::EPSILON);

        // Test very large current_time (e.g., long video)
        state.current_time = 7200.5; // 2 hours
        state = playback_repo.update(&state).await.unwrap();
        assert!((state.current_time - 7200.5).abs() < f64::EPSILON);

        // Test very small speed (but not zero)
        state.speed = 0.25;
        state = playback_repo.update(&state).await.unwrap();
        assert!((state.speed - 0.25).abs() < f64::EPSILON);

        // Test very large speed
        state.speed = 4.0;
        state = playback_repo.update(&state).await.unwrap();
        assert!((state.speed - 4.0).abs() < f64::EPSILON);

        // Test negative current_time (should be allowed for some edge cases)
        state.current_time = -1.0;
        let _result = playback_repo.update(&state).await;
        // Note: Whether negative time is allowed depends on database constraints
        // This test documents the expected behavior
    }
}
