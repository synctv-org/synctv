//! Unified API Implementation Layer
//!
//! This module contains the actual implementation of all APIs.
//! Both HTTP and gRPC handlers are thin wrappers that call these implementations.
//!
//! All methods use grpc-generated types for parameters and return values.
use synctv_livestream::error::StreamError;

pub mod admin;
pub mod client;
pub mod email;
pub mod messaging;
pub mod notification;
pub mod oauth2;
mod playback_snapshot;
mod playlist_items_snapshot;
pub mod providers;
pub mod request_context;
mod room_members_snapshot;
pub mod room_settings_snapshot;
pub(crate) mod validation;

// Re-export for convenience
pub use admin::AdminApiImpl;
pub use client::{ClientApiConfig, ClientApiImpl};
pub use email::EmailApiImpl;
pub use messaging::{
    HeartbeatSchedule, MessageConcurrencyConfig, MessageSender, ProtoCodec, StreamMessageHandler,
};
pub use notification::NotificationApiImpl;
pub use oauth2::OAuth2ApiImpl;
pub use providers::{AlistApiImpl, BilibiliApiImpl, EmbyApiImpl, ProviderCommonApiImpl};
pub use request_context::{
    EndpointRateLimitCategory, RequestContext, RequestExecutor, RequestMetadata, TransportProtocol,
};

const LIVESTREAM_NOT_AVAILABLE_MESSAGE: &str = "Live stream is not currently available";
const LIVESTREAM_PERMISSION_DENIED_MESSAGE: &str =
    "You do not have permission to access this live stream";
const LIVESTREAM_RATE_LIMITED_MESSAGE: &str =
    "Live streaming capacity limit reached. Please try again later.";
const LIVESTREAM_UNAVAILABLE_MESSAGE: &str =
    "Live streaming service is temporarily unavailable. Please try again later.";
const LIVESTREAM_REQUEST_FAILED_MESSAGE: &str = "Live streaming request failed";
const UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE: &str =
    "Upstream provider service is temporarily unavailable.";

pub fn validate_proto_request<M>(message: &M) -> Result<(), ApiError>
where
    M: prost_reflect::ReflectMessage,
{
    synctv_proto::validate(message).map_err(|error| ApiError::InvalidInput(error.to_string()))
}

pub(crate) fn proto_page_params(
    page: i32,
    page_size: i32,
    default_page_size: u32,
    max_page_size: u32,
) -> synctv_core::models::PageParams {
    let page = if page > 0 { page.cast_unsigned() } else { 1 };
    let default_page_size = default_page_size.clamp(1, max_page_size);
    let page_size = if page_size > 0 {
        page_size.cast_unsigned().clamp(1, max_page_size)
    } else {
        default_page_size
    };

    synctv_core::models::PageParams::new(Some(page), Some(page_size))
}

pub(crate) fn proto_page_size_usize(
    page_size: i32,
    default_page_size: u32,
    max_page_size: u32,
) -> usize {
    usize::try_from(proto_page_params(1, page_size, default_page_size, max_page_size).page_size)
        .unwrap_or(usize::MAX)
}

fn invalid_id_input(field: &'static str, err: impl std::fmt::Display) -> ApiError {
    ApiError::InvalidInput(format!("Invalid {field}: {err}"))
}

pub fn parse_id_param<T>(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<T, ApiError>
where
    T: synctv_core::PublicIdType,
{
    public_id_codec
        .decode::<T>(value.trim())
        .map_err(|err| invalid_id_input(field, err))
}

pub fn parse_user_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::UserId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn parse_room_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn parse_media_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::MediaId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn parse_playlist_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::PlaylistId, ApiError> {
    parse_id_param(value, field, public_id_codec)
}

pub fn proto_validated_id<T>(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<T, ApiError>
where
    T: synctv_core::PublicIdType,
{
    parse_id_param(value.as_ref(), T::TYPE_NAME, public_id_codec)
}

pub fn proto_validated_user_id(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::UserId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_room_id(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::RoomId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_media_id(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::MediaId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_playlist_id(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<synctv_core::models::PlaylistId, ApiError> {
    proto_validated_id(value, public_id_codec)
}

pub fn proto_validated_optional_id<T>(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Option<T>, ApiError>
where
    T: synctv_core::PublicIdType,
{
    let value = value.as_ref();
    if value.is_empty() {
        Ok(None)
    } else {
        proto_validated_id(value, public_id_codec).map(Some)
    }
}

pub fn proto_validated_optional_media_id(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Option<synctv_core::models::MediaId>, ApiError> {
    proto_validated_optional_id(value, public_id_codec)
}

pub fn proto_validated_optional_playlist_id(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Option<synctv_core::models::PlaylistId>, ApiError> {
    proto_validated_optional_id(value, public_id_codec)
}

pub fn proto_validated_optional_room_id(
    value: impl AsRef<str>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Option<synctv_core::models::RoomId>, ApiError> {
    proto_validated_optional_id(value, public_id_codec)
}

pub fn proto_validated_media_ids(
    values: Vec<String>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Vec<synctv_core::models::MediaId>, ApiError> {
    values
        .into_iter()
        .map(|value| proto_validated_media_id(&value, public_id_codec))
        .collect()
}

pub fn proto_validated_playlist_ids(
    values: Vec<String>,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Vec<synctv_core::models::PlaylistId>, ApiError> {
    values
        .into_iter()
        .map(|value| proto_validated_playlist_id(&value, public_id_codec))
        .collect()
}

pub fn parse_optional_media_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Option<synctv_core::models::MediaId>, ApiError> {
    parse_optional_id_param(value, field, public_id_codec)
}

pub fn parse_optional_playlist_id_param(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Option<synctv_core::models::PlaylistId>, ApiError> {
    parse_optional_id_param(value, field, public_id_codec)
}

pub fn parse_optional_id_param<T>(
    value: &str,
    field: &'static str,
    public_id_codec: &crate::PublicIdCodec,
) -> Result<Option<T>, ApiError>
where
    T: synctv_core::PublicIdType,
{
    if value.trim().is_empty() {
        Ok(None)
    } else {
        parse_id_param(value, field, public_id_codec).map(Some)
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
    pub const CONFLICT: i32 = 2003;

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

    // Internal errors (9xxx)
    pub const INTERNAL_ERROR: i32 = 9000;
    pub const DATABASE_ERROR: i32 = 9001;
    pub const SERVICE_UNAVAILABLE: i32 = 9002;
    pub const TIMEOUT: i32 = 9003;
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
    Conflict,
    InvalidArgument,
    RateLimited,
    ServiceUnavailable,
    Timeout,
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
            Self::Conflict => error_codes::CONFLICT,
            Self::InvalidArgument => error_codes::INVALID_ARGUMENT,
            Self::RateLimited => error_codes::RESOURCE_EXHAUSTED,
            Self::ServiceUnavailable => error_codes::SERVICE_UNAVAILABLE,
            Self::Timeout => error_codes::TIMEOUT,
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
    Conflict(String),
    InvalidInput(String),
    RateLimited(String),
    RateLimitedWithRetry {
        message: String,
        retry_after_seconds: u64,
    },
    ServiceUnavailable(String),
    Timeout(String),
    Internal(String),
}

impl From<synctv_core::Error> for ApiError {
    fn from(err: synctv_core::Error) -> Self {
        match err {
            synctv_core::Error::NotFound(msg) => Self::NotFound(msg),
            synctv_core::Error::Authentication(msg) => Self::Authentication(msg),
            synctv_core::Error::EmailNotVerified => Self::Authorization(
                "Email not verified. Please verify your email to continue.".to_string(),
            ),
            synctv_core::Error::Authorization(msg) => Self::Authorization(msg),
            synctv_core::Error::AlreadyExists(msg) => Self::AlreadyExists(msg),
            synctv_core::Error::Conflict(msg) | synctv_core::Error::LockConflict(msg) => {
                Self::Conflict(msg)
            }
            synctv_core::Error::OptimisticLockConflict => {
                Self::Conflict("Resource modified concurrently".to_string())
            }
            synctv_core::Error::InvalidInput(msg) => Self::InvalidInput(msg),
            synctv_core::Error::RateLimited(msg) => Self::RateLimited(msg),
            synctv_core::Error::ServiceUnavailable(msg) => Self::ServiceUnavailable(msg),
            synctv_core::Error::Timeout(msg) => Self::Timeout(msg),
            synctv_core::Error::Internal(msg) if msg.starts_with("Redis timeout:") => {
                Self::ServiceUnavailable(msg)
            }
            synctv_core::Error::Database(err) => {
                tracing::error!("Database error mapped to service unavailable: {}", err);
                Self::ServiceUnavailable(
                    "Service temporarily unavailable. Please try again later.".to_string(),
                )
            }
            synctv_core::Error::Redis(err) => {
                tracing::error!("Redis error mapped to service unavailable: {}", err);
                Self::ServiceUnavailable(
                    "Service temporarily unavailable. Please try again later.".to_string(),
                )
            }
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<synctv_core::provider::ProviderError> for ApiError {
    fn from(err: synctv_core::provider::ProviderError) -> Self {
        use synctv_core::provider::ProviderError;

        match err {
            ProviderError::NetworkError(msg) | ProviderError::ApiError(msg) => {
                Self::ServiceUnavailable(msg)
            }
            ProviderError::UpstreamHttp { status, .. } => match status {
                401 | 403 => {
                    tracing::warn!(status, "Upstream provider authentication failure");
                    Self::Authentication("Provider authentication failed".to_string())
                }
                404 => {
                    tracing::info!(status, "Upstream provider resource not found");
                    Self::NotFound(synctv_common::messages::PROVIDER_RESOURCE_NOT_FOUND.to_string())
                }
                409 => {
                    tracing::warn!(status, "Upstream provider reported a request conflict");
                    Self::Conflict("Upstream provider reported a request conflict.".to_string())
                }
                429 => {
                    tracing::warn!(status, "Upstream provider rate limited request");
                    Self::RateLimited("Upstream provider rate limited the request.".to_string())
                }
                408 => {
                    tracing::warn!(status, "Upstream provider unavailable");
                    Self::ServiceUnavailable(UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE.to_string())
                }
                status if status >= 500 => {
                    tracing::warn!(status, "Upstream provider unavailable");
                    Self::ServiceUnavailable(UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE.to_string())
                }
                _ => {
                    tracing::warn!(status, "Upstream provider rejected request");
                    Self::InvalidInput("Upstream provider rejected the request.".to_string())
                }
            },
            ProviderError::ParseError(msg)
            | ProviderError::InvalidConfig(msg)
            | ProviderError::InvalidUrl(msg)
            | ProviderError::MissingField(msg)
            | ProviderError::UnsupportedFormat(msg) => Self::InvalidInput(msg),
            ProviderError::NotFound => {
                Self::NotFound(synctv_common::messages::RESOURCE_NOT_FOUND.to_string())
            }
            ProviderError::InstanceNotFound(msg) | ProviderError::CredentialNotFound(msg) => {
                Self::NotFound(msg)
            }
            ProviderError::MissingInstance => {
                Self::NotFound("Provider instance not configured".to_string())
            }
            ProviderError::AuthRequired => {
                Self::Authentication(synctv_common::messages::AUTHENTICATION_REQUIRED.to_string())
            }
            ProviderError::Authentication(msg) | ProviderError::CredentialExpired(msg) => {
                Self::Authentication(msg)
            }
            ProviderError::CredentialRequired => {
                Self::Authentication("Credential required".to_string())
            }
            ProviderError::InvalidCredentialType => {
                Self::InvalidInput("Invalid credential type".to_string())
            }
            ProviderError::RouteRegistrationFailed(msg) | ProviderError::Internal(msg) => {
                Self::Internal(msg)
            }
            ProviderError::IoError(e) => Self::Internal(e.to_string()),
            ProviderError::JsonError(e) => Self::InvalidInput(format!("Invalid data format: {e}")),
            ProviderError::EncryptionRequired(provider) => Self::InvalidInput(format!(
                "Credential encryption required for provider '{provider}'"
            )),
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
            Self::Conflict(_) => ErrorKind::Conflict,
            Self::InvalidInput(_) => ErrorKind::InvalidArgument,
            Self::RateLimited(_) | Self::RateLimitedWithRetry { .. } => ErrorKind::RateLimited,
            Self::ServiceUnavailable(_) => ErrorKind::ServiceUnavailable,
            Self::Timeout(_) => ErrorKind::Timeout,
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
            | Self::Conflict(msg)
            | Self::InvalidInput(msg)
            | Self::RateLimited(msg)
            | Self::ServiceUnavailable(msg)
            | Self::Timeout(msg)
            | Self::Internal(msg) => msg,
            Self::RateLimitedWithRetry { message, .. } => message,
        }
    }

    #[must_use]
    pub const fn retry_after_seconds(&self) -> Option<u64> {
        match self {
            Self::RateLimitedWithRetry {
                retry_after_seconds,
                ..
            } => Some(*retry_after_seconds),
            _ => None,
        }
    }

    /// Get the application-level error code for this error.
    #[must_use]
    pub const fn code(&self) -> i32 {
        self.classify().to_code()
    }
}

pub(crate) fn find_livestream_stream_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a StreamError> {
    let mut current = Some(error);
    while let Some(err) = current {
        if let Some(stream_error) = err.downcast_ref::<StreamError>() {
            return Some(stream_error);
        }
        current = err.source();
    }

    None
}

#[must_use]
pub(crate) fn map_livestream_stream_error(stream_error: &StreamError) -> ApiError {
    match stream_error {
        StreamError::NoPublisher(_)
        | StreamError::StreamNotFound(_)
        | StreamError::InvalidStreamKey(_) => {
            ApiError::NotFound(LIVESTREAM_NOT_AVAILABLE_MESSAGE.to_string())
        }
        StreamError::PermissionDenied(_) | StreamError::AuthenticationFailed(_) => {
            ApiError::Authorization(LIVESTREAM_PERMISSION_DENIED_MESSAGE.to_string())
        }
        StreamError::ResourceExhausted(_) => {
            ApiError::RateLimited(LIVESTREAM_RATE_LIMITED_MESSAGE.to_string())
        }
        StreamError::InvalidAddress(_)
        | StreamError::ProtocolError(_)
        | StreamError::HandshakeFailed(_)
        | StreamError::InvalidState(_)
        | StreamError::RedisError(_)
        | StreamError::RegistryError(_)
        | StreamError::GrpcError(_)
        | StreamError::ConnectionFailed(_)
        | StreamError::StaleEpoch(_)
        | StreamError::StreamHubError(_) => {
            ApiError::ServiceUnavailable(LIVESTREAM_UNAVAILABLE_MESSAGE.to_string())
        }
        StreamError::IoError(_)
        | StreamError::Internal(_)
        | StreamError::AlreadyPublishing(_)
        | StreamError::PublisherExists(_) => {
            ApiError::Internal(LIVESTREAM_REQUEST_FAILED_MESSAGE.to_string())
        }
    }
}

#[must_use]
pub(crate) fn map_livestream_backend_error(error: &(dyn std::error::Error + 'static)) -> ApiError {
    if let Some(stream_error) = find_livestream_stream_error(error) {
        return map_livestream_stream_error(stream_error);
    }

    tracing::error!(error = %error, "Unexpected livestream backend error");
    ApiError::Internal(LIVESTREAM_REQUEST_FAILED_MESSAGE.to_string())
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
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
            ErrorKind::Conflict => Self::Conflict(msg),
            ErrorKind::InvalidArgument => Self::InvalidInput(msg),
            ErrorKind::RateLimited => Self::RateLimited(msg),
            ErrorKind::ServiceUnavailable => Self::ServiceUnavailable(msg),
            ErrorKind::Timeout => Self::Timeout(msg),
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
    } else if lower.contains("unauthenticated")
        || lower.contains("invalid token")
        || lower.contains("token expired")
        || lower.contains("not authenticated")
    {
        ErrorKind::Unauthenticated
    } else if lower.contains("permission")
        || lower.contains("forbidden")
        || lower.contains("not allowed")
        || lower.contains("no longer allowed")
        || lower.contains("not accepting new connections")
        || lower.contains("account is no longer available")
        || lower.contains("banned")
    {
        ErrorKind::PermissionDenied
    } else if lower.contains("already exists")
        || lower.contains("already taken")
        || lower.contains("already registered")
    {
        ErrorKind::AlreadyExists
    } else if lower.contains("optimistic lock conflict")
        || lower.contains("modified concurrently")
        || lower.contains("lock conflict")
    {
        ErrorKind::Conflict
    } else if lower.contains("room at capacity")
        || lower.contains("user at capacity")
        || lower.contains("server at capacity")
        || lower.contains("room capacity exceeded")
        || lower.contains("realtime room capacity exceeded")
        || lower.contains("too many connections for this user")
    {
        ErrorKind::RateLimited
    } else if lower.contains("distributed room capacity check unavailable")
        || lower.contains("distributed user connection check unavailable")
        || lower.contains("distributed total connection check unavailable")
    {
        ErrorKind::ServiceUnavailable
    } else if lower.contains("invalid")
        || lower.contains("too short")
        || lower.contains("too long")
        || lower.contains("cannot be empty")
        || lower.contains("too many")
        || lower.contains("required")
        || lower.contains("must be")
        || lower.contains("must be formatted")
        || lower.contains("is no longer active")
        || lower.contains("does not match")
        || lower.contains("not currently joined")
        || lower.contains("not in this room")
        || lower.contains("has not joined webrtc")
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
    } else if err.starts_with("Conflict: ")
        || err.starts_with("Optimistic lock conflict")
        || err.starts_with("Distributed lock conflict: ")
    {
        Some(ErrorKind::Conflict)
    } else if err.starts_with("Invalid input: ") {
        Some(ErrorKind::InvalidArgument)
    } else if err.starts_with("Rate limited: ") {
        Some(ErrorKind::RateLimited)
    } else if err.starts_with("Service unavailable: ") {
        Some(ErrorKind::ServiceUnavailable)
    } else if err.starts_with("Operation timeout: ") {
        Some(ErrorKind::Timeout)
    } else if err.starts_with("Internal error: ")
        || err.starts_with("Database error: ")
        || err.starts_with("Redis error: ")
        || err.starts_with("Serialization error: ")
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
    fn test_validate_proto_request_maps_protovalidate_error_to_invalid_input() {
        let request = crate::proto::client::StartOpaqueRegistrationRequest {
            username: "ab".to_string(),
            email: "not-an-email".to_string(),
            registration_request: Vec::new(),
        };

        let error = validate_proto_request(&request).unwrap_err();

        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains("username"), "{message}");
                assert!(message.contains("email"), "{message}");
                assert!(message.contains("registration_request"), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    fn assert_invalid_proto_request<M>(request: &M, expected: &str)
    where
        M: prost::Message + prost_reflect::ReflectMessage + Default,
    {
        let error = validate_proto_request(request).unwrap_err();
        match error {
            ApiError::InvalidInput(message) => {
                assert!(message.contains(expected), "{message}");
            }
            other => panic!("expected invalid input, got {other:?}"),
        }
    }

    #[test]
    fn test_validate_proto_request_rejects_empty_admin_batch_ids() {
        assert_invalid_proto_request(
            &crate::proto::admin::BatchBanUsersRequest::default(),
            "user_ids",
        );
        assert_invalid_proto_request(
            &crate::proto::admin::BatchDeleteUsersRequest::default(),
            "user_ids",
        );
        assert_invalid_proto_request(
            &crate::proto::admin::BatchBanRoomsRequest::default(),
            "room_ids",
        );
        assert_invalid_proto_request(
            &crate::proto::admin::BatchDeleteRoomsRequest::default(),
            "room_ids",
        );
    }

    #[test]
    fn test_validate_proto_request_rejects_admin_batch_ban_reason_over_limit() {
        assert_invalid_proto_request(
            &crate::proto::admin::BatchBanUsersRequest {
                user_ids: vec!["usr_abc123".to_string()],
                reason: "x".repeat(501),
            },
            "reason",
        );
        assert_invalid_proto_request(
            &crate::proto::admin::BatchBanRoomsRequest {
                room_ids: vec!["room_abc123".to_string()],
                reason: "x".repeat(501),
            },
            "reason",
        );
    }

    #[test]
    fn test_validate_proto_request_rejects_invalid_settings_group() {
        assert_invalid_proto_request(
            &crate::proto::admin::GetSettingsGroupRequest {
                group: "bad/group".to_string(),
            },
            "group",
        );
    }

    #[test]
    fn test_classify_error_not_found() {
        assert!(matches!(
            classify_error("User not found"),
            ErrorKind::NotFound
        ));
        assert!(matches!(
            classify_error("Room Not Found"),
            ErrorKind::NotFound
        ));
        assert!(matches!(
            classify_error("resource NOT FOUND"),
            ErrorKind::NotFound
        ));
    }

    #[test]
    fn test_classify_error_unauthenticated() {
        assert!(matches!(
            classify_error("Unauthenticated"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("invalid token"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Token expired"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Not authenticated"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_permission_denied() {
        assert!(matches!(
            classify_error("Permission denied"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("Forbidden access"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("Operation not allowed"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("User is banned"),
            ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn test_classify_error_already_exists() {
        assert!(matches!(
            classify_error("User already exists"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Username already taken"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Email already registered"),
            ErrorKind::AlreadyExists
        ));
    }

    #[test]
    fn test_classify_error_invalid_argument() {
        assert!(matches!(
            classify_error("Invalid email format"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Password too short"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Username too long"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Field cannot be empty"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Too many rooms"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Email required"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Password must be alphanumeric"),
            ErrorKind::InvalidArgument
        ));
    }

    #[test]
    fn test_classify_error_internal() {
        assert!(matches!(
            classify_error("Something went wrong"),
            ErrorKind::Internal
        ));
        assert!(matches!(
            classify_error("Database connection failed"),
            ErrorKind::Internal
        ));
        assert!(matches!(
            classify_error("Unexpected error"),
            ErrorKind::Internal
        ));
    }

    #[test]
    fn test_classify_error_case_insensitive() {
        assert!(matches!(classify_error("NOT FOUND"), ErrorKind::NotFound));
        assert!(matches!(
            classify_error("PERMISSION denied"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("INVALID token"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_not_found_takes_priority_over_invalid() {
        // "not found" contains "not" but should match NotFound, not InvalidArgument
        assert!(matches!(
            classify_error("Resource not found"),
            ErrorKind::NotFound
        ));
    }

    #[test]
    fn test_classify_error_invalid_token_is_unauthenticated_not_invalid_argument() {
        // "invalid token" should match Unauthenticated (checked before InvalidArgument)
        assert!(matches!(
            classify_error("invalid token supplied"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_banned_is_permission_denied() {
        // "banned" should match PermissionDenied
        assert!(matches!(
            classify_error("User has been banned from the room"),
            ErrorKind::PermissionDenied
        ));
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
        assert!(matches!(
            classify_error("Username must be alphanumeric"),
            ErrorKind::InvalidArgument
        ));
    }

    #[test]
    fn test_classify_error_not_allowed_is_permission_denied() {
        assert!(matches!(
            classify_error("This action is not allowed"),
            ErrorKind::PermissionDenied
        ));
    }

    #[test]
    fn test_classify_error_already_registered_is_already_exists() {
        assert!(matches!(
            classify_error("User already registered"),
            ErrorKind::AlreadyExists
        ));
    }

    #[test]
    fn test_classify_error_not_authenticated_is_unauthenticated() {
        assert!(matches!(
            classify_error("User is not authenticated"),
            ErrorKind::Unauthenticated
        ));
    }

    #[test]
    fn test_classify_error_mixed_case_keywords() {
        assert!(matches!(
            classify_error("Token Expired"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Already Taken"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Cannot Be Empty"),
            ErrorKind::InvalidArgument
        ));
    }

    #[test]
    fn test_classify_by_prefix_core_error_display() {
        // These match the exact Display output of synctv_core::Error variants
        assert!(matches!(
            classify_error("Not found: room 123"),
            ErrorKind::NotFound
        ));
        assert!(matches!(
            classify_error("Authentication error: expired"),
            ErrorKind::Unauthenticated
        ));
        assert!(matches!(
            classify_error("Authorization error: forbidden"),
            ErrorKind::PermissionDenied
        ));
        assert!(matches!(
            classify_error("Already exists: user"),
            ErrorKind::AlreadyExists
        ));
        assert!(matches!(
            classify_error("Conflict: upstream provider reported a request conflict"),
            ErrorKind::Conflict
        ));
        assert!(matches!(
            classify_error("Invalid input: bad field"),
            ErrorKind::InvalidArgument
        ));
        assert!(matches!(
            classify_error("Internal error: oops"),
            ErrorKind::Internal
        ));
        assert!(matches!(
            classify_error("Database error: connection refused"),
            ErrorKind::Internal
        ));
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

        let err = ApiError::RateLimited("too fast".to_string());
        assert!(matches!(err.classify(), ErrorKind::RateLimited));

        let err = ApiError::ServiceUnavailable("redis unavailable".to_string());
        assert!(matches!(err.classify(), ErrorKind::ServiceUnavailable));

        let err = ApiError::Timeout("request timed out".to_string());
        assert!(matches!(err.classify(), ErrorKind::Timeout));

        let err = ApiError::Internal("boom".to_string());
        assert!(matches!(err.classify(), ErrorKind::Internal));

        let err = ApiError::RateLimitedWithRetry {
            message: "too fast".to_string(),
            retry_after_seconds: 30,
        };
        assert!(matches!(err.classify(), ErrorKind::RateLimited));
        assert_eq!(err.retry_after_seconds(), Some(30));
    }

    #[test]
    fn test_api_error_display_uses_user_message() {
        let cases = [
            ApiError::NotFound("room not found".into()),
            ApiError::Authentication("invalid token".into()),
            ApiError::Authorization("not a member of this room".into()),
            ApiError::AlreadyExists("user already exists".into()),
            ApiError::InvalidInput("username is required".into()),
            ApiError::RateLimited("room at capacity".into()),
            ApiError::RateLimitedWithRetry {
                message: "email rate limited".into(),
                retry_after_seconds: 42,
            },
            ApiError::ServiceUnavailable("backend unavailable".into()),
            ApiError::Internal("internal details".into()),
        ];

        for api_err in cases {
            assert_eq!(api_err.to_string(), api_err.message());
        }
    }

    #[test]
    fn test_classify_error_room_capacity_is_rate_limited() {
        assert!(matches!(
            classify_error("Room at capacity (42 connections, max: 40)"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Room at capacity across all replicas (42 connections)"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Server at capacity across all replicas (42 connections)"),
            ErrorKind::RateLimited
        ));
    }

    #[test]
    fn test_classify_error_user_capacity_is_rate_limited() {
        assert!(matches!(
            classify_error("User at capacity (4 connections, max: 3)"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Too many connections for this user across all replicas (max 3)"),
            ErrorKind::RateLimited
        ));
    }

    #[test]
    fn test_classify_error_distributed_capacity_check_is_service_unavailable() {
        assert!(matches!(
            classify_error(
                "Distributed room capacity check unavailable; refusing room join while cluster Redis is degraded"
            ),
            ErrorKind::ServiceUnavailable
        ));
        assert!(matches!(
            classify_error(
                "Distributed user connection check unavailable; refusing new connection while cluster Redis is degraded"
            ),
            ErrorKind::ServiceUnavailable
        ));
        assert!(matches!(
            classify_error(
                "Distributed total connection check unavailable; refusing new connection while cluster Redis is degraded"
            ),
            ErrorKind::ServiceUnavailable
        ));
    }

    #[test]
    fn test_api_error_from_string_conversion() {
        let err = ApiError::NotFound("item".to_string());
        let s: String = err.into();
        assert_eq!(s, "item");
    }

    #[test]
    fn test_api_error_from_core_rate_limited() {
        // Test that synctv_core::Error::RateLimited is correctly converted to ApiError::RateLimited
        let core_err = synctv_core::Error::RateLimited("too many requests".to_string());
        let api_err = ApiError::from(core_err);
        assert!(matches!(api_err, ApiError::RateLimited(ref msg) if msg == "too many requests"));
        assert!(matches!(api_err.classify(), ErrorKind::RateLimited));
        assert_eq!(api_err.code(), error_codes::RESOURCE_EXHAUSTED);
    }

    #[test]
    fn test_api_error_from_core_service_unavailable() {
        let core_err = synctv_core::Error::ServiceUnavailable("redis unavailable".to_string());
        let api_err = ApiError::from(core_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg) if msg == "redis unavailable"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_core_timeout_maps_to_timeout() {
        let core_err = synctv_core::Error::Timeout("oauth2 provider timed out".to_string());
        let api_err = ApiError::from(core_err);
        assert!(matches!(
            api_err,
            ApiError::Timeout(ref msg) if msg == "oauth2 provider timed out"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::Timeout));
        assert_eq!(api_err.code(), error_codes::TIMEOUT);
    }

    #[test]
    fn test_api_error_from_core_database_maps_to_service_unavailable() {
        let core_err = synctv_core::Error::Database(sqlx::Error::PoolTimedOut);
        let api_err = ApiError::from(core_err);
        assert!(
            matches!(api_err, ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "database infrastructure failures must remain service unavailable, got: {api_err:?}"
        );
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_core_redis_maps_to_service_unavailable() {
        let redis_err = redis::RedisError::from((redis::ErrorKind::Io, "connection reset by peer"));
        let api_err = ApiError::from(synctv_core::Error::Redis(redis_err));
        assert!(
            matches!(api_err, ApiError::ServiceUnavailable(ref msg) if msg == "Service temporarily unavailable. Please try again later."),
            "redis infrastructure failures must remain service unavailable, got: {api_err:?}"
        );
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_network_error_maps_to_service_unavailable() {
        let provider_err =
            synctv_core::provider::ProviderError::NetworkError("connection refused".to_string());
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg) if msg == "connection refused"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_api_error_maps_to_service_unavailable() {
        let provider_err =
            synctv_core::provider::ProviderError::ApiError("upstream provider down".to_string());
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg) if msg == "upstream provider down"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_authentication_maps_to_authentication() {
        let provider_err = synctv_core::provider::ProviderError::Authentication(
            "provider rejected credentials".to_string(),
        );
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::Authentication(ref msg) if msg == "provider rejected credentials"
        ));
        assert!(matches!(api_err.classify(), ErrorKind::Unauthenticated));
        assert_eq!(api_err.code(), error_codes::UNAUTHENTICATED);
    }

    #[test]
    fn test_api_error_from_provider_upstream_5xx_maps_to_service_unavailable() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 503,
            url: "https://provider.example/api".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg)
                if msg == "Upstream provider service is temporarily unavailable."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_upstream_408_maps_to_service_unavailable() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 408,
            url: "https://provider.example/api?token=secret".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::ServiceUnavailable(ref msg)
                if msg == "Upstream provider service is temporarily unavailable."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::ServiceUnavailable));
        assert_eq!(api_err.code(), error_codes::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_api_error_from_provider_upstream_409_maps_to_conflict() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 409,
            url: "https://provider.example/api?token=secret".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::Conflict(ref msg)
                if msg == "Upstream provider reported a request conflict."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::Conflict));
        assert_eq!(api_err.code(), error_codes::CONFLICT);
    }

    #[test]
    fn test_api_error_from_provider_upstream_429_maps_to_rate_limited() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 429,
            url: "https://provider.example/api?token=secret".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::RateLimited(ref msg)
                if msg == "Upstream provider rate limited the request."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::RateLimited));
        assert_eq!(api_err.code(), error_codes::RESOURCE_EXHAUSTED);
    }

    #[test]
    fn test_api_error_from_provider_upstream_404_maps_to_not_found() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 404,
            url: "https://provider.example/api".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::NotFound(ref msg)
                if msg == synctv_common::messages::PROVIDER_RESOURCE_NOT_FOUND
        ));
        assert!(matches!(api_err.classify(), ErrorKind::NotFound));
        assert_eq!(api_err.code(), error_codes::NOT_FOUND);
    }

    #[test]
    fn test_api_error_from_provider_upstream_400_maps_to_invalid_input() {
        let provider_err = synctv_core::provider::ProviderError::UpstreamHttp {
            status: 400,
            url: "https://provider.example/api".to_string(),
        };
        let api_err = ApiError::from(provider_err);
        assert!(matches!(
            api_err,
            ApiError::InvalidInput(ref msg) if msg == "Upstream provider rejected the request."
        ));
        assert!(matches!(api_err.classify(), ErrorKind::InvalidArgument));
        assert_eq!(api_err.code(), error_codes::INVALID_ARGUMENT);
    }

    #[test]
    fn test_api_error_from_provider_encryption_required_maps_to_invalid_input() {
        let provider_err = synctv_core::provider::ProviderError::EncryptionRequired("bilibili");
        let api_err = ApiError::from(provider_err);

        assert!(
            matches!(
                api_err,
                ApiError::InvalidInput(ref msg)
                    if msg == "Credential encryption required for provider 'bilibili'"
            ),
            "credential encryption precondition failures must not be hidden as internal errors, got: {api_err:?}"
        );
        assert!(matches!(api_err.classify(), ErrorKind::InvalidArgument));
        assert_eq!(api_err.code(), error_codes::INVALID_ARGUMENT);
    }

    #[test]
    fn test_classify_by_prefix_rate_limited() {
        assert!(matches!(
            classify_error("Rate limited: too fast"),
            ErrorKind::RateLimited
        ));
        assert!(matches!(
            classify_error("Operation timeout: request budget exceeded"),
            ErrorKind::Timeout
        ));
    }

    #[test]
    fn test_api_error_rate_limited_display() {
        let err = ApiError::RateLimited("exceeded quota".to_string());
        let display = err.to_string();
        assert_eq!(display, "exceeded quota");
    }

    #[test]
    fn test_api_error_service_unavailable_display() {
        let err = ApiError::ServiceUnavailable("redis unavailable".to_string());
        let display = err.to_string();
        assert_eq!(display, "redis unavailable");
    }

    #[test]
    fn test_proto_page_params_uses_endpoint_default_for_zero_page_size() {
        let params = proto_page_params(0, 0, 50, 100);

        assert_eq!(params.page, 1);
        assert_eq!(params.page_size, 50);
    }

    #[test]
    fn test_proto_page_params_clamps_explicit_page_size() {
        let params = proto_page_params(2, 500, 50, 100);

        assert_eq!(params.page, 2);
        assert_eq!(params.page_size, 100);
    }

    #[test]
    fn test_proto_page_size_usize_uses_endpoint_default() {
        assert_eq!(proto_page_size_usize(0, 50, 100), 50);
    }
}
