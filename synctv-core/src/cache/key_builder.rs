//! Unified Redis Key Builder (Design Document 05-cache-design.md)
//!
//! This module provides a unified way to construct all Redis keys used in the system.
//!
//! # Design Principles
//!
//! - All keys use a configurable prefix (default: "synctv")
//! - All IDs are 12-character shared base62 strings
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
        let mut prefix = prefix.into();
        while prefix.ends_with(':') {
            prefix.pop();
        }
        Self { prefix }
    }

    /// Create `KeyBuilder` from configuration
    #[must_use]
    pub fn from_config(config: &Config) -> Self {
        Self::new(config.redis.key_prefix.clone())
    }

    /// Sanitize a user-controlled key segment to prevent Redis key hierarchy
    /// confusion.  Colons are used as the Redis key separator so they must not
    /// appear in user-supplied values (e.g. email addresses, usernames).
    #[must_use]
    fn sanitize_key_segment(segment: &str) -> String {
        segment.replace(':', "_")
    }

    fn prefixed_key(&self, suffix: &str) -> String {
        if self.prefix.is_empty() {
            suffix.to_string()
        } else {
            format!("{}:{}", self.prefix, suffix)
        }
    }

    /// Build a normalized namespace prefix for callers that append their own cache keys.
    ///
    /// `namespace_prefix("user")` returns `synctv:user:` with the default prefix.
    /// `namespace_prefix("")` returns the normalized root prefix, such as `synctv:`.
    #[must_use]
    pub fn namespace_prefix(&self, namespace: &str) -> String {
        if namespace.is_empty() {
            return if self.prefix.is_empty() {
                String::new()
            } else {
                format!("{}:", self.prefix)
            };
        }
        format!("{}:", self.prefixed_key(namespace))
    }

    /// Get the key prefix
    #[must_use]
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Node registration information
    ///
    /// Type: String + TTL (60s)
    /// Value: JSON { `node_id`, addr, ports, status, `last_heartbeat` }
    #[must_use]
    pub fn cluster_node(&self, node_id: &str) -> String {
        self.prefixed_key(&format!("cluster:nodes:{node_id}"))
    }

    /// Active nodes list (Sorted Set)
    ///
    /// Type: Sorted Set
    /// Member: `node_id`
    /// Score: timestamp (for cleanup)
    #[must_use]
    pub fn cluster_nodes_active(&self) -> String {
        self.prefixed_key("cluster:nodes:active")
    }

    /// Stream publisher information
    ///
    /// Type: Hash + TTL (300s)
    /// Fields: `node_id`, `started_at`, status, `viewer_count`
    #[must_use]
    pub fn stream_info(&self, stream_key: &str) -> String {
        self.prefixed_key(&format!("stream:info:{stream_key}"))
    }

    /// Stream pull subscribers
    ///
    /// Type: Set + TTL (300s)
    /// Members: `node_id` (nodes that are pulling this stream)
    #[must_use]
    pub fn stream_subscribers(&self, stream_key: &str) -> String {
        self.prefixed_key(&format!("stream:subscribers:{stream_key}"))
    }

    /// Stream statistics
    ///
    /// Type: Hash + TTL (600s)
    /// Fields: viewers, bitrate, packets, bytes
    #[must_use]
    pub fn stream_stats(&self, stream_key: &str) -> String {
        self.prefixed_key(&format!("stream:stats:{stream_key}"))
    }

    /// Room current state
    ///
    /// Type: Hash
    /// Fields: `room_id`, `playing_media_id`, position, speed, `is_playing`, `updated_at`, version
    #[must_use]
    pub fn room_state(&self, room_id: &str) -> String {
        self.prefixed_key(&format!("room:{room_id}:state"))
    }

    /// Room member list
    ///
    /// Type: Set
    /// Members: `user_id`
    #[must_use]
    pub fn room_members(&self, room_id: &str) -> String {
        self.prefixed_key(&format!("room:{room_id}:members"))
    }

    /// Room online users
    ///
    /// Type: Sorted Set
    /// Members: `user_id`
    /// Score: `last_activity_timestamp`
    #[must_use]
    pub fn room_online_users(&self, room_id: &str) -> String {
        self.prefixed_key(&format!("room:{room_id}:online"))
    }

    /// Room viewer count
    ///
    /// Type: String + TTL (60s)
    /// Value: number (count)
    #[must_use]
    pub fn room_viewers(&self, room_id: &str) -> String {
        self.prefixed_key(&format!("room:{room_id}:viewers"))
    }

    /// Playback information cache
    ///
    /// Type: String + TTL (dynamic)
    /// Value: JSON with playback state
    #[must_use]
    pub fn playback_cache(&self, cache_key: &str) -> String {
        self.prefixed_key(&format!("playback:{cache_key}"))
    }

    /// User session
    ///
    /// Type: String + TTL (dynamic)
    /// Value: JSON with session data
    #[must_use]
    pub fn user_session(&self, session_id: &str) -> String {
        self.prefixed_key(&format!("session:{session_id}"))
    }

    /// Short-lived auth/session workflow key.
    ///
    /// Namespaces are fixed by the owning service (for example OPAQUE, MFA, or
    /// passkey session stores). The session id is generated server-side and
    /// sanitized here so shared Redis session storage uses the same prefix rules
    /// as other auth keys.
    #[must_use]
    pub fn session(&self, namespace: &str, session_id: &str) -> String {
        self.prefixed_key(&format!(
            "{}:{}",
            namespace,
            Self::sanitize_key_segment(session_id)
        ))
    }

    /// API rate limiting
    ///
    /// Type: String + TTL (window duration)
    /// Value: counter (INCR operation)
    ///
    /// identifier: `user_id`, IP, etc.
    /// window: "1s", "1m", "1h", etc.
    #[must_use]
    pub fn rate_limit(&self, identifier: &str, window: &str) -> String {
        self.prefixed_key(&format!(
            "ratelimit:{}:{}",
            Self::sanitize_key_segment(identifier),
            window
        ))
    }

    /// `OAuth2` state token (for CSRF protection during authorization flow)
    ///
    /// Type: String + TTL (300s)
    /// Value: JSON with `OAuth2State`
    #[must_use]
    pub fn oauth2_state(&self, state_token: &str) -> String {
        self.prefixed_key(&format!("oauth2:state:{state_token}"))
    }

    /// Email token code
    ///
    /// Type: String + TTL (configurable)
    /// Value: JSON with code + attempts
    #[must_use]
    pub fn email_code(&self, email: &str) -> String {
        self.prefixed_key(&format!("email:code:{}", Self::sanitize_key_segment(email)))
    }

    /// Failed login attempt counter per username
    ///
    /// Type: String + TTL (15 minutes)
    /// Value: counter (INCR operation)
    #[must_use]
    pub fn login_attempts(&self, username: &str) -> String {
        self.prefixed_key(&format!(
            "auth:login_attempts:{}",
            Self::sanitize_key_segment(username)
        ))
    }

    /// Failed login attempt counter per IP address
    ///
    /// Type: String + TTL (10 minutes)
    /// Value: JSON with count and `last_failure_at`
    #[must_use]
    pub fn login_attempts_ip(&self, ip: &str) -> String {
        self.prefixed_key(&format!(
            "auth:login_attempts_ip:{}",
            Self::sanitize_key_segment(ip)
        ))
    }

    /// Failed room password verification counter per room+IP combination
    ///
    /// Type: String + TTL (15 minutes)
    /// Value: JSON with count and `last_failure_at`
    #[must_use]
    pub fn room_password_attempts(&self, room_id: &str, ip: &str) -> String {
        self.prefixed_key(&format!("room:pwd_attempts:{room_id}:{ip}"))
    }

    /// Blacklisted refresh token JTI (used for refresh token rotation)
    ///
    /// Type: String + TTL (remaining token lifetime)
    /// Value: "1" (presence check only)
    #[must_use]
    pub fn refresh_token_blacklist(&self, jti: &str) -> String {
        self.prefixed_key(&format!("auth:rt_blacklist:{jti}"))
    }

    /// Blacklisted access token JTI (used on logout to invalidate access tokens)
    ///
    /// Type: String + TTL (remaining token lifetime)
    /// Value: "1" (presence check only)
    #[must_use]
    pub fn access_token_blacklist(&self, jti: &str) -> String {
        self.prefixed_key(&format!("auth:at_blacklist:{jti}"))
    }

    /// Refresh token session revocation key (per user login session).
    ///
    /// Type: String + TTL (max refresh token lifetime)
    /// Value: Unix timestamp when the session was revoked
    #[must_use]
    pub fn refresh_token_session_revoked(&self, user_id: &str, session_id: &str) -> String {
        self.prefixed_key(&format!(
            "auth:rt_session_revoked:{}:{}",
            Self::sanitize_key_segment(user_id),
            Self::sanitize_key_segment(session_id)
        ))
    }

    /// Blacklisted guest token JTI (for revoking guest access)
    ///
    /// Type: String + TTL (remaining token lifetime)
    /// Value: "1" (presence check only)
    #[must_use]
    pub fn guest_token_blacklist(&self, jti: &str) -> String {
        self.prefixed_key(&format!("auth:guest_blacklist:{jti}"))
    }

    /// Room guest version key (for revoking all guest tokens in a room)
    ///
    /// Type: String + TTL (max guest token lifetime)
    /// Value: Monotonically increasing version number
    #[must_use]
    pub fn room_guest_version(&self, room_id: &str) -> String {
        self.prefixed_key(&format!("room:{room_id}:guest_version"))
    }

    /// WebSocket ticket (one-time use)
    ///
    /// Type: String + TTL (30s)
    /// Value: JSON with ticket data
    #[must_use]
    pub fn ws_ticket(&self, ticket: &str) -> String {
        self.prefixed_key(&format!("ws_ticket:{ticket}"))
    }

    /// Claimed RTMP publish-key JTI (for single-use enforcement)
    ///
    /// Type: String + TTL (publish key lifetime)
    /// Value: "1" (presence check only)
    #[must_use]
    pub fn publish_key_jti(&self, jti: &str) -> String {
        self.prefixed_key(&format!("publish_key:jti:{jti}"))
    }

    /// Cache invalidation stream key
    ///
    /// Used for cross-node cache invalidation via Redis Streams
    #[must_use]
    pub fn cache_invalidation_stream(&self) -> String {
        self.prefixed_key("cache:invalidate:stream")
    }

    /// Cluster events pub/sub channel
    ///
    /// Used for cross-cluster message broadcasting
    #[must_use]
    pub fn realtime_events_channel(&self) -> String {
        self.prefixed_key("cluster:events")
    }

    /// Room-specific messages channel
    ///
    /// Used for room message broadcasting.
    #[must_use]
    pub fn room_messages_channel(&self, room_id: &str) -> String {
        self.prefixed_key(&format!("room:{room_id}:messages"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_key_builder_custom_prefix() {
        let builder = KeyBuilder::new("prod");

        assert_eq!(builder.cluster_node("node-1"), "prod:cluster:nodes:node-1");

        assert_eq!(builder.stream_info("room_123"), "prod:stream:info:room_123");
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
    fn test_email_code_key_sanitizes_colons() {
        let builder = KeyBuilder::default();
        // Colons in email-like strings must be replaced to avoid breaking Redis key hierarchy
        assert_eq!(
            builder.email_code("user:tag@example.com"),
            "synctv:email:code:user_tag@example.com"
        );
    }

    #[test]
    fn test_login_attempts_sanitizes_colons() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.login_attempts("user:name"),
            "synctv:auth:login_attempts:user_name"
        );
    }

    #[test]
    fn test_rate_limit_sanitizes_colons() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.rate_limit("user:id:123", "1m"),
            "synctv:ratelimit:user_id_123:1m"
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
    fn test_key_builder_trims_trailing_colon_prefix() {
        let builder = KeyBuilder::new("synctv:");
        assert_eq!(
            builder.refresh_token_blacklist("jti_abc"),
            "synctv:auth:rt_blacklist:jti_abc"
        );
        assert_eq!(
            builder.ws_ticket("ticket_abc"),
            "synctv:ws_ticket:ticket_abc"
        );
    }

    #[test]
    fn test_key_builder_empty_prefix_has_no_leading_separator() {
        let builder = KeyBuilder::new("");
        assert_eq!(builder.cluster_node("node-1"), "cluster:nodes:node-1");
        assert_eq!(builder.room_state("room-1"), "room:room-1:state");
        assert_eq!(builder.rate_limit("user:id", "1m"), "ratelimit:user_id:1m");
        assert_eq!(
            builder.login_attempts("user:name"),
            "auth:login_attempts:user_name"
        );
        assert_eq!(builder.oauth2_state("state_abc"), "oauth2:state:state_abc");
        assert_eq!(builder.ws_ticket("ticket_abc"), "ws_ticket:ticket_abc");
        assert_eq!(builder.session("auth:test", "sess:1"), "auth:test:sess_1");
        assert_eq!(
            builder.publish_key_jti("jti_abc"),
            "publish_key:jti:jti_abc"
        );
    }

    #[test]
    fn test_session_key_uses_prefix_and_sanitizes_session_id() {
        let builder = KeyBuilder::new("synctv:");
        assert_eq!(
            builder.session("auth:test", "sess:1"),
            "synctv:auth:test:sess_1"
        );
    }

    #[test]
    fn test_key_builder_from_config_trims_trailing_colon_prefix() {
        let mut config = Config::default();
        config.redis.key_prefix = "tenant-a:".to_string();
        let builder = KeyBuilder::from_config(&config);

        assert_eq!(builder.prefix(), "tenant-a");
    }

    #[test]
    fn test_namespace_prefix_normalizes_separators() {
        let builder = KeyBuilder::new("tenant-a:");
        assert_eq!(builder.namespace_prefix("username"), "tenant-a:username:");
        assert_eq!(builder.namespace_prefix(""), "tenant-a:");

        let empty = KeyBuilder::new("");
        assert_eq!(empty.namespace_prefix("username"), "username:");
        assert_eq!(empty.namespace_prefix(""), "");
    }

    #[test]
    fn test_refresh_token_session_revoked_key_sanitizes_segments() {
        let builder = KeyBuilder::new("tenant-a");
        assert_eq!(
            builder.refresh_token_session_revoked("user:1", "session:1"),
            "tenant-a:auth:rt_session_revoked:user_1:session_1"
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
    fn test_publish_key_jti_key() {
        let builder = KeyBuilder::default();
        assert_eq!(
            builder.publish_key_jti("jti_abc"),
            "synctv:publish_key:jti:jti_abc"
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
