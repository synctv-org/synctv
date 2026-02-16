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
pub mod providers;

// Re-export for convenience
pub use admin::AdminApiImpl;
pub use client::{ClientApiImpl, ClientApiConfig};
pub use email::EmailApiImpl;
pub use messaging::{StreamMessageHandler, MessageSender, ProtoCodec};
pub use notification::NotificationApiImpl;
pub use providers::{AlistApiImpl, BilibiliApiImpl, EmbyApiImpl};

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
    pub fn classify(&self) -> ErrorKind {
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

/// Classify either a typed `ApiError` or an untyped error string.
///
/// Prefer passing `ApiError` when available for guaranteed correct classification.
/// Falls back to `classify_error()` for legacy string errors.
pub fn classify_api_or_string_error(err: &str) -> ErrorKind {
    classify_error(err)
}

/// Classify an impls-layer error string into a semantic error kind.
///
/// First attempts to match known `synctv_core::Error` display prefixes
/// for structured classification. Falls back to keyword matching for
/// errors that don't originate from the core layer.
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
