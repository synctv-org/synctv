//! Test helpers and fixtures for synctv-core tests
//!
//! This module provides common test utilities, fixtures, and helpers
//! to reduce boilerplate and improve test consistency across the codebase.

use crate::models::{PlaylistId, RoomId, UserId, UserRole, UserStatus};
use chrono::Utc;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

fn stable_test_id(id: &str) -> i64 {
    if let Ok(parsed) = id.parse::<i64>() {
        return parsed.max(1);
    }
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    i64::try_from((hasher.finish() % (i64::MAX as u64 - 1)) + 1).expect("bounded positive id")
}

/// Create a test user ID
#[must_use]
pub fn test_user_id(id: &str) -> UserId {
    UserId::expect_positive(stable_test_id(id))
}

/// Create a test room ID
#[must_use]
pub fn test_room_id(id: &str) -> RoomId {
    RoomId::expect_positive(stable_test_id(id))
}

/// Generate a random user ID for testing
#[must_use]
pub fn random_user_id() -> UserId {
    UserId::new()
}

/// Generate a random room ID for testing
pub fn random_room_id() -> RoomId {
    RoomId::new()
}

/// Test fixture builder for User
pub struct UserFixture {
    id: UserId,
    username: String,
    email: Option<String>,
    password_hash: String,
    role: UserRole,
    status: UserStatus,
}

impl UserFixture {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: random_user_id(),
            username: "test_user".to_string(),
            email: None, // Will be auto-generated in build()
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: UserId) -> Self {
        self.id = id;
        self
    }

    #[must_use]
    pub fn with_username(mut self, username: &str) -> Self {
        self.username = username.to_string();
        self
    }

    #[must_use]
    pub fn with_role(mut self, role: UserRole) -> Self {
        self.role = role;
        self
    }

    #[must_use]
    pub fn with_status(mut self, status: UserStatus) -> Self {
        self.status = status;
        self
    }

    #[must_use]
    pub fn with_email(mut self, email: &str) -> Self {
        self.email = Some(email.to_string());
        self
    }

    #[must_use]
    pub fn build(self) -> crate::models::User {
        let now = Utc::now();
        // Generate unique email if not provided to avoid unique constraint violations in parallel tests
        let email = self
            .email
            .unwrap_or_else(|| format!("test_{}@example.com", synctv_common::snanoid!(8)));
        crate::models::User {
            id: self.id,
            username: self.username,
            email: Some(email),
            password_hash: self.password_hash,
            role: self.role,
            status: self.status,
            signup_method: crate::models::SignupMethod::Email,
            email_verified: true,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            version: 0,
            deleted_at: None,
            is_banned: false,
            banned_at: None,
            banned_by: None,
            banned_reason: None,
        }
    }
}

impl Default for UserFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Test fixture builder for Room
pub struct RoomFixture {
    id: RoomId,
    name: String,
    description: String,
    created_by: UserId,
    created_at: Option<chrono::DateTime<chrono::Utc>>,
    updated_at: Option<chrono::DateTime<chrono::Utc>>,
    last_activity_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl RoomFixture {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: random_room_id(),
            name: "Test Room".to_string(),
            description: String::new(),
            created_by: random_user_id(),
            created_at: None,
            updated_at: None,
            last_activity_at: None,
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: RoomId) -> Self {
        self.id = id;
        self
    }

    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    #[must_use]
    pub fn with_description(mut self, description: &str) -> Self {
        self.description = description.to_string();
        self
    }

    #[must_use]
    pub fn with_owner(mut self, created_by: UserId) -> Self {
        self.created_by = created_by;
        self
    }

    /// Set the `created_at` timestamp for testing `room_ttl` enforcement
    #[must_use]
    pub fn with_created_at(mut self, created_at: chrono::DateTime<chrono::Utc>) -> Self {
        self.created_at = Some(created_at);
        self
    }

    /// Set the `updated_at` timestamp for testing `room_ttl` enforcement
    #[must_use]
    pub fn with_updated_at(mut self, updated_at: chrono::DateTime<chrono::Utc>) -> Self {
        self.updated_at = Some(updated_at);
        self
    }

    /// Set the `last_activity_at` timestamp for testing `room_ttl` enforcement
    #[must_use]
    pub fn with_last_activity_at(
        mut self,
        last_activity_at: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        self.last_activity_at = Some(last_activity_at);
        self
    }

    #[must_use]
    pub fn build(self) -> crate::models::Room {
        let now = Utc::now();
        crate::models::Room {
            id: self.id,
            name: self.name,
            description: self.description,
            created_by: self.created_by,
            status: crate::models::RoomStatus::Active,
            is_banned: false,
            closed_at: None,
            created_at: self.created_at.unwrap_or(now),
            updated_at: self.updated_at.unwrap_or(now),
            deleted_at: None,
            version: 0,
            last_activity_at: self.last_activity_at.unwrap_or(now),
        }
    }
}

impl Default for RoomFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Test fixture builder for chat messages
pub struct ChatMessageFixture {
    id: i64,
    room_id: RoomId,
    user_id: UserId,
    content: String,
}

impl ChatMessageFixture {
    pub fn new() -> Self {
        Self {
            id: 0,
            room_id: random_room_id(),
            user_id: random_user_id(),
            content: "Test message".to_string(),
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: &str) -> Self {
        self.id = stable_test_id(id);
        self
    }

    #[must_use]
    pub fn with_room_id(mut self, room_id: RoomId) -> Self {
        self.room_id = room_id;
        self
    }

    #[must_use]
    pub fn with_user_id(mut self, user_id: UserId) -> Self {
        self.user_id = user_id;
        self
    }

    #[must_use]
    pub fn with_content(mut self, content: &str) -> Self {
        self.content = content.to_string();
        self
    }

    #[must_use]
    pub fn build(self) -> crate::models::ChatMessage {
        crate::models::ChatMessage {
            id: self.id,
            room_id: self.room_id,
            user_id: Some(self.user_id),
            content: self.content,
            message_type: 1,
            created_at: Utc::now(),
        }
    }
}

impl Default for ChatMessageFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Test fixture builder for Playlist
pub struct PlaylistFixture {
    id: PlaylistId,
    room_id: RoomId,
    creator_id: Option<UserId>,
    name: String,
    parent_id: Option<PlaylistId>,
    position: f64,
}

impl PlaylistFixture {
    /// Create a top-level playlist fixture.
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: PlaylistId::new(),
            room_id: random_room_id(),
            creator_id: None,
            name: String::new(),
            parent_id: None,
            position: 0.0,
        }
    }

    /// Create a child playlist fixture with a parent.
    pub fn new_child(parent_id: PlaylistId) -> Self {
        Self {
            id: PlaylistId::new(),
            room_id: random_room_id(),
            creator_id: None,
            name: format!("playlist_{}", synctv_common::snanoid!(8)),
            parent_id: Some(parent_id),
            position: 0.0,
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: PlaylistId) -> Self {
        self.id = id;
        self
    }

    #[must_use]
    pub fn with_room_id(mut self, room_id: RoomId) -> Self {
        self.room_id = room_id;
        self
    }

    #[must_use]
    pub fn with_creator(mut self, creator_id: UserId) -> Self {
        self.creator_id = Some(creator_id);
        self
    }

    /// Set the playlist display name.
    #[must_use]
    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    #[must_use]
    pub fn with_parent(mut self, parent_id: PlaylistId) -> Self {
        self.parent_id = Some(parent_id);
        self
    }

    #[must_use]
    pub fn with_position(mut self, position: impl Into<f64>) -> Self {
        self.position = position.into();
        self
    }

    #[must_use]
    pub fn build(self) -> crate::models::Playlist {
        let now = Utc::now();
        crate::models::Playlist {
            id: self.id,
            room_id: self.room_id,
            creator_id: self.creator_id,
            name: self.name,
            parent_id: self.parent_id,
            position: self.position,
            source_provider: None,
            source_config: None,
            provider_instance_name: None,
            created_at: now,
            updated_at: now,
            version: 0,
        }
    }
}

impl Default for PlaylistFixture {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper to create a valid playlist hierarchy for media tests.
///
/// Creates a top-level playlist and a child playlist with the given name.
///
/// # Example
/// ```ignore
/// let (top_level, child) = create_top_level_playlist_hierarchy(&playlist_repo, room.id.clone(), "Videos").await;
/// // Use child playlist for media items
/// let media = Media::new(child.id.clone(), ...);
/// ```
pub async fn create_top_level_playlist_hierarchy(
    playlist_repo: &crate::repository::playlist::PlaylistRepository,
    room_id: RoomId,
    child_name: &str,
) -> (crate::models::Playlist, crate::models::Playlist) {
    let top_level = PlaylistFixture::new().with_room_id(room_id).build();
    let top_level = playlist_repo
        .create(&top_level)
        .await
        .expect("Failed to create top-level playlist");

    let child = PlaylistFixture::new_child(top_level.id)
        .with_room_id(room_id)
        .with_name(child_name)
        .build();
    let child = playlist_repo
        .create(&child)
        .await
        .expect("Failed to create child playlist");

    (top_level, child)
}

/// Async test wrapper with timeout
///
/// Use this to prevent tests from hanging indefinitely.
pub async fn with_timeout<F>(duration: std::time::Duration, future: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::select! {
        result = future => result,
        () = tokio::time::sleep(duration) => {
            panic!("Test timed out after {duration:?}");
        }
    }
}

/// Assert that two futures complete concurrently within a time delta.
///
/// Both futures are wrapped with `tokio::time::timeout(max_delta_ms)` and
/// run via `tokio::join!`. If either future exceeds the deadline, the test
/// panics, ensuring both complete within the allowed window.
pub async fn assert_concurrent_completion<F1, F2>(
    max_delta_ms: u64,
    future1: F1,
    future2: F2,
) -> (F1::Output, F2::Output)
where
    F1: std::future::Future,
    F2: std::future::Future,
{
    let timeout = std::time::Duration::from_millis(max_delta_ms);
    let (r1, r2) = tokio::join!(
        tokio::time::timeout(timeout, future1),
        tokio::time::timeout(timeout, future2),
    );
    let result1 = r1.expect("future1 did not complete within max_delta_ms");
    let result2 = r2.expect("future2 did not complete within max_delta_ms");
    (result1, result2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_fixture() {
        let user = UserFixture::new()
            .with_username("alice")
            .with_role(UserRole::Admin)
            .with_status(UserStatus::Active)
            .build();

        assert_eq!(user.username, "alice");
        assert_eq!(user.role, UserRole::Admin);
        assert_eq!(user.status, UserStatus::Active);
    }

    #[test]
    fn test_room_fixture() {
        let owner_id = test_user_id("owner1");
        let room = RoomFixture::new()
            .with_name("My Room")
            .with_description("Test description")
            .with_owner(owner_id)
            .build();

        assert_eq!(room.name, "My Room");
        assert_eq!(room.description, "Test description");
        assert_eq!(room.created_by, owner_id);
    }

    #[test]
    fn test_chat_message_fixture() {
        let room_id = test_room_id("room1");
        let user_id = test_user_id("user1");
        let message = ChatMessageFixture::new()
            .with_room_id(room_id)
            .with_user_id(user_id)
            .with_content("Hello, world!")
            .build();

        assert_eq!(message.room_id, room_id);
        assert_eq!(message.user_id, Some(user_id));
        assert_eq!(message.content, "Hello, world!");
    }
}
