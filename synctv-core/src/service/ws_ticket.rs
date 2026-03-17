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
//! so callers can reject banned/deleted users without burning an otherwise
//! valid one-time ticket on a retriable authorization failure.

use async_trait::async_trait;
use base64::Engine;
use rand::RngExt;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, warn};

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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Outcome of a successful pre-validation before the ticket is finally consumed.
#[derive(Debug, Clone)]
pub struct PendingValidatedTicket {
    /// User ID associated with the ticket
    pub user_id: UserId,
    /// Password version at ticket creation time
    pub password_version: i32,
    ticket_data: WsTicketData,
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

    /// Load a ticket scoped to the expected room without consuming it.
    ///
    /// Returns `None` if the ticket does not exist or has expired.
    async fn load(&self, ticket: &str, expected_room_id: &RoomId) -> Result<Option<WsTicketData>>;

    /// Try to claim a ticket after validation succeeds.
    ///
    /// The claim must only succeed if the stored ticket still matches the exact
    /// ticket data that was previously loaded and validated by the caller.
    /// This closes the `load -> validate -> consume` TOCTOU window by turning
    /// the final delete step into a compare-and-delete.
    ///
    /// Returns `true` if the ticket was successfully consumed by this caller,
    /// `false` if it had already expired, been consumed concurrently, or no
    /// longer matched the validated value.
    async fn claim(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        expected_ticket: &WsTicketData,
    ) -> Result<bool>;

    /// Atomically get and delete a ticket scoped to the expected room.
    /// Returns `None` if the ticket does not exist or has expired.
    async fn consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<Option<WsTicketData>>;

    /// A label for logging/debug purposes (e.g. "redis", "memory").
    fn backend_name(&self) -> &'static str;
}

// ============================================================================
// Redis implementation
// ============================================================================

/// Redis-backed ticket store for multi-replica deployments.
///
/// Uses a shared `Arc<RwLock<ConnectionManager>>` so that in Sentinel mode the
/// background health check can hot-swap the inner connection on failover and
/// this store automatically picks up the new master.
pub struct RedisTicketStore {
    shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
    key_prefix: String,
}

impl RedisTicketStore {
    fn normalize_key_prefix(prefix: impl Into<String>) -> String {
        let key_prefix = prefix.into();
        if key_prefix.is_empty() || key_prefix.ends_with(':') {
            key_prefix
        } else {
            format!("{key_prefix}:")
        }
    }

    #[must_use]
    pub fn new(
        shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: impl Into<String>,
    ) -> Self {
        Self {
            shared_conn,
            key_prefix: Self::normalize_key_prefix(key_prefix),
        }
    }

    async fn conn(&self) -> redis::aio::ConnectionManager {
        self.shared_conn.read().await.clone()
    }

    fn redis_key(&self, ticket: &str, room_id: &RoomId) -> String {
        format!(
            "{}ws_ticket:{}:{}",
            self.key_prefix,
            room_id.as_str(),
            ticket
        )
    }
}

#[async_trait]
impl TicketStore for RedisTicketStore {
    async fn store(&self, ticket: &str, data: &WsTicketData, ttl_secs: u64) -> Result<()> {
        use redis::AsyncCommands;

        let room_id = RoomId::from_string(data.room_id.clone());
        let key = self.redis_key(ticket, &room_id);
        let json = serde_json::to_string(data)
            .map_err(|e| Error::Internal(format!("Failed to serialize ticket data: {e}")))?;

        let mut conn = self.conn().await;
        let _: () = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            conn.set_ex(&key, json, ttl_secs),
        )
        .await
        .map_err(|_| Error::Internal("Redis timeout: store ticket".to_string()))?
        .map_err(|e| Error::Internal(format!("Failed to store ticket: {e}")))?;

        Ok(())
    }

    async fn load(&self, ticket: &str, expected_room_id: &RoomId) -> Result<Option<WsTicketData>> {
        use redis::AsyncCommands;

        let key = self.redis_key(ticket, expected_room_id);
        let mut conn = self.conn().await;

        let json: Option<String> = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            conn.get(&key),
        )
        .await
        .map_err(|_| Error::Internal("Redis timeout: load ticket".to_string()))?
        .map_err(|e| Error::Internal(format!("Failed to load ticket: {e}")))?;

        let Some(json) = json else {
            return Ok(None);
        };

        let data: WsTicketData = serde_json::from_str(&json)
            .map_err(|e| Error::Internal(format!("Failed to deserialize ticket data: {e}")))?;

        Ok(Some(data))
    }

    async fn claim(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        expected_ticket: &WsTicketData,
    ) -> Result<bool> {
        let key = self.redis_key(ticket, expected_room_id);
        let mut conn = self.conn().await;
        let expected_json = serde_json::to_string(expected_ticket)
            .map_err(|e| Error::Internal(format!("Failed to serialize ticket data: {e}")))?;

        let deleted: i64 = tokio::time::timeout(
            crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
            redis::Script::new(
                r#"
                local value = redis.call("GET", KEYS[1])
                if not value then
                    return 0
                end
                if value ~= ARGV[1] then
                    return 0
                end
                redis.call("DEL", KEYS[1])
                return 1
            "#,
            )
            .key(&key)
            .arg(&expected_json)
            .invoke_async(&mut conn),
        )
        .await
        .map_err(|_| Error::Internal("Redis timeout: claim ticket".to_string()))?
        .map_err(|e| Error::Internal(format!("Failed to claim ticket: {e}")))?;

        Ok(deleted > 0)
    }

    async fn consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<Option<WsTicketData>> {
        let key = self.redis_key(ticket, expected_room_id);
        let mut conn = self.conn().await;

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

/// Wrapper that pairs ticket data with its per-entry TTL for moka's `Expiry` trait.
#[derive(Clone)]
struct TtlTicketData {
    data: WsTicketData,
    ttl: std::time::Duration,
}

/// Moka `Expiry` implementation that uses the per-entry TTL.
struct TicketEntryExpiry;

impl moka::Expiry<String, TtlTicketData> for TicketEntryExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &TtlTicketData,
        _current_time: std::time::Instant,
    ) -> Option<std::time::Duration> {
        Some(value.ttl)
    }
}

/// In-memory ticket store for single-replica deployments using moka cache with per-entry TTL.
pub struct InMemoryTicketStore {
    cache: moka::future::Cache<String, TtlTicketData>,
}

impl InMemoryTicketStore {
    #[must_use]
    pub fn new(_ttl_secs: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .expire_after(TicketEntryExpiry)
                .max_capacity(10_000)
                .build(),
        }
    }
}

#[async_trait]
impl TicketStore for InMemoryTicketStore {
    async fn store(&self, ticket: &str, data: &WsTicketData, ttl_secs: u64) -> Result<()> {
        self.cache
            .insert(
                format!("{}:{ticket}", data.room_id),
                TtlTicketData {
                    data: data.clone(),
                    ttl: std::time::Duration::from_secs(ttl_secs),
                },
            )
            .await;
        Ok(())
    }

    async fn load(&self, ticket: &str, expected_room_id: &RoomId) -> Result<Option<WsTicketData>> {
        let cache_key = format!("{}:{ticket}", expected_room_id.as_str());
        let Some(entry) = self.cache.get(&cache_key).await else {
            return Ok(None);
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(entry.data.created_at) > entry.ttl.as_secs() {
            self.cache.remove(&cache_key).await;
            return Ok(None);
        }

        Ok(Some(entry.data))
    }

    async fn claim(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        expected_ticket: &WsTicketData,
    ) -> Result<bool> {
        let cache_key = format!("{}:{ticket}", expected_room_id.as_str());
        let Some(entry) = self.cache.get(&cache_key).await else {
            return Ok(false);
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(entry.data.created_at) > entry.ttl.as_secs() {
            self.cache.remove(&cache_key).await;
            return Ok(false);
        }
        if entry.data != *expected_ticket {
            return Ok(false);
        }

        let Some(removed) = self.cache.remove(&cache_key).await else {
            return Ok(false);
        };
        if now.saturating_sub(removed.data.created_at) > removed.ttl.as_secs() {
            return Ok(false);
        }

        Ok(removed.data == *expected_ticket)
    }

    async fn consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<Option<WsTicketData>> {
        // Use remove() for atomic get-and-delete to prevent TOCTOU race conditions.
        // Since moka uses lazy eviction, remove() may return entries that haven't
        // been evicted yet, so we manually check TTL expiry on the returned value.
        let cache_key = format!("{}:{ticket}", expected_room_id.as_str());
        let entry = match self.cache.remove(&cache_key).await {
            Some(e) => e,
            None => return Ok(None),
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(entry.data.created_at) > entry.ttl.as_secs() {
            return Ok(None);
        }
        Ok(Some(entry.data))
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
    ///
    /// Accepts a shared `Arc<RwLock<ConnectionManager>>` that follows Sentinel failover.
    #[must_use]
    pub fn with_redis(
        shared_conn: Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>,
        key_prefix: impl Into<String>,
        ticket_ttl_secs: Option<u64>,
    ) -> Self {
        Self::from_store(
            Arc::new(RedisTicketStore::new(shared_conn, key_prefix)),
            ticket_ttl_secs,
        )
    }

    /// Create a new WebSocket ticket service with memory storage (single-replica mode).
    #[must_use]
    pub fn with_memory(ticket_ttl_secs: Option<u64>) -> Self {
        let ttl = ticket_ttl_secs.unwrap_or(DEFAULT_TICKET_TTL_SECS);
        Self::from_store(Arc::new(InMemoryTicketStore::new(ttl)), ticket_ttl_secs)
    }

    /// Create a new WebSocket ticket service, choosing backend based on Redis availability.
    ///
    /// Backend capability decisions belong to configuration validation and
    /// startup wiring. This constructor only maps an already-chosen storage
    /// dependency to the correct implementation.
    #[must_use]
    pub fn new(
        redis_conn: Option<Arc<tokio::sync::RwLock<redis::aio::ConnectionManager>>>,
        key_prefix: impl Into<String>,
        ticket_ttl_secs: Option<u64>,
    ) -> Self {
        if let Some(shared_conn) = redis_conn {
            Self::with_redis(shared_conn, key_prefix, ticket_ttl_secs)
        } else {
            warn!(
                "WebSocket ticket service using in-memory storage. \
                 This is only suitable for deployments that intentionally run without cluster-backed tickets."
            );
            Self::with_memory(ticket_ttl_secs)
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

        let Some(ticket_data) = self.store.consume(ticket, expected_room_id).await? else {
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

    /// Validate and consume a ticket with user status check.
    ///
    /// This is the recommended method for WebSocket ticket validation. It:
    /// 1. Validates room-scoped ticket presence
    /// 2. Calls the `user_validator` to check user status and password version
    /// 3. Consumes the ticket only after all validation succeeds
    ///
    /// Returns [`ValidatedTicket`] if all checks pass.
    pub async fn validate_and_consume_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<ValidatedTicket> {
        let mode = self.store.backend_name();

        let pending = self
            .validate_checked(ticket, expected_room_id, user_validator)
            .await?;

        if !self
            .store
            .claim(ticket, expected_room_id, &pending.ticket_data)
            .await?
        {
            debug!(
                ticket = %ticket,
                mode = %mode,
                "WebSocket ticket already consumed during checked validation"
            );
            return Err(Error::Authorization(
                "Invalid or expired ticket".to_string(),
            ));
        }

        debug!(
            user_id = %pending.user_id.as_str(),
            room_id = %pending.ticket_data.room_id,
            mode = %mode,
            "WebSocket ticket validated and consumed with user check"
        );

        Ok(ValidatedTicket {
            user_id: pending.user_id,
            password_version: pending.password_version,
        })
    }

    /// Validate a ticket and user state without consuming the ticket yet.
    ///
    /// This is intended for handshake flows that still have additional checks
    /// before the connection is definitively established. The caller must later
    /// call [`Self::consume_prevalidated`] to preserve one-time-use semantics.
    pub async fn validate_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<PendingValidatedTicket> {
        let mode = self.store.backend_name();

        let Some(ticket_data) = self.store.load(ticket, expected_room_id).await? else {
            debug!(ticket = %ticket, mode = %mode, "WebSocket ticket not found or expired");
            return Err(Error::Authorization(
                "Invalid or expired ticket".to_string(),
            ));
        };

        let user_id = UserId::from_string(ticket_data.user_id.clone());

        // Room binding is enforced by the storage key. A ticket fetched here
        // is already scoped to `expected_room_id`.
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

        // Check password version after loading the current user state.
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
            "WebSocket ticket prevalidated with user check"
        );

        Ok(PendingValidatedTicket {
            user_id,
            password_version: ticket_data.password_version,
            ticket_data,
        })
    }

    /// Consume a previously prevalidated ticket.
    pub async fn consume_prevalidated(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        pending: &PendingValidatedTicket,
    ) -> Result<ValidatedTicket> {
        let mode = self.store.backend_name();

        if !self
            .store
            .claim(ticket, expected_room_id, &pending.ticket_data)
            .await?
        {
            debug!(
                ticket = %ticket,
                mode = %mode,
                "WebSocket ticket already consumed before final handshake commit"
            );
            return Err(Error::Authorization(
                "Invalid or expired ticket".to_string(),
            ));
        }

        debug!(
            user_id = %pending.user_id.as_str(),
            room_id = %pending.ticket_data.room_id,
            mode = %mode,
            "WebSocket ticket consumed after prevalidated handshake succeeded"
        );

        Ok(ValidatedTicket {
            user_id: pending.user_id.clone(),
            password_version: pending.password_version,
        })
    }

    /// Generate a secure random ticket string
    fn generate_ticket() -> String {
        let mut rng = rand::rng();
        let mut bytes = [0u8; TICKET_LENGTH];
        rng.fill(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
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

    struct StaticUserValidator {
        result: std::result::Result<UserValidationResult, &'static str>,
    }

    #[async_trait]
    impl UserValidator for StaticUserValidator {
        async fn validate_for_ticket(&self, _user_id: &UserId) -> Result<UserValidationResult> {
            self.result
                .clone()
                .map_err(|message| Error::Authorization((*message).to_string()))
        }
    }

    #[tokio::test]
    async fn test_ticket_room_mismatch_does_not_consume_ticket() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_a = create_test_room_id("room-a");
        let room_b = create_test_room_id("room-b");

        let ticket = service.create_ticket(&user_id, &room_a, 7).await.unwrap();

        let wrong_room_result = service.validate_and_consume(&ticket, &room_b).await;
        assert!(
            matches!(wrong_room_result, Err(Error::Authorization(_))),
            "room mismatch should be rejected"
        );

        let correct_room_result = service.validate_and_consume(&ticket, &room_a).await;
        assert!(
            correct_room_result.is_ok(),
            "room mismatch must not consume the ticket"
        );
        let validated = correct_room_result.unwrap();
        assert_eq!(validated.user_id.as_str(), "user1");
        assert_eq!(validated.password_version, 7);
    }

    #[tokio::test]
    async fn test_ticket_user_validation_failure_does_not_consume_ticket() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room-a");
        let ticket = service.create_ticket(&user_id, &room_id, 4).await.unwrap();

        let rejecting_validator = StaticUserValidator {
            result: Err("banned"),
        };
        let allow_validator = StaticUserValidator {
            result: Ok(UserValidationResult {
                password_version: 4,
            }),
        };

        let first_result = service
            .validate_and_consume_checked(&ticket, &room_id, &rejecting_validator)
            .await;
        assert!(
            matches!(first_result, Err(Error::Authorization(_))),
            "user validation failure should reject the ticket"
        );

        let second_result = service
            .validate_and_consume_checked(&ticket, &room_id, &allow_validator)
            .await;
        assert!(
            second_result.is_ok(),
            "user validation rejection must not consume the ticket"
        );
        let validated = second_result.unwrap();
        assert_eq!(validated.user_id.as_str(), "user1");
        assert_eq!(validated.password_version, 4);
    }

    #[tokio::test]
    async fn test_ticket_checked_validation_is_still_one_time_use() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room-a");
        let ticket = service.create_ticket(&user_id, &room_id, 2).await.unwrap();

        let allow_validator = StaticUserValidator {
            result: Ok(UserValidationResult {
                password_version: 2,
            }),
        };

        let first_result = service
            .validate_and_consume_checked(&ticket, &room_id, &allow_validator)
            .await;
        assert!(
            first_result.is_ok(),
            "first checked validation should succeed"
        );

        let second_result = service
            .validate_and_consume_checked(&ticket, &room_id, &allow_validator)
            .await;
        assert!(
            matches!(second_result, Err(Error::Authorization(_))),
            "checked validation must still enforce one-time use"
        );
    }

    #[tokio::test]
    async fn test_ticket_prevalidation_does_not_consume_until_commit() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room-prevalidated");
        let ticket = service.create_ticket(&user_id, &room_id, 5).await.unwrap();

        let allow_validator = StaticUserValidator {
            result: Ok(UserValidationResult {
                password_version: 5,
            }),
        };

        service
            .validate_checked(&ticket, &room_id, &allow_validator)
            .await
            .expect("prevalidation should succeed");

        let still_valid = service
            .validate_and_consume(&ticket, &room_id)
            .await
            .expect("prevalidation alone must not consume the ticket");
        assert_eq!(still_valid.user_id.as_str(), "user1");
        assert_eq!(still_valid.password_version, 5);

        let second_ticket = service.create_ticket(&user_id, &room_id, 5).await.unwrap();
        let pending = service
            .validate_checked(&second_ticket, &room_id, &allow_validator)
            .await
            .expect("second prevalidation should succeed");
        let committed = service
            .consume_prevalidated(&second_ticket, &room_id, &pending)
            .await
            .expect("commit should consume the prevalidated ticket");
        assert_eq!(committed.user_id.as_str(), "user1");

        let consumed_again = service.validate_and_consume(&second_ticket, &room_id).await;
        assert!(
            matches!(consumed_again, Err(Error::Authorization(_))),
            "committed prevalidated ticket must become one-time-use"
        );
    }

    #[tokio::test]
    async fn test_ticket_checked_validation_concurrent_consumption_only_succeeds_once() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room-concurrent");
        let ticket = service.create_ticket(&user_id, &room_id, 2).await.unwrap();

        let validator = Arc::new(StaticUserValidator {
            result: Ok(UserValidationResult {
                password_version: 2,
            }),
        });

        let mut handles = Vec::new();
        for _ in 0..8 {
            let service = service.clone();
            let ticket = ticket.clone();
            let room_id = room_id.clone();
            let validator = validator.clone();
            handles.push(tokio::spawn(async move {
                service
                    .validate_and_consume_checked(&ticket, &room_id, &*validator)
                    .await
            }));
        }

        let results: Vec<_> = futures::future::join_all(handles)
            .await
            .into_iter()
            .map(|result| result.expect("task should join"))
            .collect();

        let successes = results.iter().filter(|result| result.is_ok()).count();
        let failures = results.iter().filter(|result| result.is_err()).count();

        assert_eq!(successes, 1, "exactly one checked consume should succeed");
        assert_eq!(failures, 7, "all remaining concurrent consumers must fail");
    }

    #[tokio::test]
    async fn test_in_memory_claim_mismatch_does_not_consume_ticket() {
        let room_id = create_test_room_id("room-claim");
        let ticket = "ticket-claim";
        let store = InMemoryTicketStore::new(30);
        let original = WsTicketData {
            user_id: "user1".to_string(),
            room_id: room_id.as_str().to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            password_version: 7,
        };
        store.store(ticket, &original, 30).await.unwrap();

        let mut mismatched = original.clone();
        mismatched.password_version += 1;

        let first_claim = store.claim(ticket, &room_id, &mismatched).await.unwrap();
        assert!(
            !first_claim,
            "claim with mismatched ticket data must fail without consuming the ticket"
        );

        let second_claim = store.claim(ticket, &room_id, &original).await.unwrap();
        assert!(
            second_claim,
            "ticket must remain claimable after a failed compare-and-delete attempt"
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
    fn test_non_cluster_mode_allows_memory() {
        let service = WsTicketService::new(None, "synctv:", None);
        assert_eq!(service.store.backend_name(), "memory");
    }

    // ============================================================================
    // Cluster mode Redis dependency tests (TDD)
    // ============================================================================

    /// Test: backend selection without Redis uses memory.
    #[test]
    fn test_new_without_redis_uses_memory_backend() {
        let service = WsTicketService::new(None, "synctv:", Some(30));
        assert_eq!(service.backend_name(), "memory");
    }

    #[test]
    fn test_new_without_redis_preserves_custom_ttl() {
        let service = WsTicketService::new(None, "synctv:", Some(60));
        assert_eq!(service.ticket_ttl_secs(), 60);
    }

    /// Test: non-cluster mode without Redis works but logs warning.
    /// Single-replica deployments should still function without Redis.
    #[test]
    fn test_non_cluster_mode_without_redis_succeeds() {
        let service = WsTicketService::new(None, "synctv:", Some(30));
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

    #[test]
    fn test_redis_ticket_store_normalizes_prefix_separator() {
        assert_eq!(
            RedisTicketStore::normalize_key_prefix("tenant-a:"),
            "tenant-a:"
        );
        assert_eq!(
            RedisTicketStore::normalize_key_prefix("tenant-a"),
            "tenant-a:"
        );
        assert_eq!(RedisTicketStore::normalize_key_prefix(""), "");
    }
}
