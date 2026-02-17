// Provider Context
//
// Contains all information needed for provider execution

use sqlx::PgPool;

/// Provider execution context
///
/// Provides access to database, Redis, user information, and other resources
/// needed by providers to generate playback information.
#[derive(Clone)]
pub struct ProviderContext<'a> {
    /// User ID requesting playback (optional)
    pub user_id: Option<&'a str>,

    /// Room ID (optional)
    pub room_id: Option<&'a str>,

    /// Base URL for generating proxy URLs
    pub base_url: Option<&'a str>,

    /// Cache key prefix (e.g., "synctv")
    pub key_prefix: &'a str,

    /// Database connection pool (optional)
    pub db: Option<&'a PgPool>,

    /// Redis connection manager (optional)
    pub redis: Option<&'a redis::aio::ConnectionManager>,
}

impl<'a> ProviderContext<'a> {
    /// Create new context with defaults
    #[must_use] 
    pub const fn new(key_prefix: &'a str) -> Self {
        Self {
            user_id: None,
            room_id: None,
            base_url: None,
            key_prefix,
            db: None,
            redis: None,
        }
    }

    /// Set user ID
    #[must_use] 
    pub const fn with_user_id(mut self, user_id: &'a str) -> Self {
        self.user_id = Some(user_id);
        self
    }

    /// Set room ID
    #[must_use] 
    pub const fn with_room_id(mut self, room_id: &'a str) -> Self {
        self.room_id = Some(room_id);
        self
    }

    /// Set base URL
    #[must_use] 
    pub const fn with_base_url(mut self, base_url: &'a str) -> Self {
        self.base_url = Some(base_url);
        self
    }

    /// Set database pool
    #[must_use] 
    pub const fn with_db(mut self, db: &'a PgPool) -> Self {
        self.db = Some(db);
        self
    }

    /// Set Redis connection manager
    #[must_use]
    pub const fn with_redis(mut self, redis: &'a redis::aio::ConnectionManager) -> Self {
        self.redis = Some(redis);
        self
    }

    /// Validate that all required fields are present for playback generation.
    ///
    /// Returns an error listing missing required fields (`base_url`, `db`).
    /// Optional fields (`user_id`, `room_id`, `redis`) are not checked.
    pub fn validate(&self) -> Result<(), crate::Error> {
        let mut missing = Vec::new();
        if self.base_url.is_none() {
            missing.push("base_url");
        }
        if self.db.is_none() {
            missing.push("db");
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(crate::Error::InvalidInput(format!(
                "ProviderContext missing required fields: {}",
                missing.join(", ")
            )))
        }
    }
}
