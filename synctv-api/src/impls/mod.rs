//! Unified API Implementation Layer
//!
//! This module contains the actual implementation of all APIs.
//! Both HTTP and gRPC handlers are thin wrappers that call these implementations.
//!
//! All methods use grpc-generated types for parameters and return values.

pub mod admin;
pub mod client;
pub mod email;
pub mod messaging;
pub mod notification;
pub mod oauth2;
pub mod provider;
pub mod providers;

// Re-export for convenience
pub use admin::AdminApiImpl;
pub use client::{ClientApiImpl, ClientApiConfig};
pub use email::EmailApiImpl;
pub use messaging::{MessageConcurrencyConfig, StreamMessageHandler, MessageSender, ProtoCodec};
pub use notification::NotificationApiImpl;
pub use oauth2::OAuth2ApiImpl;
pub use providers::{AlistApiImpl, BilibiliApiImpl, EmbyApiImpl};

/// Try to publish a cluster event via the Redis publish channel.
///
/// On success, the event is queued for publication. On failure (channel full
/// or closed), a warning is logged and the `CLUSTER_EVENTS_DROPPED` metric
/// is incremented so operators can detect state drift.
pub fn try_publish_cluster_event(
    tx: &tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>,
    request: synctv_cluster::sync::PublishRequest,
) {
    if let Err(err) = tx.try_send(request) {
        match err {
            tokio::sync::mpsc::error::TrySendError::Full(_) => {
                synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                    .with_label_values(&["channel_full"])
                    .inc();
                tracing::warn!("Cluster event publish channel full, event dropped");
            }
            tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                synctv_core::metrics::cluster::CLUSTER_EVENTS_DROPPED
                    .with_label_values(&["channel_closed"])
                    .inc();
                tracing::warn!("Cluster event publish channel closed, event dropped");
            }
        }
    }
}

/// Kick a stream both locally and cluster-wide via Redis Pub/Sub.
///
/// Shared utility used by both `ClientApiImpl` and `AdminApiImpl` after media
/// deletion to terminate any active RTMP stream.
pub fn kick_stream_cluster(
    live_streaming_infrastructure: Option<&std::sync::Arc<synctv_livestream::api::LiveStreamingInfrastructure>>,
    redis_publish_tx: Option<&tokio::sync::mpsc::Sender<synctv_cluster::sync::PublishRequest>>,
    room_id: &str,
    media_id: &str,
    reason: &str,
) {
    use synctv_cluster::sync::{ClusterEvent, PublishRequest};
    use synctv_core::models::{RoomId as Rid, MediaId as Mid};

    // 1. Local kick (no-op if stream not on this node)
    if let Some(infra) = live_streaming_infrastructure {
        if let Err(e) = infra.kick_publisher(room_id, media_id) {
            tracing::warn!(room_id, media_id, error = %e, "Failed to kick local publisher");
        }
    }

    // 2. Cluster-wide via Redis
    if let Some(tx) = redis_publish_tx {
        if tx.try_send(PublishRequest {
            event: ClusterEvent::KickPublisher {
                event_id: nanoid::nanoid!(16),
                room_id: Rid::from_string(room_id.to_string()),
                media_id: Mid::from_string(media_id.to_string()),
                reason: reason.to_string(),
                timestamp: chrono::Utc::now(),
            },
        }).is_err() {
            tracing::warn!(room_id, media_id, "Failed to send cluster-wide kick event (Redis channel closed or full)");
        }
    }
}

/// Application-level error codes for client-side programmatic handling.
///
/// These codes are included in the `ErrorMessage.code` field and allow
/// clients to handle specific error conditions without parsing text.
///
/// Convention: 1xxx for auth, 2xxx for resources, 3xxx for validation,
/// 4xxx for permissions, 9xxx for internal errors.
pub mod error_codes {
    /// Unspecified error (fallback)
    pub const UNSPECIFIED: i32 = 0;

    // Authentication errors (1xxx)
    pub const UNAUTHENTICATED: i32 = 1000;
    pub const TOKEN_EXPIRED: i32 = 1001;
    pub const INVALID_CREDENTIALS: i32 = 1002;

    // Resource errors (2xxx)
    pub const NOT_FOUND: i32 = 2000;
    pub const ALREADY_EXISTS: i32 = 2001;
    /// System resource exhausted (backpressure/overload protection)
    pub const RESOURCE_EXHAUSTED: i32 = 2002;

    // Validation errors (3xxx)
    pub const INVALID_ARGUMENT: i32 = 3000;
    pub const INVALID_FORMAT: i32 = 3001;
    pub const VALUE_TOO_SHORT: i32 = 3002;
    pub const VALUE_TOO_LONG: i32 = 3003;
    pub const REQUIRED_FIELD_MISSING: i32 = 3004;

    // Permission errors (4xxx)
    pub const PERMISSION_DENIED: i32 = 4000;
    pub const FORBIDDEN: i32 = 4001;
    pub const BANNED: i32 = 4002;

    // Push notification codes (5xxx) - DEPRECATED
    // User notifications now use the dedicated Notification variant in ServerMessage.
    // This code is kept for backward compatibility with old clients that may still
    // expect notifications via the ErrorMessage abuse.
    //
    // NEW IMPLEMENTATION: ServerMessage.Notification is the proper way to deliver
    // user notifications. It carries structured data:
    //   - notification_id: UUID for deduplication
    //   - notification_type: Type of notification (e.g., "room_invitation", "system")
    //   - title: Display title
    //   - content: Display content
    //   - data: JSON-encoded additional data
    //   - timestamp: Unix timestamp in milliseconds
    #[deprecated(since = "0.1.0", note = "Use ServerMessage::Notification variant instead")]
    pub const NOTIFICATION_PUSH: i32 = 5000;

    // Internal errors (9xxx)
    pub const INTERNAL_ERROR: i32 = 9000;
    pub const DATABASE_ERROR: i32 = 9001;
    pub const SERVICE_UNAVAILABLE: i32 = 9002;
}

/// Shared error classification for impls-layer `String` errors.
///
/// Maps keyword patterns in error strings to semantic error categories.
/// Used by both HTTP and gRPC error mapping functions to ensure consistent
/// behavior across transports.
pub enum ErrorKind {
    NotFound,
    Unauthenticated,
    PermissionDenied,
    AlreadyExists,
    InvalidArgument,
    Internal,
}

impl ErrorKind {
    /// Convert this error kind to an application-level error code.
    #[must_use] 
    pub const fn to_code(&self) -> i32 {
        match self {
            Self::NotFound => error_codes::NOT_FOUND,
            Self::Unauthenticated => error_codes::UNAUTHENTICATED,
            Self::PermissionDenied => error_codes::PERMISSION_DENIED,
            Self::AlreadyExists => error_codes::ALREADY_EXISTS,
            Self::InvalidArgument => error_codes::INVALID_ARGUMENT,
            Self::Internal => error_codes::INTERNAL_ERROR,
        }
    }
}

/// Structured API error that wraps `synctv_core::Error` variants for
/// type-safe status code mapping. This allows callers that propagate
/// typed errors to bypass keyword matching entirely.
///
/// Use `ApiError::from(core_error)` to convert, then call
/// `.classify()` for the `ErrorKind`.
#[derive(Debug)]
pub enum ApiError {
    NotFound(String),
    Authentication(String),
    Authorization(String),
    AlreadyExists(String),
    InvalidInput(String),
    Internal(String),
}

impl From<synctv_core::Error> for ApiError {
    fn from(err: synctv_core::Error) -> Self {
        match err {
            synctv_core::Error::NotFound(msg) => Self::NotFound(msg),
            synctv_core::Error::Authentication(msg) => Self::Authentication(msg),
            synctv_core::Error::Authorization(msg) => Self::Authorization(msg),
            synctv_core::Error::AlreadyExists(msg) => Self::AlreadyExists(msg),
            synctv_core::Error::InvalidInput(msg) => Self::InvalidInput(msg),
            other => Self::Internal(other.to_string()),
        }
    }
}

impl ApiError {
    /// Convert this structured error into an `ErrorKind`.
    #[must_use] 
    pub const fn classify(&self) -> ErrorKind {
        match self {
            Self::NotFound(_) => ErrorKind::NotFound,
            Self::Authentication(_) => ErrorKind::Unauthenticated,
            Self::Authorization(_) => ErrorKind::PermissionDenied,
            Self::AlreadyExists(_) => ErrorKind::AlreadyExists,
            Self::InvalidInput(_) => ErrorKind::InvalidArgument,
            Self::Internal(_) => ErrorKind::Internal,
        }
    }

    /// Get the error message.
    #[must_use] 
    pub fn message(&self) -> &str {
        match self {
            Self::NotFound(msg)
            | Self::Authentication(msg)
            | Self::Authorization(msg)
            | Self::AlreadyExists(msg)
            | Self::InvalidInput(msg)
            | Self::Internal(msg) => msg,
        }
    }

    /// Get the application-level error code for this error.
    #[must_use] 
    pub const fn code(&self) -> i32 {
        self.classify().to_code()
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Use the same prefixes that classify_by_prefix recognizes, so that
        // if an ApiError is converted to String it still classifies correctly.
        match self {
            Self::NotFound(msg) => write!(f, "Not found: {msg}"),
            Self::Authentication(msg) => write!(f, "Authentication error: {msg}"),
            Self::Authorization(msg) => write!(f, "Authorization error: {msg}"),
            Self::AlreadyExists(msg) => write!(f, "Already exists: {msg}"),
            Self::InvalidInput(msg) => write!(f, "Invalid input: {msg}"),
            Self::Internal(msg) => write!(f, "Internal error: {msg}"),
        }
    }
}

impl From<ApiError> for String {
    fn from(err: ApiError) -> Self {
        err.to_string()
    }
}

impl From<String> for ApiError {
    fn from(msg: String) -> Self {
        // Try to classify the string using the existing prefix/keyword matching
        // so that pre-existing string errors map to the right variant.
        match classify_error(&msg) {
            ErrorKind::NotFound => Self::NotFound(msg),
            ErrorKind::Unauthenticated => Self::Authentication(msg),
            ErrorKind::PermissionDenied => Self::Authorization(msg),
            ErrorKind::AlreadyExists => Self::AlreadyExists(msg),
            ErrorKind::InvalidArgument => Self::InvalidInput(msg),
            ErrorKind::Internal => Self::Internal(msg),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(err: serde_json::Error) -> Self {
        Self::InvalidInput(err.to_string())
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(err: sqlx::Error) -> Self {
        Self::Internal(err.to_string())
    }
}

impl ApiError {
    /// Convert this error into a proto `ErrorMessage` with proper code and detail.
    ///
    /// The detail field is left empty to avoid leaking sensitive information.
    #[must_use] 
    pub fn to_proto_error(&self) -> crate::proto::client::ErrorMessage {
        // Sanitize Internal errors to avoid leaking sensitive implementation details
        // (e.g. database connection strings, stack traces) to clients.
        let message = match self {
            Self::Internal(_) => "Internal error".to_string(),
            _ => self.message().to_string(),
        };
        crate::proto::client::ErrorMessage {
            message,
            code: self.code(),
            detail: String::new(),
        }
    }
}

/// Classify an impls-layer error string into a semantic error kind.
///
/// First attempts to match known `synctv_core::Error` display prefixes
/// for structured classification. Falls back to keyword matching for
/// errors that don't originate from the core layer.
#[must_use] 
pub fn classify_error(err: &str) -> ErrorKind {
    // Try structured prefix matching first (matches synctv_core::Error::Display output)
    if let Some(kind) = classify_by_prefix(err) {
        return kind;
    }

    // Fallback: keyword-based classification for untyped error strings
    let lower = err.to_lowercase();
    if lower.contains("not found") {
        ErrorKind::NotFound
    } else if lower.contains("unauthenticated") || lower.contains("invalid token")
        || lower.contains("token expired") || lower.contains("not authenticated")
    {
        ErrorKind::Unauthenticated
    } else if lower.contains("permission") || lower.contains("forbidden")
        || lower.contains("not allowed") || lower.contains("banned")
    {
        ErrorKind::PermissionDenied
    } else if lower.contains("already exists") || lower.contains("already taken")
        || lower.contains("already registered")
    {
        ErrorKind::AlreadyExists
    } else if lower.contains("invalid") || lower.contains("too short") || lower.contains("too long")
        || lower.contains("cannot be empty") || lower.contains("too many")
        || lower.contains("required") || lower.contains("must be")
    {
        ErrorKind::InvalidArgument
    } else {
        ErrorKind::Internal
    }
}

/// Try to classify an error string by matching the display prefixes
/// produced by `synctv_core::Error` variants (e.g., "Not found: ...",
/// "Authentication error: ..."). Returns `None` if no prefix matches.
fn classify_by_prefix(err: &str) -> Option<ErrorKind> {
    if err.starts_with("Not found: ") {
        Some(ErrorKind::NotFound)
    } else if err.starts_with("Authentication error: ") {
        Some(ErrorKind::Unauthenticated)
    } else if err.starts_with("Authorization error: ") {
        Some(ErrorKind::PermissionDenied)
    } else if err.starts_with("Already exists: ") {
        Some(ErrorKind::AlreadyExists)
    } else if err.starts_with("Invalid input: ") {
        Some(ErrorKind::InvalidArgument)
    } else if err.starts_with("Internal error: ") || err.starts_with("Database error: ")
        || err.starts_with("Redis error: ") || err.starts_with("Serialization error: ")
    {
        Some(ErrorKind::Internal)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_error_not_found() {
        assert!(matches!(classify_error("User not found"), ErrorKind::NotFound));
        assert!(matches!(classify_error("Room Not Found"), ErrorKind::NotFound));
        assert!(matches!(classify_error("resource NOT FOUND"), ErrorKind::NotFound));
    }

    #[test]
    fn test_classify_error_unauthenticated() {
        assert!(matches!(classify_error("Unauthenticated"), ErrorKind::Unauthenticated));
        assert!(matches!(classify_error("invalid token"), ErrorKind::Unauthenticated));
        assert!(matches!(classify_error("Token expired"), ErrorKind::Unauthenticated));
        assert!(matches!(classify_error("Not authenticated"), ErrorKind::Unauthenticated));
    }

    #[test]
    fn test_classify_error_permission_denied() {
        assert!(matches!(classify_error("Permission denied"), ErrorKind::PermissionDenied));
        assert!(matches!(classify_error("Forbidden access"), ErrorKind::PermissionDenied));
        assert!(matches!(classify_error("Operation not allowed"), ErrorKind::PermissionDenied));
        assert!(matches!(classify_error("User is banned"), ErrorKind::PermissionDenied));
    }

    #[test]
    fn test_classify_error_already_exists() {
        assert!(matches!(classify_error("User already exists"), ErrorKind::AlreadyExists));
        assert!(matches!(classify_error("Username already taken"), ErrorKind::AlreadyExists));
        assert!(matches!(classify_error("Email already registered"), ErrorKind::AlreadyExists));
    }

    #[test]
    fn test_classify_error_invalid_argument() {
        assert!(matches!(classify_error("Invalid email format"), ErrorKind::InvalidArgument));
        assert!(matches!(classify_error("Password too short"), ErrorKind::InvalidArgument));
        assert!(matches!(classify_error("Username too long"), ErrorKind::InvalidArgument));
        assert!(matches!(classify_error("Field cannot be empty"), ErrorKind::InvalidArgument));
        assert!(matches!(classify_error("Too many rooms"), ErrorKind::InvalidArgument));
        assert!(matches!(classify_error("Email required"), ErrorKind::InvalidArgument));
        assert!(matches!(classify_error("Password must be alphanumeric"), ErrorKind::InvalidArgument));
    }

    #[test]
    fn test_classify_error_internal() {
        assert!(matches!(classify_error("Something went wrong"), ErrorKind::Internal));
        assert!(matches!(classify_error("Database connection failed"), ErrorKind::Internal));
        assert!(matches!(classify_error("Unexpected error"), ErrorKind::Internal));
    }

    #[test]
    fn test_classify_error_case_insensitive() {
        assert!(matches!(classify_error("NOT FOUND"), ErrorKind::NotFound));
        assert!(matches!(classify_error("PERMISSION denied"), ErrorKind::PermissionDenied));
        assert!(matches!(classify_error("INVALID token"), ErrorKind::Unauthenticated));
    }

    // ========== Priority / Ordering Edge Cases ==========

    #[test]
    fn test_classify_error_not_found_takes_priority_over_invalid() {
        // "not found" contains "not" but should match NotFound, not InvalidArgument
        assert!(matches!(classify_error("Resource not found"), ErrorKind::NotFound));
    }

    #[test]
    fn test_classify_error_invalid_token_is_unauthenticated_not_invalid_argument() {
        // "invalid token" should match Unauthenticated (checked before InvalidArgument)
        assert!(matches!(classify_error("invalid token supplied"), ErrorKind::Unauthenticated));
    }

    #[test]
    fn test_classify_error_banned_is_permission_denied() {
        // "banned" should match PermissionDenied
        assert!(matches!(classify_error("User has been banned from the room"), ErrorKind::PermissionDenied));
    }

    #[test]
    fn test_classify_error_empty_string_is_internal() {
        assert!(matches!(classify_error(""), ErrorKind::Internal));
    }

    #[test]
    fn test_classify_error_whitespace_only_is_internal() {
        assert!(matches!(classify_error("   "), ErrorKind::Internal));
    }

    #[test]
    fn test_classify_error_must_be_is_invalid_argument() {
        assert!(matches!(classify_error("Username must be alphanumeric"), ErrorKind::InvalidArgument));
    }

    #[test]
    fn test_classify_error_not_allowed_is_permission_denied() {
        assert!(matches!(classify_error("This action is not allowed"), ErrorKind::PermissionDenied));
    }

    #[test]
    fn test_classify_error_already_registered_is_already_exists() {
        assert!(matches!(classify_error("User already registered"), ErrorKind::AlreadyExists));
    }

    #[test]
    fn test_classify_error_not_authenticated_is_unauthenticated() {
        assert!(matches!(classify_error("User is not authenticated"), ErrorKind::Unauthenticated));
    }

    #[test]
    fn test_classify_error_mixed_case_keywords() {
        assert!(matches!(classify_error("Token Expired"), ErrorKind::Unauthenticated));
        assert!(matches!(classify_error("Already Taken"), ErrorKind::AlreadyExists));
        assert!(matches!(classify_error("Cannot Be Empty"), ErrorKind::InvalidArgument));
    }

    // ========== Structured prefix classification ==========

    #[test]
    fn test_classify_by_prefix_core_error_display() {
        // These match the exact Display output of synctv_core::Error variants
        assert!(matches!(classify_error("Not found: room 123"), ErrorKind::NotFound));
        assert!(matches!(classify_error("Authentication error: expired"), ErrorKind::Unauthenticated));
        assert!(matches!(classify_error("Authorization error: forbidden"), ErrorKind::PermissionDenied));
        assert!(matches!(classify_error("Already exists: user"), ErrorKind::AlreadyExists));
        assert!(matches!(classify_error("Invalid input: bad field"), ErrorKind::InvalidArgument));
        assert!(matches!(classify_error("Internal error: oops"), ErrorKind::Internal));
        assert!(matches!(classify_error("Database error: connection refused"), ErrorKind::Internal));
    }

    #[test]
    fn test_api_error_classify() {
        let err = ApiError::NotFound("room".to_string());
        assert!(matches!(err.classify(), ErrorKind::NotFound));
        assert_eq!(err.message(), "room");

        let err = ApiError::Authentication("bad token".to_string());
        assert!(matches!(err.classify(), ErrorKind::Unauthenticated));

        let err = ApiError::Authorization("denied".to_string());
        assert!(matches!(err.classify(), ErrorKind::PermissionDenied));

        let err = ApiError::AlreadyExists("dup".to_string());
        assert!(matches!(err.classify(), ErrorKind::AlreadyExists));

        let err = ApiError::InvalidInput("bad".to_string());
        assert!(matches!(err.classify(), ErrorKind::InvalidArgument));

        let err = ApiError::Internal("boom".to_string());
        assert!(matches!(err.classify(), ErrorKind::Internal));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_api_error_display_roundtrips_through_classify() {
        // ApiError::Display produces prefixed strings that classify_by_prefix recognizes
        let cases: Vec<(ApiError, fn(&ErrorKind) -> bool)> = vec![
            (ApiError::NotFound("room".into()), |k| matches!(k, ErrorKind::NotFound)),
            (ApiError::Authentication("bad".into()), |k| matches!(k, ErrorKind::Unauthenticated)),
            (ApiError::Authorization("denied".into()), |k| matches!(k, ErrorKind::PermissionDenied)),
            (ApiError::AlreadyExists("dup".into()), |k| matches!(k, ErrorKind::AlreadyExists)),
            (ApiError::InvalidInput("bad".into()), |k| matches!(k, ErrorKind::InvalidArgument)),
            (ApiError::Internal("boom".into()), |k| matches!(k, ErrorKind::Internal)),
        ];
        for (api_err, check) in cases {
            let as_string = api_err.to_string();
            let classified = classify_error(&as_string);
            assert!(check(&classified), "ApiError '{}' misclassified after Display roundtrip", as_string);
        }
    }

    #[test]
    fn test_api_error_from_string_conversion() {
        let err = ApiError::NotFound("item".to_string());
        let s: String = err.into();
        assert!(s.starts_with("Not found: "));
    }
}
