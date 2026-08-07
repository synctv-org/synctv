use std::borrow::Cow;

use synctv_core::provider::ProviderError;

const UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE: &str =
    "Upstream provider service is temporarily unavailable.";

/// Application-level error codes for client-side programmatic handling.
pub mod error_codes {
    pub const UNSPECIFIED: i32 = 0;

    pub const UNAUTHENTICATED: i32 = 1000;
    pub const TOKEN_EXPIRED: i32 = 1001;
    pub const INVALID_CREDENTIALS: i32 = 1002;

    pub const NOT_FOUND: i32 = 2000;
    pub const ALREADY_EXISTS: i32 = 2001;
    pub const RESOURCE_EXHAUSTED: i32 = 2002;
    pub const CONFLICT: i32 = 2003;

    pub const INVALID_ARGUMENT: i32 = 3000;
    pub const INVALID_FORMAT: i32 = 3001;
    pub const VALUE_TOO_SHORT: i32 = 3002;
    pub const VALUE_TOO_LONG: i32 = 3003;
    pub const REQUIRED_FIELD_MISSING: i32 = 3004;

    pub const PERMISSION_DENIED: i32 = 4000;
    pub const FORBIDDEN: i32 = 4001;
    pub const BANNED: i32 = 4002;

    pub const INTERNAL_ERROR: i32 = 9000;
    pub const DATABASE_ERROR: i32 = 9001;
    pub const SERVICE_UNAVAILABLE: i32 = 9002;
    pub const TIMEOUT: i32 = 9003;
}

/// Shared error classification for protocol adapters and API runtimes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

pub trait ClassifiedError {
    fn classify(&self) -> ErrorKind;

    fn message(&self) -> Cow<'_, str>;
}

#[must_use]
pub fn classified_error_to_tonic_status(error: &impl ClassifiedError) -> tonic::Status {
    let message = match error.classify() {
        ErrorKind::Internal => Cow::Borrowed("Internal error"),
        _ => error.message(),
    };

    match error.classify() {
        ErrorKind::NotFound => tonic::Status::not_found(message),
        ErrorKind::Unauthenticated => tonic::Status::unauthenticated(message),
        ErrorKind::PermissionDenied => tonic::Status::permission_denied(message),
        ErrorKind::AlreadyExists => tonic::Status::already_exists(message),
        ErrorKind::Conflict => tonic::Status::aborted(message),
        ErrorKind::InvalidArgument => tonic::Status::invalid_argument(message),
        ErrorKind::RateLimited => tonic::Status::resource_exhausted(message),
        ErrorKind::ServiceUnavailable => tonic::Status::unavailable(message),
        ErrorKind::Timeout => tonic::Status::deadline_exceeded(message),
        ErrorKind::Internal => tonic::Status::internal(message),
    }
}

impl ClassifiedError for ProviderError {
    fn classify(&self) -> ErrorKind {
        match self {
            Self::NetworkError(_) | Self::ApiError(_) => ErrorKind::ServiceUnavailable,
            Self::UpstreamHttp { status, .. } => match *status {
                401 | 403 => ErrorKind::Unauthenticated,
                404 => ErrorKind::NotFound,
                409 => ErrorKind::Conflict,
                429 => ErrorKind::RateLimited,
                408 => ErrorKind::ServiceUnavailable,
                status if status >= 500 => ErrorKind::ServiceUnavailable,
                _ => ErrorKind::InvalidArgument,
            },
            Self::ParseError(_)
            | Self::InvalidConfig(_)
            | Self::InvalidUrl(_)
            | Self::MissingField(_)
            | Self::UnsupportedFormat(_)
            | Self::InvalidCredentialType
            | Self::EncryptionRequired(_)
            | Self::JsonError(_) => ErrorKind::InvalidArgument,
            Self::NotFound | Self::InstanceNotFound(_) | Self::CredentialNotFound(_) => {
                ErrorKind::NotFound
            }
            Self::MissingInstance => ErrorKind::NotFound,
            Self::AuthRequired
            | Self::Authentication(_)
            | Self::CredentialExpired(_)
            | Self::CredentialRequired => ErrorKind::Unauthenticated,
            Self::RouteRegistrationFailed(_) | Self::Internal(_) | Self::IoError(_) => {
                ErrorKind::Internal
            }
        }
    }

    fn message(&self) -> Cow<'_, str> {
        match self {
            Self::UpstreamHttp { status, .. } => match *status {
                401 | 403 => Cow::Borrowed("Provider authentication failed"),
                404 => Cow::Borrowed(synctv_common::messages::PROVIDER_RESOURCE_NOT_FOUND),
                409 => Cow::Borrowed("Upstream provider reported a request conflict."),
                429 => Cow::Borrowed("Upstream provider rate limited the request."),
                408 => Cow::Borrowed(UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE),
                status if status >= 500 => Cow::Borrowed(UPSTREAM_PROVIDER_UNAVAILABLE_MESSAGE),
                _ => Cow::Borrowed("Upstream provider rejected the request."),
            },
            Self::NetworkError(message)
            | Self::ApiError(message)
            | Self::ParseError(message)
            | Self::InvalidConfig(message)
            | Self::InvalidUrl(message)
            | Self::MissingField(message)
            | Self::UnsupportedFormat(message)
            | Self::InstanceNotFound(message)
            | Self::CredentialNotFound(message)
            | Self::Authentication(message)
            | Self::CredentialExpired(message)
            | Self::RouteRegistrationFailed(message)
            | Self::Internal(message) => Cow::Borrowed(message),
            Self::NotFound => Cow::Borrowed(synctv_common::messages::RESOURCE_NOT_FOUND),
            Self::MissingInstance => Cow::Borrowed("Provider instance not configured"),
            Self::AuthRequired => Cow::Borrowed(synctv_common::messages::AUTHENTICATION_REQUIRED),
            Self::CredentialRequired => Cow::Borrowed("Credential required"),
            Self::InvalidCredentialType => Cow::Borrowed("Invalid credential type"),
            Self::EncryptionRequired(provider) => Cow::Owned(format!(
                "Credential encryption required for provider '{provider}'"
            )),
            Self::IoError(error) => Cow::Owned(error.to_string()),
            Self::JsonError(error) => Cow::Owned(format!("Invalid data format: {error}")),
        }
    }
}
