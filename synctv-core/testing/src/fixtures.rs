//! Test fixtures (users, rooms, etc.)

use chrono::Utc;
use synctv_core::{
    models::{Room, RoomId, SignupMethod, User, UserId, UserRole, UserStatus},
    service::auth::password::hash_password,
};

/// Builder pattern for creating test users
///
/// # Example
///
/// ```text
/// use synctv_core_testing::TestUser;
///
/// let user = TestUser::new()
///     .with_username("testuser")
///     .with_role(UserRole::Admin)
///     .build();
/// ```
pub struct TestUser {
    username: String,
    email: Option<String>,
    password: Option<String>,
    role: UserRole,
    status: UserStatus,
    email_verified: bool,
}

impl Default for TestUser {
    fn default() -> Self {
        Self::new()
    }
}

impl TestUser {
    /// Creates a new `TestUser` builder with default values
    pub fn new() -> Self {
        Self {
            username: format!("test_user_{}", nanoid::nanoid!(10)),
            email: None,
            password: None,
            role: UserRole::User,
            status: UserStatus::Active,
            email_verified: true,
        }
    }

    /// Sets the username
    pub fn with_username(mut self, username: impl Into<String>) -> Self {
        self.username = username.into();
        self
    }

    /// Sets the email
    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    /// Sets the password (will be hashed)
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    /// Sets the user role
    #[must_use]
    pub const fn with_role(mut self, role: UserRole) -> Self {
        self.role = role;
        self
    }

    /// Sets the user status
    #[must_use]
    pub const fn with_status(mut self, status: UserStatus) -> Self {
        self.status = status;
        self
    }

    /// Sets email verification status
    #[must_use]
    pub const fn with_email_verified(mut self, verified: bool) -> Self {
        self.email_verified = verified;
        self
    }

    /// Builds the User model
    ///
    /// Note: This is a synchronous method for password hashing.
    /// In async tests, use `build_async` instead.
    #[must_use]
    pub fn build(self) -> User {
        let password_hash = self
            .password
            .as_ref()
            .map_or_else(|| "default_hash".to_string(), |pwd| format!("hashed_{pwd}"));

        User {
            id: UserId::new(),
            username: self.username,
            email: self.email,
            password_hash,
            role: self.role,
            status: self.status,
            email_verified: self.email_verified,
            signup_method: SignupMethod::Email,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            password_changed_at: Utc::now(),
            password_version: 0,
            version: 0,
            deleted_at: None,
        }
    }

    /// Builds the User model with async password hashing
    ///
    /// # Example
    ///
    /// ```text
    /// let user = TestUser::new()
    ///     .with_password("SecurePassword123!")
    ///     .build_async()
    ///     .await;
    /// ```
    pub async fn build_async(self) -> User {
        let password_hash = if let Some(pwd) = &self.password {
            hash_password(pwd).await.expect("Failed to hash password")
        } else {
            "default_hash".to_string()
        };

        User {
            id: UserId::new(),
            username: self.username,
            email: self.email,
            password_hash,
            role: self.role,
            status: self.status,
            email_verified: self.email_verified,
            signup_method: SignupMethod::Email,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            password_changed_at: Utc::now(),
            password_version: 0,
            version: 0,
            deleted_at: None,
        }
    }
}

/// Builder pattern for creating test rooms
///
/// # Example
///
/// ```text
/// use synctv_core_testing::TestRoom;
///
/// let room = TestRoom::new()
///     .with_name("Test Room")
///     .with_creator(user_id)
///     .build();
/// ```
pub struct TestRoom {
    name: String,
    created_by: UserId,
    status: synctv_core::models::RoomStatus,
    description: String,
}

impl Default for TestRoom {
    fn default() -> Self {
        Self::new()
    }
}

impl TestRoom {
    /// Creates a new `TestRoom` builder with default values
    pub fn new() -> Self {
        Self {
            name: format!("Test Room {}", nanoid::nanoid!(6)),
            created_by: UserId::new(),
            status: synctv_core::models::RoomStatus::Active,
            description: String::new(),
        }
    }

    /// Sets the room name
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
    }

    /// Sets the creator user ID
    #[must_use]
    pub fn with_creator(mut self, created_by: UserId) -> Self {
        self.created_by = created_by;
        self
    }

    /// Sets the room status
    #[must_use]
    pub const fn with_status(mut self, status: synctv_core::models::RoomStatus) -> Self {
        self.status = status;
        self
    }

    /// Sets the room description
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = description.into();
        self
    }

    /// Builds the Room model
    #[must_use]
    pub fn build(self) -> Room {
        let now = Utc::now();
        Room {
            id: RoomId::new(),
            name: self.name,
            created_by: self.created_by,
            status: self.status,
            description: self.description,
            created_at: now,
            updated_at: now,
            version: 0,
            deleted_at: None,
            is_banned: false,
            last_activity_at: now,
        }
    }
}
