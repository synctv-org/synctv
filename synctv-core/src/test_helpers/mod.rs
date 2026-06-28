//! Test helpers and fixtures for synctv-core tests
//!
//! This module provides common test utilities, fixtures, and helpers
//! to reduce boilerplate and improve test consistency across the codebase.

use crate::models::{PlaylistId, RoomId, UserId, UserRole, UserStatus};
use chrono::Utc;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub fn ok<T, E: std::fmt::Debug>(result: std::result::Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

pub fn err<T, E: std::fmt::Debug>(result: std::result::Result<T, E>, context: &str) -> E {
    match result {
        Ok(_) => std::panic::panic_any(context.to_string()),
        Err(error) => error,
    }
}

pub fn some<T>(value: Option<T>, context: &str) -> T {
    match value {
        Some(value) => value,
        None => std::panic::panic_any(context.to_string()),
    }
}

pub trait TestResultExt<T, E> {
    fn checked(self, context: &str) -> T;
    fn failed(self, context: &str) -> E;
}

impl<T, E: std::fmt::Debug> TestResultExt<T, E> for std::result::Result<T, E> {
    fn checked(self, context: &str) -> T {
        ok(self, context)
    }

    fn failed(self, context: &str) -> E {
        err(self, context)
    }
}

pub trait TestOptionExt<T> {
    fn checked(self, context: &str) -> T;
}

impl<T> TestOptionExt<T> for Option<T> {
    fn checked(self, context: &str) -> T {
        some(self, context)
    }
}

#[derive(Clone)]
struct FailingRedisRuntime;

#[async_trait::async_trait]
impl crate::RedisConnectionRuntime for FailingRedisRuntime {
    async fn snapshot(&self) -> redis::RedisResult<redis::aio::ConnectionManager> {
        std::panic::panic_any("failing_redis_runtime snapshot should not be called")
    }
}

#[must_use]
pub fn failing_redis_runtime() -> Arc<dyn crate::RedisConnectionRuntime> {
    Arc::new(FailingRedisRuntime)
}

fn stable_test_id(id: &str) -> i64 {
    if let Ok(parsed) = id.parse::<i64>() {
        return parsed.max(1);
    }
    let mut hasher = DefaultHasher::new();
    id.hash(&mut hasher);
    let bounded = (hasher.finish() % (i64::MAX as u64 - 1)) + 1;
    match i64::try_from(bounded) {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("bounded test id should fit in i64: {error}")),
    }
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
    role: UserRole,
    status: UserStatus,
}

impl UserFixture {
    #[must_use]
    pub fn new() -> Self {
        Self {
            id: random_user_id(),
            username: "test_user".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
        }
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
    pub fn build(self) -> crate::models::User {
        let now = Utc::now();
        crate::models::User {
            id: self.id,
            username: self.username,
            role: self.role,
            avatar_file_reference_id: None,
            status: self.status,
            signup_method: crate::models::SignupMethod::Email,
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

    #[must_use]
    pub fn build(self) -> crate::models::Room {
        let now = Utc::now();
        crate::models::Room {
            id: self.id,
            name: self.name,
            description: self.description,
            cover_file_reference_id: None,
            category: None,
            labels: Vec::new(),
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
            client_message_id: None,
            content: self.content,
            message_type: crate::models::ChatMessageType::Text,
            status: crate::models::ChatMessageStatus::Active,
            version: 1,
            reply_to_message_id: None,
            reply_to_message_created_at: None,
            metadata: crate::models::ChatMetadata::default(),
            edited_at: None,
            deleted_at: None,
            deleted_by: None,
            delete_reason: None,
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
            description: String::new(),
            cover_file_reference_id: None,
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
    let top_level = ok(
        playlist_repo.create(&top_level).await,
        "Failed to create top-level playlist",
    );

    let child = PlaylistFixture::new_child(top_level.id)
        .with_room_id(room_id)
        .with_name(child_name)
        .build();
    let child = ok(
        playlist_repo.create(&child).await,
        "Failed to create child playlist",
    );

    (top_level, child)
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
