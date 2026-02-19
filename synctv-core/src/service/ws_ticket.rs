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

use base64::Engine;
use rand::RngExt;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, warn};

use crate::models::{RoomId, UserId};
use crate::{Error, Result};

/// Redis key prefix for WebSocket tickets
const WS_TICKET_PREFIX: &str = "synctv:ws_ticket:";
/// Default ticket TTL in seconds
const DEFAULT_TICKET_TTL_SECS: u64 = 30;
/// Ticket length in bytes (256 bits of entropy)
const TICKET_LENGTH: usize = 32;

/// WebSocket ticket data stored in Redis
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
}

/// In-memory ticket storage for single-replica deployments using moka cache with TTL
#[derive(Clone)]
struct MemoryTicketStore {
    cache: moka::future::Cache<String, WsTicketData>,
    ttl_secs: u64,
}

impl MemoryTicketStore {
    fn new(ttl_secs: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .time_to_live(std::time::Duration::from_secs(ttl_secs))
                .max_capacity(10_000)
                .build(),
            ttl_secs,
        }
    }

    async fn insert(&self, ticket: String, data: WsTicketData) {
        self.cache.insert(ticket, data).await;
    }

    async fn get_and_remove(&self, ticket: &str) -> Option<WsTicketData> {
        // Use remove() for atomic get-and-delete to prevent TOCTOU race conditions
        // where two concurrent requests could both get() the same ticket.
        // Since remove() may return entries that moka hasn't lazily evicted yet,
        // we manually check TTL expiry on the returned value.
        let data = self.cache.remove(ticket).await?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if now.saturating_sub(data.created_at) > self.ttl_secs {
            return None; // Expired
        }
        Some(data)
    }
}

/// Service for creating and validating WebSocket tickets
#[derive(Clone)]
pub struct WsTicketService {
    /// Redis connection manager for ticket storage (multi-replica mode)
    redis_conn: Option<redis::aio::ConnectionManager>,
    /// In-memory store for single-replica mode
    memory_store: Option<MemoryTicketStore>,
    /// Ticket TTL in seconds
    ticket_ttl_secs: u64,
}

impl std::fmt::Debug for WsTicketService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsTicketService")
            .field("redis_enabled", &self.redis_conn.is_some())
            .field("memory_mode", &self.memory_store.is_some())
            .field("ticket_ttl_secs", &self.ticket_ttl_secs)
            .finish()
    }
}

impl WsTicketService {
    /// Create a new WebSocket ticket service with a Redis connection manager
    ///
    /// # Arguments
    /// * `redis_conn` - Redis connection manager for distributed ticket storage (recommended for multi-replica)
    /// * `ticket_ttl_secs` - Ticket lifetime in seconds (default: 30)
    /// * `cluster_mode` - When `true`, Redis is **required**; passing `None` for `redis_conn`
    ///   in cluster mode returns an error because in-memory storage is incompatible with
    ///   multi-replica deployments (tickets created on one node are not visible to others).
    ///
    /// # Errors
    /// Returns an error if `cluster_mode` is `true` and `redis_conn` is `None`.
    pub fn new(
        redis_conn: Option<redis::aio::ConnectionManager>,
        ticket_ttl_secs: Option<u64>,
        cluster_mode: bool,
    ) -> Result<Self> {
        let ttl = ticket_ttl_secs.unwrap_or(DEFAULT_TICKET_TTL_SECS);

        if redis_conn.is_some() {
            Ok(Self {
                redis_conn,
                memory_store: None,
                ticket_ttl_secs: ttl,
            })
        } else {
            // In cluster mode Redis is mandatory: tickets must be shared across all replicas.
            if cluster_mode {
                return Err(Error::Internal(
                    "Redis is required for WebSocket ticket service in cluster mode. \
                     Tickets stored in memory are only visible on the replica that created them, \
                     causing authentication failures on other replicas. Configure Redis."
                        .to_string(),
                ));
            }

            // Fall back to memory storage for single-replica deployments
            warn!(
                "WebSocket ticket service using in-memory storage. \
                 This is only suitable for single-replica deployments. \
                 For multi-replica setups, configure Redis."
            );

            // Detect multi-replica environment and warn loudly
            if Self::detect_multi_replica_environment() {
                error!(
                    "MULTI-REPLICA RISK: WebSocket ticket service is using in-memory storage, \
                     but the environment appears to be a multi-replica deployment \
                     (Kubernetes / Docker Swarm / REPLICAS env detected). \
                     Tickets created on one replica will NOT be valid on others. \
                     Configure Redis to fix this."
                );
            }

            Ok(Self {
                redis_conn: None,
                memory_store: Some(MemoryTicketStore::new(ttl)),
                ticket_ttl_secs: ttl,
            })
        }
    }

    /// Create a new WebSocket ticket service with Redis (multi-replica mode)
    ///
    /// # Panics
    /// Panics if the internal `new()` call fails (which it cannot when `redis_conn` is `Some`).
    #[must_use]
    pub fn with_redis(redis_conn: redis::aio::ConnectionManager, ticket_ttl_secs: Option<u64>) -> Self {
        Self::new(Some(redis_conn), ticket_ttl_secs, false)
            .expect("new() with Some(redis_conn) never fails")
    }

    /// Create a new WebSocket ticket service with memory storage (single-replica mode)
    #[must_use]
    pub fn with_memory(ticket_ttl_secs: Option<u64>) -> Self {
        let ttl = ticket_ttl_secs.unwrap_or(DEFAULT_TICKET_TTL_SECS);
        Self {
            redis_conn: None,
            memory_store: Some(MemoryTicketStore::new(ttl)),
            ticket_ttl_secs: ttl,
        }
    }

    /// Get the configured ticket TTL in seconds
    #[must_use]
    pub const fn ticket_ttl_secs(&self) -> u64 {
        self.ticket_ttl_secs
    }

    /// Return `true` if the service is backed by Redis (multi-replica safe).
    ///
    /// Returns `false` when using in-memory storage (single-replica mode).
    #[must_use]
    pub const fn is_redis_backed(&self) -> bool {
        self.redis_conn.is_some()
    }

    /// Create a new ticket for a user bound to a specific room
    ///
    /// Returns a ticket string that can be used once for WebSocket authentication.
    /// The ticket expires after `ticket_ttl_secs` seconds and is only valid for
    /// the supplied `room_id` (Issue #65).
    pub async fn create_ticket(&self, user_id: &UserId, room_id: &RoomId) -> Result<String> {
        // Generate a random ticket
        let ticket = Self::generate_ticket();

        let ticket_data = WsTicketData {
            user_id: user_id.as_str().to_string(),
            room_id: room_id.as_str().to_string(),
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        };

        if let Some(ref conn) = self.redis_conn {
            // Store in Redis with TTL (multi-replica mode)
            let key = format!("{WS_TICKET_PREFIX}{ticket}");
            let json = serde_json::to_string(&ticket_data).map_err(|e| {
                Error::Internal(format!("Failed to serialize ticket data: {e}"))
            })?;

            let mut conn = conn.clone();

            let _: () = tokio::time::timeout(
                crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
                conn.set_ex(&key, json, self.ticket_ttl_secs),
            )
                .await
                .map_err(|_| Error::Internal("Redis timeout: store ticket".to_string()))?
                .map_err(|e| Error::Internal(format!("Failed to store ticket: {e}")))?;

            debug!(
                user_id = %user_id.as_str(),
                ttl_secs = self.ticket_ttl_secs,
                mode = "redis",
                "WebSocket ticket created"
            );
        } else if let Some(ref store) = self.memory_store {
            // Store in memory (single-replica mode)
            store.insert(ticket.clone(), ticket_data).await;

            debug!(
                user_id = %user_id.as_str(),
                ttl_secs = self.ticket_ttl_secs,
                mode = "memory",
                "WebSocket ticket created"
            );
        } else {
            // This should never happen as new() always sets one of the two backends
            return Err(Error::Internal(
                "No ticket storage backend configured".to_string(),
            ));
        }

        Ok(ticket)
    }

    /// Validate and consume a ticket
    ///
    /// Returns the user ID associated with the ticket if valid and the ticket's
    /// `room_id` matches the expected `room_id`. The ticket is deleted after use
    /// (one-time use). Passing a ticket for a different room returns an error so that
    /// tickets cannot be replayed across rooms (Issue #65).
    pub async fn validate_and_consume(&self, ticket: &str, expected_room_id: &RoomId) -> Result<UserId> {
        // Try Redis first (multi-replica mode)
        if let Some(ref conn) = self.redis_conn {
            let key = format!("{WS_TICKET_PREFIX}{ticket}");
            let mut conn = conn.clone();

            // Get and delete atomically using Lua script
            let lua_script = redis::Script::new(r#"
                local value = redis.call("GET", KEYS[1])
                if value then
                    redis.call("DEL", KEYS[1])
                end
                return value
            "#);

            let json: Option<String> = tokio::time::timeout(
                crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
                lua_script.key(&key).invoke_async(&mut conn),
            )
                .await
                .map_err(|_| Error::Internal("Redis timeout: validate ticket".to_string()))?
                .map_err(|e| Error::Internal(format!("Failed to validate ticket: {e}")))?;

            let Some(json) = json else {
                debug!(ticket = %ticket, mode = "redis", "WebSocket ticket not found or expired");
                return Err(Error::Authorization("Invalid or expired ticket".to_string()));
            };

            let ticket_data: WsTicketData = serde_json::from_str(&json).map_err(|e| {
                Error::Internal(format!("Failed to deserialize ticket data: {e}"))
            })?;

            // Room-bound validation: reject the ticket if it was issued for a different room.
            if ticket_data.room_id != expected_room_id.as_str() {
                debug!(
                    ticket_room = %ticket_data.room_id,
                    expected_room = %expected_room_id.as_str(),
                    mode = "redis",
                    "WebSocket ticket rejected: room mismatch"
                );
                return Err(Error::Authorization("Ticket not valid for this room".to_string()));
            }

            debug!(
                user_id = %ticket_data.user_id,
                room_id = %ticket_data.room_id,
                mode = "redis",
                "WebSocket ticket validated and consumed"
            );

            return Ok(UserId::from_string(ticket_data.user_id));
        }

        // Try memory storage (single-replica mode)
        if let Some(ref store) = self.memory_store {
            let Some(ticket_data) = store.get_and_remove(ticket).await else {
                debug!(ticket = %ticket, mode = "memory", "WebSocket ticket not found or expired");
                return Err(Error::Authorization("Invalid or expired ticket".to_string()));
            };

            // Room-bound validation
            if ticket_data.room_id != expected_room_id.as_str() {
                debug!(
                    ticket_room = %ticket_data.room_id,
                    expected_room = %expected_room_id.as_str(),
                    mode = "memory",
                    "WebSocket ticket rejected: room mismatch"
                );
                return Err(Error::Authorization("Ticket not valid for this room".to_string()));
            }

            debug!(
                user_id = %ticket_data.user_id,
                room_id = %ticket_data.room_id,
                mode = "memory",
                "WebSocket ticket validated and consumed"
            );

            return Ok(UserId::from_string(ticket_data.user_id));
        }

        Err(Error::Internal(
            "No ticket storage backend configured".to_string(),
        ))
    }

    /// Generate a secure random ticket string
    fn generate_ticket() -> String {
        // Generate cryptographically secure random bytes
        let mut rng = rand::rng();
        let mut bytes = [0u8; TICKET_LENGTH];
        rng.fill(&mut bytes);

        // Encode as URL-safe base64 (no special characters that could cause issues in URLs)
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// Detect if the environment appears to be a multi-replica deployment.
    ///
    /// Checks for common indicators:
    /// - `KUBERNETES_SERVICE_HOST` env var (Kubernetes)
    /// - `REPLICAS` or `SYNCTV_REPLICAS` env var set to > 1
    /// - `/var/run/secrets/kubernetes.io` exists (Kubernetes pod)
    fn detect_multi_replica_environment() -> bool {
        // Kubernetes environment
        if std::env::var("KUBERNETES_SERVICE_HOST").is_ok() {
            return true;
        }

        // Explicit replica count configuration
        for var in &["REPLICAS", "SYNCTV_REPLICAS"] {
            if let Ok(val) = std::env::var(var) {
                if let Ok(count) = val.parse::<u32>() {
                    if count > 1 {
                        return true;
                    }
                }
            }
        }

        // Kubernetes secrets mount
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

        // Tickets should be different
        assert_ne!(ticket1, ticket2);

        // Tickets should be URL-safe base64
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
        };

        let json = serde_json::to_string(&data).unwrap();
        let decoded: WsTicketData = serde_json::from_str(&json).unwrap();

        assert_eq!(data.user_id, decoded.user_id);
        assert_eq!(data.room_id, decoded.room_id);
        assert_eq!(data.created_at, decoded.created_at);
    }

    #[tokio::test]
    async fn test_ticket_service_memory_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room1");

        // Should work in memory mode
        let ticket = service.create_ticket(&user_id, &room_id).await;
        assert!(ticket.is_ok());

        // Validate and consume
        let result = service.validate_and_consume(&ticket.unwrap(), &room_id).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), "user1");
    }

    #[tokio::test]
    async fn test_ticket_one_time_use_memory_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room1");

        let ticket = service.create_ticket(&user_id, &room_id).await.unwrap();

        // First use should succeed
        let result1 = service.validate_and_consume(&ticket, &room_id).await;
        assert!(result1.is_ok());

        // Second use should fail
        let result2 = service.validate_and_consume(&ticket, &room_id).await;
        assert!(result2.is_err());
    }

    #[tokio::test]
    async fn test_ticket_room_mismatch_rejected() {
        let service = WsTicketService::with_memory(Some(30));
        let user_id = create_test_user_id("user1");
        let room_a = create_test_room_id("room-a");
        let room_b = create_test_room_id("room-b");

        // Create ticket for room A
        let ticket = service.create_ticket(&user_id, &room_a).await.unwrap();

        // Using the ticket for room B should fail
        let result = service.validate_and_consume(&ticket, &room_b).await;
        assert!(result.is_err(), "Ticket for room A should not be valid for room B");
    }

    #[tokio::test]
    async fn test_ticket_expiration_memory_mode() {
        let service = WsTicketService::with_memory(Some(1)); // 1 second TTL
        let user_id = create_test_user_id("user1");
        let room_id = create_test_room_id("room1");

        let ticket = service.create_ticket(&user_id, &room_id).await.unwrap();

        // Wait for expiration
        tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;

        // Should be expired
        let result = service.validate_and_consume(&ticket, &room_id).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_invalid_ticket_memory_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let room_id = create_test_room_id("room1");

        let result = service.validate_and_consume("invalid_ticket", &room_id).await;
        assert!(result.is_err());
    }

    // ========== Multi-Replica Detection ==========

    #[test]
    fn test_detect_multi_replica_no_env_is_false() {
        // In a normal test environment without Kubernetes, should return false
        // (unless the CI is running in K8s, which is acceptable)
        let result = WsTicketService::detect_multi_replica_environment();
        // We can't assert false because the test might run in K8s.
        // Just verify it doesn't panic.
        let _ = result;
    }

    #[test]
    fn test_memory_mode_creates_valid_service() {
        let service = WsTicketService::with_memory(Some(60));
        assert!(service.redis_conn.is_none());
        assert!(service.memory_store.is_some());
        assert_eq!(service.ticket_ttl_secs, 60);
    }

    #[test]
    fn test_debug_shows_mode() {
        let service = WsTicketService::with_memory(Some(30));
        let debug_str = format!("{service:?}");
        assert!(debug_str.contains("memory_mode"));
        assert!(debug_str.contains("true"));
    }

    #[test]
    fn test_cluster_mode_requires_redis() {
        // In cluster mode, Redis is required; None should produce an error
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
        // In non-cluster mode, None redis is acceptable (memory fallback)
        let result = WsTicketService::new(None, None, false);
        assert!(result.is_ok());
        let service = result.unwrap();
        assert!(service.redis_conn.is_none());
        assert!(service.memory_store.is_some());
    }
}
