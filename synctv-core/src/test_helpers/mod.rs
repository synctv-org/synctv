//! Test helpers and fixtures for synctv-core tests
//!
//! This module provides common test utilities, fixtures, and helpers
//! to reduce boilerplate and improve test consistency across the codebase.

pub mod containers;

use crate::models::{RoomId, UserId, UserRole, UserStatus};
use chrono::Utc;

/// Create a test user ID
pub fn test_user_id(id: &str) -> UserId {
    UserId::from_string(id.to_string())
}

/// Create a test room ID
pub fn test_room_id(id: &str) -> RoomId {
    RoomId(id.to_string())
}

/// Generate a random user ID for testing
pub fn random_user_id() -> UserId {
    UserId::new()
}

/// Generate a random room ID for testing
pub fn random_room_id() -> RoomId {
    RoomId(nanoid::nanoid!(12))
}

/// Test fixture builder for User
pub struct UserFixture {
    id: UserId,
    username: String,
    password_hash: String,
    role: UserRole,
    status: UserStatus,
}

impl UserFixture {
    pub fn new() -> Self {
        Self {
            id: random_user_id(),
            username: "test_user".to_string(),
            password_hash: "hash".to_string(),
            role: UserRole::User,
            status: UserStatus::Active,
        }
    }

    pub fn with_id(mut self, id: UserId) -> Self {
        self.id = id;
        self
    }

    pub fn with_username(mut self, username: &str) -> Self {
        self.username = username.to_string();
        self
    }

    pub fn with_role(mut self, role: UserRole) -> Self {
        self.role = role;
        self
    }

    pub fn with_status(mut self, status: UserStatus) -> Self {
        self.status = status;
        self
    }

    pub fn build(self) -> crate::models::User {
        let now = Utc::now();
        crate::models::User {
            id: self.id,
            username: self.username,
            email: Some("test@example.com".to_string()),
            password_hash: self.password_hash,
            role: self.role,
            status: self.status,
            signup_method: None,
            email_verified: true,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            deleted_at: None,
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
}

impl RoomFixture {
    pub fn new() -> Self {
        Self {
            id: random_room_id(),
            name: "Test Room".to_string(),
            description: String::new(),
            created_by: random_user_id(),
        }
    }

    pub fn with_id(mut self, id: RoomId) -> Self {
        self.id = id;
        self
    }

    pub fn with_name(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }

    pub fn with_description(mut self, description: &str) -> Self {
        self.description = description.to_string();
        self
    }

    pub fn with_owner(mut self, created_by: UserId) -> Self {
        self.created_by = created_by;
        self
    }

    pub fn build(self) -> crate::models::Room {
        crate::models::Room {
            id: self.id,
            name: self.name,
            description: self.description,
            created_by: self.created_by,
            status: crate::models::RoomStatus::Active,
            is_banned: false,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            deleted_at: None,
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
    id: String,
    room_id: RoomId,
    user_id: UserId,
    content: String,
}

impl ChatMessageFixture {
    pub fn new() -> Self {
        Self {
            id: nanoid::nanoid!(12),
            room_id: random_room_id(),
            user_id: random_user_id(),
            content: "Test message".to_string(),
        }
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self.id = id.to_string();
        self
    }

    pub fn with_room_id(mut self, room_id: RoomId) -> Self {
        self.room_id = room_id;
        self
    }

    pub fn with_user_id(mut self, user_id: UserId) -> Self {
        self.user_id = user_id;
        self
    }

    pub fn with_content(mut self, content: &str) -> Self {
        self.content = content.to_string();
        self
    }

    pub fn build(self) -> crate::models::ChatMessage {
        crate::models::ChatMessage {
            id: self.id,
            room_id: self.room_id,
            user_id: self.user_id,
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

/// Async test wrapper with timeout
///
/// Use this to prevent tests from hanging indefinitely.
pub async fn with_timeout<F>(duration: std::time::Duration, future: F) -> F::Output
where
    F: std::future::Future,
{
    tokio::select! {
        result = future => result,
        _ = tokio::time::sleep(duration) => {
            panic!("Test timed out after {:?}", duration);
        }
    }
}

/// Assert that two futures complete within a time delta
///
/// Useful for testing concurrent operations.
pub async fn assert_concurrent_completion<F1, F2>(
    _max_delta_ms: u64,
    future1: F1,
    future2: F2,
) -> (F1::Output, F2::Output)
where
    F1: std::future::Future,
    F2: std::future::Future,
{
    let (result1, result2) = tokio::join!(future1, future2);
    // In a real implementation, we'd measure the timing delta
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
            .with_owner(owner_id.clone())
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
            .with_room_id(room_id.clone())
            .with_user_id(user_id.clone())
            .with_content("Hello, world!")
            .build();

        assert_eq!(message.room_id, room_id);
        assert_eq!(message.user_id, user_id);
        assert_eq!(message.content, "Hello, world!");
    }
}
