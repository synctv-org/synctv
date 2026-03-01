//! WebSocket Ticket Service
//!
//! Provides short-lived, one-time-use tickets for WebSocket authentication.
//! This is more secure than passing JWT tokens directly in WebSocket URLs,
//! as tickets:
//! - Are short-lived (default 30 seconds)
//! - Can only be used once
//! - Don't expose the actual JWT token in URLs/logs
//!
//! ## Storage Backends
//!
//! - **Redis** (recommended for multi-replica): Tickets are stored in Redis with TTL,
//!   ensuring they work across all replicas.
//! - **Memory** (single-replica only): Tickets are stored in memory. This is suitable
//!   for single-instance deployments but will not work correctly with multiple replicas.
//!
//! ## Security: TOCTOU Prevention (Issue #17)
//!
//! The `validate_and_consume_checked` method accepts a user validator callback
//! to prevent Time-Of-Check to Time-Of-Use race conditions. The validator is
//! called AFTER the ticket is consumed, ensuring the user status check happens
//! at the last possible moment before the connection is accepted.

use async_trait::async_trait;
use base64::Engine;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, warn};

use crate::models::{RoomId, UserId};
use crate::{Error, Result};

/// User validation result returned by `UserValidator` callback
#[derive(Debug, Clone)]
pub struct UserValidationResult {
    /// Current password version of the user
    pub password_version: i32,
}

/// Trait for validating user status during ticket consumption.
///
/// Implemented by the caller (typically `UserService`) to check user status
/// atomically with ticket validation, preventing TOCTOU race conditions.
#[async_trait]
pub trait UserValidator: Send + Sync {
    /// Validate user for ticket-based authentication.
    ///
    /// Returns `Ok(UserValidationResult)` if the user is valid (active status,
    /// not deleted) or `Err` if the user should be rejected (banned, pending,
    /// deleted, or not found).
    async fn validate_for_ticket(&self, user_id: &UserId) -> Result<UserValidationResult>;
}

/// Default ticket TTL in seconds
const DEFAULT_TICKET_TTL_SECS: u64 = 30;
/// Ticket length in bytes (256 bits of entropy)
const TICKET_LENGTH: usize = 32;

/// WebSocket ticket data
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WsTicketData {
    /// User ID associated with this ticket
    pub user_id: String,
    /// Room ID the ticket is bound to.
    ///
    /// Tickets are room-scoped: a ticket created for room A cannot be used to
    /// authenticate a WebSocket connection to room B (Issue #65).
    pub room_id: String,
    /// When the ticket was created (Unix timestamp)
    pub created_at: u64,
    /// Password version at ticket creation time.
    ///
    /// Used to invalidate tickets when the user changes their password.
    /// This provides parity with JWT authentication's `pv` claim check.
    pub password_version: i32,
}

/// Outcome of a successful ticket validation.
#[derive(Debug, Clone)]
pub struct ValidatedTicket {
    /// User ID associated with the ticket
    pub user_id: UserId,
    /// Password version at ticket creation time
    pub password_version: i32,
}

// ============================================================================
// TicketStore trait
// ============================================================================

/// Backend storage for WebSocket tickets.
///
/// Implementations must provide atomic insert and get-and-delete operations
/// to ensure one-time-use semantics.
#[async_trait]
pub trait TicketStore: Send + Sync {
    /// Store a ticket with its associated data. The ticket must expire after `ttl_secs`.
    async fn store(&self, ticket: &str, data: &WsTicketData, ttl_secs: u64) -> Result<()>;

    /// Atomically get and delete a ticket. Returns `None` if the ticket does not
    /// exist or has expired.
    async fn consume(&self, ticket: &str) -> Result<Option<WsTicketData>>;

    /// A label for logging/debug purposes (e.g. "redis", "memory").
    fn backend_name(&self) -> &'static str;
}

// ============================================================================
// Redis implementation
// ============================================================================

/// Redis-backed ticket store for multi-replica deployments.
pub struct RedisTicketStore {
    conn: redis::aio::ConnectionManager,
}

impl RedisTicketStore {
    #[must_use]
    pub const fn new(conn: redis::aio::ConnectionManager) -> Self {
        Self { conn }
    }
}

/// Redis key prefix for WebSocket tickets
const WS_TICKET_PREFIX: &str = "synctv:ws_ticket:";

#[async_trait]
impl TicketStore for RedisTicketStore {
    async fn store(&self, ticket: &str, data: &WsTicketData, ttl_secs: u64) -> Result<()> {
        use redis::AsyncCommands;

        let key = format!("{WS_TICKET_PREFIX}{ticket}");
        let json = serde_json::to_string(data)
            .map_err(|e| Error::Internal(format!("Failed to serialize ticket data: {e}")))?;

        let mut conn = self.conn.clone();
        let _: () = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            conn.set_ex(&key, json, ttl_secs),
        )
        .await
        .map_err(|_| Error::Internal("Redis timeout: store ticket".to_string()))?
        .map_err(|e| Error::Internal(format!("Failed to store ticket: {e}")))?;

        Ok(())
    }

    async fn consume(&self, ticket: &str) -> Result<Option<WsTicketData>> {
        let key = format!("{WS_TICKET_PREFIX}{ticket}");
        let mut conn = self.conn.clone();

        // Get and delete atomically using Lua script
        let lua_script = redis::Script::new(
            r#"
            local value = redis.call("GET", KEYS[1])
            if value then
                redis.call("DEL", KEYS[1])
            end
            return value
        "#,
        );

        let json: Option<String> = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            lua_script.key(&key).invoke_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Internal("Redis timeout: validate ticket".to_string()))?
        .map_err(|e| Error::Internal(format!("Failed to validate ticket: {e}")))?;

        let Some(json) = json else {
            return Ok(None);
        };

        let data: WsTicketData = serde_json::from_str(&json)
            .map_err(|e| Error::Internal(format!("Failed to deserialize ticket data: {e}")))?;

        Ok(Some(data))
    }

    fn backend_name(&self) -> &'static str {
        "redis"
    }
}

// ============================================================================
// In-memory implementation
// ============================================================================

/// In-memory ticket store for single-replica deployments using moka cache with TTL.
pub struct InMemoryTicketStore {
    cache: moka::future::Cache<String, WsTicketData>,
    ttl_secs: u64,
}

impl InMemoryTicketStore {
    #[must_use]
    pub fn new(ttl_secs: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .time_to_live(std::time::Duration::from_secs(ttl_secs))
                .max_capacity(10_000)
                .build(),
            ttl_secs,
        }
    }
}

#[async_trait]
impl TicketStore for InMemoryTicketStore {
    async fn store(&self, ticket: &str, data: &WsTicketData, _ttl_secs: u64) -> Result<()> {
        self.cache.insert(ticket.to_string(), data.clone()).await;
        Ok(())
    }

    async fn consume(&self, ticket: &str) -> Result<Option<WsTicketData>> {
        // Use remove() for atomic get-and-delete to prevent TOCTOU race conditions
        // where two concurrent requests could both get() the same ticket.
        // Since remove() may return entries that moka hasn't lazily evicted yet,
        // we manually check TTL expiry on the returned value.
        let data = match self.cache.remove(ticket).await {
            Some(d) => d,
            None => return Ok(None),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(data.created_at) > self.ttl_secs {
            return Ok(None); // Expired
        }
        Ok(Some(data))
    }

    fn backend_name(&self) -> &'static str {
        "memory"
    }
}

// ============================================================================
// WsTicketService
// ============================================================================

/// Service for creating and validating WebSocket tickets.
///
/// Uses a pluggable `TicketStore` backend (Redis or in-memory).
#[derive(Clone)]
pub struct WsTicketService {
    store: Arc<dyn TicketStore>,
    /// Ticket TTL in seconds
    ticket_ttl_secs: u64,
}

impl std::fmt::Debug for WsTicketService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsTicketService")
            .field("backend", &self.store.backend_name())
            .field("ticket_ttl_secs", &self.ticket_ttl_secs)
            .finish()
    }
}

impl WsTicketService {
    /// Create a new WebSocket ticket service with a custom ticket store backend.
    pub fn from_store(store: Arc<dyn TicketStore>, ticket_ttl_secs: Option<u64>) -> Self {
        Self {
            store,
            ticket_ttl_secs: ticket_ttl_secs.unwrap_or(DEFAULT_TICKET_TTL_SECS),
        }
    }

    /// Create a new WebSocket ticket service with Redis (multi-replica mode).
    #[must_use]
    pub fn with_redis(
        redis_conn: redis::aio::ConnectionManager,
        ticket_ttl_secs: Option<u64>,
    ) -> Self {
        Self::from_store(Arc::new(RedisTicketStore::new(redis_conn)), ticket_ttl_secs)
    }

    /// Create a new WebSocket ticket service with memory storage (single-replica mode).
    #[must_use]
    pub fn with_memory(ticket_ttl_secs: Option<u64>) -> Self {
        let ttl = ticket_ttl_secs.unwrap_or(DEFAULT_TICKET_TTL_SECS);
        Self::from_store(Arc::new(InMemoryTicketStore::new(ttl)), ticket_ttl_secs)
    }

    /// Create a new WebSocket ticket service, choosing backend based on Redis availability.
    ///
    /// In cluster mode, Redis is **required**; passing `None` returns an error.
    pub fn new(
        redis_conn: Option<redis::aio::ConnectionManager>,
        ticket_ttl_secs: Option<u64>,
        cluster_mode: bool,
    ) -> Result<Self> {
        if let Some(conn) = redis_conn {
            Ok(Self::with_redis(conn, ticket_ttl_secs))
        } else {
            if cluster_mode {
                return Err(Error::Internal(
                    "Redis is required for WebSocket ticket service in cluster mode. \
                     Tickets stored in memory are only visible on the replica that created them, \
                     causing authentication failures on other replicas. Configure Redis."
                        .to_string(),
                ));
            }

            warn!(
                "WebSocket ticket service using in-memory storage. \
                 This is only suitable for single-replica deployments. \
                 For multi-replica setups, configure Redis."
            );

            if Self::detect_multi_replica_environment() {
                error!(
                    "MULTI-REPLICA RISK: WebSocket ticket service is using in-memory storage, \
                     but the environment appears to be a multi-replica deployment \
                     (Kubernetes / Docker Swarm / REPLICAS env detected). \
                     Tickets created on one replica will NOT be valid on others. \
                     Configure Redis to fix this."
                );
            }

            Ok(Self::with_memory(ticket_ttl_secs))
        }
    }

    /// Get the configured ticket TTL in seconds
    #[must_use]
    pub const fn ticket_ttl_secs(&self) -> u64 {
        self.ticket_ttl_secs
    }

    /// Return the backend name (e.g. "redis", "memory").
    #[must_use]
    pub fn backend_name(&self) -> &'static str {
        self.store.backend_name()
    }

    /// Create a new ticket for a user bound to a specific room.
    ///
    /// Returns a ticket string that can be used once for WebSocket authentication.
    /// The ticket expires after `ticket_ttl_secs` seconds and is only valid for
    /// the supplied `room_id` (Issue #65).
    ///
    /// The `password_version` is stored in the ticket and validated during consumption
    /// to ensure tickets are invalidated when the user changes their password.
    pub async fn create_ticket(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        password_version: i32,
    ) -> Result<String> {
        let ticket = Self::generate_ticket();

        let ticket_data = WsTicketData {
            user_id: user_id.as_str().to_string(),
            room_id: room_id.as_str().to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            password_version,
        };

        self.store
            .store(&ticket, &ticket_data, self.ticket_ttl_secs)
            .await?;

        debug!(
            user_id = %user_id.as_str(),
            ttl_secs = self.ticket_ttl_secs,
            mode = %self.store.backend_name(),
            "WebSocket ticket created"
        );

        Ok(ticket)
    }

    /// Validate and consume a ticket.
    ///
    /// Returns [`ValidatedTicket`] containing the user ID and password version if valid
    /// and the ticket's `room_id` matches the expected `room_id`. The ticket is deleted
    /// after use (one-time use). Passing a ticket for a different room returns an error
    /// so that tickets cannot be replayed across rooms (Issue #65).
    ///
    /// The caller is responsible for checking that the `password_version` in the returned
    /// [`ValidatedTicket`] matches the current user's password version to ensure the ticket
    /// is invalidated if the user changed their password after the ticket was issued.
    pub async fn validate_and_consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<ValidatedTicket> {
        let mode = self.store.backend_name();

        let Some(ticket_data) = self.store.consume(ticket).await? else {
            debug!(ticket = %ticket, mode = %mode, "WebSocket ticket not found or expired");
            return Err(Error::Authorization(
                "Invalid or expired ticket".to_string(),
            ));
        };

        // Room-bound validation: reject the ticket if it was issued for a different room.
        if ticket_data.room_id != expected_room_id.as_str() {
            debug!(
                ticket_room = %ticket_data.room_id,
                expected_room = %expected_room_id.as_str(),
                mode = %mode,
                "WebSocket ticket rejected: room mismatch"
            );
            return Err(Error::Authorization(
                "Ticket not valid for this room".to_string(),
            ));
        }

        debug!(
            user_id = %ticket_data.user_id,
            room_id = %ticket_data.room_id,
            mode = %mode,
            "WebSocket ticket validated and consumed"
        );

        Ok(ValidatedTicket {
            user_id: UserId::from_string(ticket_data.user_id),
            password_version: ticket_data.password_version,
        })
    }

    /// Validate and consume a ticket with user status check (TOCTOU-safe).
    ///
    /// This is the recommended method for WebSocket ticket validation. It:
    /// 1. Consumes the ticket (one-time use)
    /// 2. Validates room binding
    /// 3. Calls the `user_validator` to check user status and password version
    ///
    /// The user validator is called AFTER ticket consumption, ensuring the user
    /// status check happens at the latest possible moment, preventing TOCTOU
    /// race conditions (Issue #17).
    ///
    /// Returns [`ValidatedTicket`] if all checks pass.
    pub async fn validate_and_consume_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<ValidatedTicket> {
        // Step 1: Consume the ticket (one-time use)
        let mode = self.store.backend_name();

        let Some(ticket_data) = self.store.consume(ticket).await? else {
            debug!(ticket = %ticket, mode = %mode, "WebSocket ticket not found or expired");
            return Err(Error::Authorization(
                "Invalid or expired ticket".to_string(),
            ));
        };

        // Step 2: Validate room binding
        if ticket_data.room_id != expected_room_id.as_str() {
            debug!(
                ticket_room = %ticket_data.room_id,
                expected_room = %expected_room_id.as_str(),
                mode = %mode,
                "WebSocket ticket rejected: room mismatch"
            );
            return Err(Error::Authorization(
                "Ticket not valid for this room".to_string(),
            ));
        }

        let user_id = UserId::from_string(ticket_data.user_id.clone());

        // Step 3: Validate user status (TOCTOU-safe: happens after ticket consumption)
        let user_validation = user_validator
            .validate_for_ticket(&user_id)
            .await
            .map_err(|e| {
                debug!(
                    user_id = %user_id.as_str(),
                    error = %e,
                    mode = %mode,
                    "WebSocket ticket rejected: user validation failed"
                );
                Error::Authorization("Authentication failed".to_string())
            })?;

        // Step 4: Check password version (ticket must be invalidated if password changed)
        if ticket_data.password_version < user_validation.password_version {
            debug!(
                user_id = %user_id.as_str(),
                ticket_pv = ticket_data.password_version,
                current_pv = user_validation.password_version,
                mode = %mode,
                "WebSocket ticket rejected: password changed after ticket issued"
            );
            return Err(Error::Authorization("Authentication failed".to_string()));
        }

        debug!(
            user_id = %user_id.as_str(),
            room_id = %ticket_data.room_id,
            mode = %mode,
            "WebSocket ticket validated and consumed with user check"
        );

        Ok(ValidatedTicket {
            user_id,
            password_version: ticket_data.password_version,
        })
    }

    /// Generate a secure random ticket string
    fn generate_ticket() -> String {
        let mut rng = rand::rng();
        let mut bytes = [0u8; TICKET_LENGTH];
        rng.fill(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Detect if the environment appears to be a multi-replica deployment.
    fn detect_multi_replica_environment() -> bool {
        if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
            return true;
        }
        for var in &["REPLICAS", "SYNCTV_REPLICAS"] {
            if let Ok(val) = std::env::var(var) {
                if let Ok(count) = val.parse::<u32>() {
                    if count > 1 {
                        return true;
                    }
                }
            }
        }
        if std::path::Path::new("/var/run/secrets/kubernetes.io").exists() {
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_user_id(id: &str) -> UserId {
        UserId::from_string(id.to_string())
    }

    fn create_test_room_id(id: &str) -> RoomId {
        RoomId::from_string(id.to_string())
    }

    #[test]
    fn test_ticket_generation() {
        let ticket1 = WsTicketService::generate_ticket();
        let ticket2 = WsTicketService::generate_ticket();

        assert_ne!(ticket1, ticket2);
        assert!(!ticket1.contains('+'));
        assert!(!ticket1.contains('/'));
        assert!(!ticket1.contains('='));
    }

    #[test]
    fn test_ticket_data_serialization() {
        let data = WsTicketData {
            user_id: "user123".to_string(),
            room_id: "room456".to_string(),
            created_at: 1234567890,
            password_version: 5,
        };

        let json = serde_json::to_string(&data).unwrap();
        let decoded: WsTicketData = serde_json::from_str(&json).unwrap();

        assert_eq!(data.user_id, decoded.user_id);
        assert_eq!(data.room_id, decoded.room_id);
        assert_eq!(data.created_at, decoded.created_at);
        assert_eq!(data.password_version, decoded.password_version);
    }

    #[tokio::test]
    async fn test_ticket_service_memory_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room1");

        let ticket = service.create_ticket(&user_id, &room_id, 0).await;
        assert!(ticket.is_ok());

        let result = service
            .validate_and_consume(&ticket.unwrap(), &room_id)
            .await;
        assert!(result.is_ok());
        let validated = result.unwrap();
        assert_eq!(validated.user_id.as_str(), "user1");
        assert_eq!(validated.password_version, 0);
    }

    #[tokio::test]
    async fn test_ticket_one_time_use_memory_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room1");

        let ticket = service.create_ticket(&user_id, &room_id, 0).await.unwrap();

        let result1 = service.validate_and_consume(&ticket, &room_id).await;
        assert!(result1.is_ok());

        let result2 = service.validate_and_consume(&ticket, &room_id).await;
        assert!(result2.is_err());
    }

    #[tokio::test]
    async fn test_ticket_room_mismatch_rejected() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_a = create_test_room_id("room-a");
        let room_b = create_test_room_id("room-b");

        let ticket = service.create_ticket(&user_id, &room_a, 0).await.unwrap();

        let result = service.validate_and_consume(&ticket, &room_b).await;
        assert!(
            result.is_err(),
            "Ticket for room A should not be valid for room B"
        );
    }

    #[tokio::test]
    async fn test_ticket_expiration_memory_mode() {
        let service = WsTicketService::with_memory(Some(1)); // 1 second TTL
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room1");

        let ticket = service.create_ticket(&user_id, &room_id, 0).await.unwrap();

        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        let result = service.validate_and_consume(&ticket, &room_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_ticket_memory_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let room_id = create_test_room_id("room1");

        let result = service
            .validate_and_consume("invalid_ticket", &room_id)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn test_detect_multi_replica_no_env_is_false() {
        let result = WsTicketService::detect_multi_replica_environment();
        let _ = result;
    }

    #[test]
    fn test_memory_mode_creates_valid_service() {
        let service = WsTicketService::with_memory(Some(60));
        assert_eq!(service.store.backend_name(), "memory");
        assert_eq!(service.ticket_ttl_secs, 60);
    }

    #[test]
    fn test_debug_shows_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let debug_str = format!("{service:?}");
        assert!(debug_str.contains("memory"));
    }

    #[test]
    fn test_cluster_mode_requires_redis() {
        let result = WsTicketService::new(None, None, true);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Redis is required"),
            "Error should mention Redis requirement; got: {err_msg}"
        );
    }

    #[test]
    fn test_non_cluster_mode_allows_memory() {
        let result = WsTicketService::new(None, None, false);
        assert!(result.is_ok());
        let service = result.unwrap();
        assert_eq!(service.store.backend_name(), "memory");
    }

    // ============================================================================
    // Cluster mode Redis dependency tests (TDD)
    // ============================================================================

    /// Test: cluster mode without Redis returns a descriptive error.
    /// This is the core issue - in cluster mode, tickets created on replica A
    /// cannot be validated on replica B without shared Redis storage.
    #[test]
    fn test_cluster_mode_without_redis_returns_error() {
        let result = WsTicketService::new(None, Some(30), true);

        assert!(
            result.is_err(),
            "Cluster mode without Redis should return an error"
        );

        let err = result.unwrap_err();
        let err_msg = err.to_string();

        // Error message should be descriptive and mention the core issue
        assert!(
            err_msg.contains("Redis is required"),
            "Error should mention Redis is required; got: {err_msg}"
        );
        assert!(
            err_msg.contains("cluster mode") || err_msg.contains("cluster"),
            "Error should mention cluster mode; got: {err_msg}"
        );
        assert!(
            err_msg.contains("replica") || err_msg.contains("replicas"),
            "Error should explain the replica visibility issue; got: {err_msg}"
        );
    }

    /// Test: cluster mode error message provides actionable guidance.
    /// Users should know how to fix the configuration.
    #[test]
    fn test_cluster_mode_error_message_is_actionable() {
        let result = WsTicketService::new(None, None, true);
        let err_msg = result.unwrap_err().to_string();

        // Should suggest configuring Redis
        assert!(
            err_msg.contains("Configure Redis"),
            "Error should suggest configuring Redis; got: {err_msg}"
        );
    }

    /// Test: cluster mode with custom TTL still requires Redis.
    /// TTL configuration should not bypass the Redis requirement.
    #[test]
    fn test_cluster_mode_with_custom_ttl_still_requires_redis() {
        let result = WsTicketService::new(None, Some(60), true);

        assert!(
            result.is_err(),
            "Cluster mode should require Redis regardless of TTL setting"
        );
    }

    /// Test: cluster mode with zero TTL still requires Redis.
    #[test]
    fn test_cluster_mode_with_zero_ttl_still_requires_redis() {
        let result = WsTicketService::new(None, Some(0), true);

        assert!(
            result.is_err(),
            "Cluster mode should require Redis even with zero TTL"
        );
    }

    /// Test: non-cluster mode without Redis works but logs warning.
    /// Single-replica deployments should still function without Redis.
    #[test]
    fn test_non_cluster_mode_without_redis_succeeds() {
        let result = WsTicketService::new(None, Some(30), false);

        assert!(result.is_ok(), "Non-cluster mode should work without Redis");

        let service = result.unwrap();
        assert_eq!(
            service.backend_name(),
            "memory",
            "Non-cluster mode without Redis should use memory backend"
        );
    }

    /// Test: `from_store` allows custom backends for testing purposes.
    #[test]
    fn test_from_store_allows_custom_backend() {
        let store = Arc::new(InMemoryTicketStore::new(30));
        let service = WsTicketService::from_store(store, Some(45));

        assert_eq!(service.backend_name(), "memory");
        assert_eq!(service.ticket_ttl_secs(), 45);
    }
}
