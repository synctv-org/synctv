//! Unified Redis Key Builder (Design Document 05-缓存设计.md)
//!
//! This module provides a unified way to construct all Redis keys used in the system.
//!
//! # Design Principles
//!
//! - All keys use a configurable prefix (default: "synctv")
//! - All IDs are nanoid(12) strings
//! - Consistent naming convention for easy debugging
//! - Support for multi-environment isolation

use crate::Config;

/// Unified Redis Key Builder
///
/// This struct provides a centralized way to generate all Redis keys,
/// ensuring consistency and supporting configuration (prefix, environment).
#[derive(Clone)]
pub struct KeyBuilder {
    prefix: String,
}

impl Default for KeyBuilder {
    fn default() -> Self {
        Self::new("synctv")
    }
}

impl KeyBuilder {
    /// Create a new `KeyBuilder` with the given prefix
    pub fn new(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
        }
    }

    /// Create `KeyBuilder` from configuration
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self::new(config.redis.key_prefix.clone())
    }

    /// Get the key prefix
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    // ==================== Cluster Management ====================

    /// Node registration information
    ///
    /// Type: String + TTL (60s)
    /// Value: JSON { `node_id`, addr, ports, status, `last_heartbeat` }
    #[must_use]
    pub fn cluster_node(&self, node_id: &str) -> String {
        format!("{}:cluster:nodes:{}", self.prefix, node_id)
    }

    /// Active nodes list (Sorted Set)
    ///
    /// Type: Sorted Set
    /// Member: `node_id`
    /// Score: timestamp (for cleanup)
    #[must_use]
    pub fn cluster_nodes_active(&self) -> String {
        format!("{}:cluster:nodes:active", self.prefix)
    }

    // ==================== Live Streaming Management ====================

    /// Stream publisher information
    ///
    /// Type: Hash + TTL (300s)
    /// Fields: `node_id`, `started_at`, status, `viewer_count`
    #[must_use]
    pub fn stream_info(&self, stream_key: &str) -> String {
        format!("{}:stream:info:{}", self.prefix, stream_key)
    }

    /// Stream pull subscribers
    ///
    /// Type: Set + TTL (300s)
    /// Members: `node_id` (nodes that are pulling this stream)
    #[must_use]
    pub fn stream_subscribers(&self, stream_key: &str) -> String {
        format!("{}:stream:subscribers:{}", self.prefix, stream_key)
    }

    /// Stream statistics
    ///
    /// Type: Hash + TTL (600s)
    /// Fields: viewers, bitrate, packets, bytes
    #[must_use]
    pub fn stream_stats(&self, stream_key: &str) -> String {
        format!("{}:stream:stats:{}", self.prefix, stream_key)
    }

    // ==================== Room State ====================

    /// Room current state
    ///
    /// Type: Hash
    /// Fields: `room_id`, `playing_media_id`, position, speed, `is_playing`, `updated_at`, version
    #[must_use]
    pub fn room_state(&self, room_id: &str) -> String {
        format!("{}:room:{}:state", self.prefix, room_id)
    }

    /// Room member list
    ///
    /// Type: Set
    /// Members: `user_id`
    #[must_use]
    pub fn room_members(&self, room_id: &str) -> String {
        format!("{}:room:{}:members", self.prefix, room_id)
    }

    /// Room online users
    ///
    /// Type: Sorted Set
    /// Members: `user_id`
    /// Score: `last_activity_timestamp`
    #[must_use]
    pub fn room_online_users(&self, room_id: &str) -> String {
        format!("{}:room:{}:online", self.prefix, room_id)
    }

    /// Room viewer count
    ///
    /// Type: String + TTL (60s)
    /// Value: number (count)
    #[must_use]
    pub fn room_viewers(&self, room_id: &str) -> String {
        format!("{}:room:{}:viewers", self.prefix, room_id)
    }

    // ==================== Playback Cache ====================

    /// Playback information cache
    ///
    /// Type: String + TTL (dynamic)
    /// Value: JSON with playback state
    #[must_use]
    pub fn playback_cache(&self, cache_key: &str) -> String {
        format!("{}:playback:{}", self.prefix, cache_key)
    }

    // ==================== Session Management ====================

    /// User session
    ///
    /// Type: String + TTL (dynamic)
    /// Value: JSON with session data
    #[must_use]
    pub fn user_session(&self, session_id: &str) -> String {
        format!("{}:session:{}", self.prefix, session_id)
    }

    // ==================== Rate Limiting ====================

    /// API rate limiting
    ///
    /// Type: String + TTL (window duration)
    /// Value: counter (INCR operation)
    ///
    /// identifier: `user_id`, IP, etc.
    /// window: "1s", "1m", "1h", etc.
    #[must_use]
    pub fn rate_limit(&self, identifier: &str, window: &str) -> String {
        format!("{}:ratelimit:{}:{}", self.prefix, identifier, window)
    }

    // ==================== OAuth2 State ====================

    /// `OAuth2` state token (for CSRF protection during authorization flow)
    ///
    /// Type: String + TTL (300s)
    /// Value: JSON with `OAuth2State`
    #[must_use]
    pub fn oauth2_state(&self, state_token: &str) -> String {
        format!("{}:oauth2:state:{}", self.prefix, state_token)
    }

    // ==================== Email Verification ====================

    /// Email verification code
    ///
    /// Type: String + TTL (configurable)
    /// Value: JSON with code + attempts
    #[must_use]
    pub fn email_code(&self, email: &str) -> String {
        format!("{}:email:code:{}", self.prefix, email)
    }

    // ==================== Brute-Force Protection ====================

    /// Failed login attempt counter per username
    ///
    /// Type: String + TTL (15 minutes)
    /// Value: counter (INCR operation)
    #[must_use]
    pub fn login_attempts(&self, username: &str) -> String {
        format!("{}:auth:login_attempts:{}", self.prefix, username)
    }

    /// Failed login attempt counter per IP address
    ///
    /// Type: String + TTL (10 minutes)
    /// Value: JSON with count and `last_failure_at`
    #[must_use]
    pub fn login_attempts_ip(&self, ip: &str) -> String {
        format!("{}:auth:login_attempts_ip:{}", self.prefix, ip)
    }

    /// Failed room password verification counter per room+IP combination
    ///
    /// Type: String + TTL (15 minutes)
    /// Value: JSON with count and `last_failure_at`
    #[must_use]
    pub fn room_password_attempts(&self, room_id: &str, ip: &str) -> String {
        format!("{}:room:pwd_attempts:{}:{}", self.prefix, room_id, ip)
    }

    // ==================== Refresh Token Blacklist ====================

    /// Blacklisted refresh token JTI (used for refresh token rotation)
    ///
    /// Type: String + TTL (remaining token lifetime)
    /// Value: "1" (presence check only)
    #[must_use]
    pub fn refresh_token_blacklist(&self, jti: &str) -> String {
        format!("{}:auth:rt_blacklist:{}", self.prefix, jti)
    }

    /// Blacklisted access token JTI (used on logout to invalidate access tokens)
    ///
    /// Type: String + TTL (remaining token lifetime)
    /// Value: "1" (presence check only)
    #[must_use]
    pub fn access_token_blacklist(&self, jti: &str) -> String {
        format!("{}:auth:at_blacklist:{}", self.prefix, jti)
    }

    /// Refresh token family revocation key (per `user_id`)
    ///
    /// Type: String + TTL (max refresh token lifetime)
    /// Value: Unix timestamp when the family was revoked
    #[must_use]
    pub fn refresh_token_family_revoked(&self, user_id: &str) -> String {
        format!("{}:auth:rt_family_revoked:{}", self.prefix, user_id)
    }

    // ==================== Guest Token Blacklist ====================

    /// Blacklisted guest token JTI (for revoking guest access)
    ///
    /// Type: String + TTL (remaining token lifetime)
    /// Value: "1" (presence check only)
    #[must_use]
    pub fn guest_token_blacklist(&self, jti: &str) -> String {
        format!("{}:auth:guest_blacklist:{}", self.prefix, jti)
    }

    /// Room guest version key (for revoking all guest tokens in a room)
    ///
    /// Type: String + TTL (max guest token lifetime)
    /// Value: Monotonically increasing version number
    #[must_use]
    pub fn room_guest_version(&self, room_id: &str) -> String {
        format!("{}:room:{}:guest_version", self.prefix, room_id)
    }

    // ==================== WebSocket Ticket ====================

    /// WebSocket ticket (one-time use)
    ///
    /// Type: String + TTL (30s)
    /// Value: JSON with ticket data
    #[must_use]
    pub fn ws_ticket(&self, ticket: &str) -> String {
        format!("{}:ws_ticket:{}", self.prefix, ticket)
    }

    // ==================== Cache Invalidation ====================

    /// Cache invalidation stream key
    ///
    /// Used for cross-node cache invalidation via Redis Streams
    #[must_use]
    pub fn cache_invalidation_stream(&self) -> String {
        format!("{}:cache:invalidate:stream", self.prefix)
    }

    // ==================== Cluster Events ====================

    /// Cluster events pub/sub channel
    ///
    /// Used for cross-cluster message broadcasting
    #[must_use]
    pub fn cluster_events_channel(&self) -> String {
        format!("{}:cluster:events", self.prefix)
    }

    /// Room-specific messages channel
    ///
    /// Used for room message broadcasting (chat, danmaku, etc.)
    #[must_use]
    pub fn room_messages_channel(&self, room_id: &str) -> String {
        format!("{}:room:{}:messages", self.prefix, room_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_builder_default() {
        let builder = KeyBuilder::default();

        assert_eq!(
            builder.cluster_node("node-1"),
            "synctv:cluster:nodes:node-1"
        );

        assert_eq!(
            builder.stream_info("room_123"),
            "synctv:stream:info:room_123"
        );

        assert_eq!(
            builder.room_state("abc123"),
            "synctv:room:abc123:state"
        );
    }

    #[test]
    fn test_key_builder_custom_prefix() {
        let builder = KeyBuilder::new("prod");

        assert_eq!(
            builder.cluster_node("node-1"),
            "prod:cluster:nodes:node-1"
        );

        assert_eq!(
            builder.stream_info("room_123"),
            "prod:stream:info:room_123"
        );
    }

    #[test]
    fn test_rate_limit_keys() {
        let builder = KeyBuilder::default();

        // User-specific rate limit
        assert_eq!(
            builder.rate_limit("user_123", "1m"),
            "synctv:ratelimit:user_123:1m"
        );

        // IP-based rate limit
        assert_eq!(
            builder.rate_limit("192.168.1.1", "1s"),
            "synctv:ratelimit:192.168.1.1:1s"
        );
    }

    #[test]
    fn test_oauth2_state_key() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.oauth2_state("abc123token"),
            "synctv:oauth2:state:abc123token"
        );
    }

    #[test]
    fn test_email_code_key() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.email_code("user@example.com"),
            "synctv:email:code:user@example.com"
        );
    }

    #[test]
    fn test_ws_ticket_key() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.ws_ticket("ticket_abc"),
            "synctv:ws_ticket:ticket_abc"
        );
    }

    #[test]
    fn test_guest_token_blacklist_key() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.guest_token_blacklist("jti_abc123"),
            "synctv:auth:guest_blacklist:jti_abc123"
        );
    }

    #[test]
    fn test_room_guest_version_key() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.room_guest_version("room_xyz789"),
            "synctv:room:room_xyz789:guest_version"
        );
    }
}
