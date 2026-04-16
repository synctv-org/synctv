use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Database error: {0}")]
    Database(sqlx::Error),

    #[error("Redis error: {0}")]
    Redis(#[from] redis::RedisError),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Deserialization error: {context}")]
    Deserialization { context: String },

    #[error("Authentication error: {0}")]
    Authentication(String),

    #[error("Email not verified")]
    EmailNotVerified,

    #[error("Authorization error: {0}")]
    Authorization(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Already exists: {0}")]
    AlreadyExists(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Service unavailable: {0}")]
    ServiceUnavailable(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Optimistic lock conflict")]
    OptimisticLockConflict,

    #[error("Distributed lock conflict: {0}")]
    LockConflict(String),

    #[error("Operation timeout: {0}")]
    Timeout(String),
}

impl From<sqlx::Error> for Error {
    fn from(err: sqlx::Error) -> Self {
        match &err {
            // Map "no rows" to NotFound
            sqlx::Error::RowNotFound => Self::NotFound("Resource not found".to_string()),
            // Map unique constraint violations to AlreadyExists
            sqlx::Error::Database(db_err) => {
                let code = db_err.code().unwrap_or_default();
                match code.as_ref() {
                    // PostgreSQL unique_violation
                    "23505" => {
                        let constraint = db_err.constraint().unwrap_or_default();
                        if constraint.contains("username") {
                            Self::AlreadyExists("Username already taken".to_string())
                        } else if constraint.contains("email") {
                            Self::AlreadyExists("Email already registered".to_string())
                        } else {
                            Self::AlreadyExists("Resource already exists".to_string())
                        }
                    }
                    // PostgreSQL foreign_key_violation
                    "23503" => Self::NotFound("Referenced resource not found".to_string()),
                    // PostgreSQL check_violation
                    "23514" => Self::InvalidInput("Constraint check failed".to_string()),
                    // PostgreSQL not_null_violation
                    "23502" => Self::InvalidInput("Required field is missing".to_string()),
                    _ => Self::Database(err),
                }
            }
            _ => Self::Database(err),
        }
    }
}

impl From<anyhow::Error> for Error {
    fn from(err: anyhow::Error) -> Self {
        // Preserve error chain information for better debugging
        let mut msg = String::new();
        for (i, cause) in err.chain().enumerate() {
            if i > 0 {
                msg.push_str(": ");
            }
            msg.push_str(&cause.to_string());
        }
        Self::Internal(msg)
    }
}

impl From<crate::provider::ProviderError> for Error {
    fn from(err: crate::provider::ProviderError) -> Self {
        use crate::provider::ProviderError;
        match err {
            // Network-related errors -> Timeout for transient issues
            ProviderError::NetworkError(msg) => {
                Self::Timeout(format!("Provider network error: {msg}"))
            }
            // Authentication errors
            ProviderError::AuthRequired
            | ProviderError::CredentialRequired
            | ProviderError::InvalidCredentialType => {
                Self::Authentication("Provider authentication required".to_string())
            }
            // Configuration errors
            ProviderError::InvalidConfig(msg) => {
                Self::InvalidInput(format!("Invalid provider configuration: {msg}"))
            }
            ProviderError::MissingField(field) => {
                Self::InvalidInput(format!("Missing required field: {field}"))
            }
            ProviderError::MissingInstance | ProviderError::InstanceNotFound(_) => {
                Self::NotFound("Provider instance not found".to_string())
            }
            // Not found
            ProviderError::NotFound => Self::NotFound("Provider resource not found".to_string()),
            // Invalid URL
            ProviderError::InvalidUrl(msg) => Self::InvalidInput(format!("Invalid URL: {msg}")),
            // Upstream HTTP errors
            ProviderError::UpstreamHttp { status, .. } => {
                if status == 401 || status == 403 {
                    tracing::warn!(status, "Provider upstream authentication failure");
                    Self::Authentication("Provider authentication failed".to_string())
                } else if status == 404 {
                    tracing::info!(status, "Provider upstream resource not found");
                    Self::NotFound("Provider resource not found".to_string())
                } else if status == 408 || status == 429 || status >= 500 {
                    tracing::warn!(status, "Provider upstream unavailable");
                    Self::Timeout(
                        "Upstream provider service is temporarily unavailable.".to_string(),
                    )
                } else {
                    tracing::warn!(status, "Provider upstream rejected request");
                    Self::InvalidInput("Upstream provider rejected the request.".to_string())
                }
            }
            // Encryption required
            ProviderError::EncryptionRequired(provider) => Self::InvalidInput(format!(
                "Credential encryption required for provider '{provider}'"
            )),
            // Credential errors
            ProviderError::CredentialNotFound(msg) => {
                Self::NotFound(format!("Credential not found: {msg}"))
            }
            ProviderError::CredentialExpired(msg) => {
                Self::Authentication(format!("Credential expired: {msg}"))
            }
            // Internal errors
            ProviderError::Internal(msg) => Self::Internal(format!("Provider error: {msg}")),
            // API errors - could be various things
            ProviderError::ApiError(msg) => Self::Internal(format!("Provider API error: {msg}")),
            // Format/parse errors
            ProviderError::UnsupportedFormat(fmt) => {
                Self::InvalidInput(format!("Unsupported format: {fmt}"))
            }
            ProviderError::ParseError(msg) => Self::InvalidInput(format!("Parse error: {msg}")),
            // Route registration
            ProviderError::RouteRegistrationFailed(msg) => {
                Self::Internal(format!("Route registration failed: {msg}"))
            }
            // IO/JSON errors
            ProviderError::IoError(e) => Self::Internal(format!("IO error: {e}")),
            ProviderError::JsonError(e) => Self::Internal(format!("JSON error: {e}")),
        }
    }
}

impl From<Error> for tonic::Status {
    fn from(err: Error) -> Self {
        match err {
            Error::NotFound(msg) => Self::not_found(msg),
            Error::Authentication(msg) => Self::unauthenticated(msg),
            Error::EmailNotVerified => {
                Self::permission_denied("Email not verified. Please verify your email to continue.")
            }
            Error::Authorization(msg) => Self::permission_denied(msg),
            Error::InvalidInput(msg) => Self::invalid_argument(msg),
            Error::AlreadyExists(msg) => Self::already_exists(msg),
            Error::RateLimited(msg) => Self::resource_exhausted(msg),
            Error::ServiceUnavailable(msg) => Self::unavailable(msg),
            Error::OptimisticLockConflict => Self::aborted("Resource modified concurrently"),
            Error::LockConflict(msg) => Self::aborted(msg),
            Error::Timeout(msg) => Self::deadline_exceeded(msg),
            other => {
                tracing::error!("Internal error: {other}");
                Self::internal("Internal error")
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Extension trait for convenient error mapping to Internal
///
/// This trait provides convenient methods to map errors to `Error::Internal`
/// with context, reducing boilerplate across the codebase.
///
/// # Examples
///
/// Before:
/// ```text
/// serde_json::to_string(&data)
///     .map_err(|e| Error::Internal(format!("Failed to serialize: {e}")))?
/// ```
///
/// After:
/// ```text
/// serde_json::to_string(&data).internal("Failed to serialize")?
/// ```
pub trait InternalExt<T> {
    /// Map any error to `Error::Internal` with a static message
    fn internal(self, msg: &str) -> Result<T>;

    /// Map any error to `Error::Internal` with a formatted message that includes the original error
    fn internal_with_err(self, context: &str) -> Result<T>;
}

impl<T, E: std::fmt::Display> InternalExt<T> for std::result::Result<T, E> {
    fn internal(self, msg: &str) -> Result<T> {
        self.map_err(|_| Error::Internal(msg.to_string()))
    }

    fn internal_with_err(self, context: &str) -> Result<T> {
        self.map_err(|e| Error::Internal(format!("{context}: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a sqlx::Error from a DatabaseError with a specific code and optional constraint
    fn make_db_error(code: &str, message: &str) -> sqlx::Error {
        make_db_error_with_constraint(code, message, None)
    }

    fn make_db_error_with_constraint(
        code: &str,
        message: &str,
        constraint: Option<&str>,
    ) -> sqlx::Error {
        use std::borrow::Cow;

        #[derive(Debug)]
        struct FakeDbError {
            code: String,
            message: String,
            constraint: Option<String>,
        }

        impl std::fmt::Display for FakeDbError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.message)
            }
        }

        impl std::error::Error for FakeDbError {}

        impl sqlx::error::DatabaseError for FakeDbError {
            fn message(&self) -> &str {
                &self.message
            }
            fn code(&self) -> Option<Cow<'_, str>> {
                Some(Cow::Borrowed(&self.code))
            }
            fn constraint(&self) -> Option<&str> {
                self.constraint.as_deref()
            }
            fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
                self
            }
            fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
                self
            }
            fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
                self
            }
            fn kind(&self) -> sqlx::error::ErrorKind {
                sqlx::error::ErrorKind::Other
            }
        }

        sqlx::Error::Database(Box::new(FakeDbError {
            code: code.to_string(),
            message: message.to_string(),
            constraint: constraint.map(String::from),
        }))
    }

    #[test]
    fn test_sqlx_unique_violation_username() {
        let err = make_db_error_with_constraint(
            "23505",
            "duplicate key violates unique constraint",
            Some("users_username_key"),
        );
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::AlreadyExists(ref msg) if msg.contains("Username")));
    }

    #[test]
    fn test_sqlx_unique_violation_email() {
        let err = make_db_error_with_constraint(
            "23505",
            "duplicate key violates unique constraint",
            Some("users_email_key"),
        );
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::AlreadyExists(ref msg) if msg.contains("Email")));
    }

    #[test]
    fn test_sqlx_unique_violation_generic() {
        let err = make_db_error_with_constraint(
            "23505",
            "duplicate key violates unique constraint",
            Some("other_unique_key"),
        );
        let core_err: Error = err.into();
        assert!(
            matches!(core_err, Error::AlreadyExists(ref msg) if msg.contains("already exists"))
        );
    }

    #[test]
    fn test_sqlx_unique_violation_no_constraint_name() {
        // When PostgreSQL doesn't provide a constraint name, fall back to generic message
        let err = make_db_error("23505", "duplicate key violates unique constraint");
        let core_err: Error = err.into();
        assert!(
            matches!(core_err, Error::AlreadyExists(ref msg) if msg.contains("already exists"))
        );
    }

    #[test]
    fn test_sqlx_foreign_key_violation() {
        let err = make_db_error("23503", "insert or update violates foreign key constraint");
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::NotFound(ref msg) if msg.contains("Referenced")));
    }

    #[test]
    fn test_sqlx_check_violation() {
        let err = make_db_error("23514", "check constraint failed");
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::InvalidInput(ref msg) if msg.contains("Constraint")));
    }

    #[test]
    fn test_sqlx_not_null_violation() {
        let err = make_db_error("23502", "null value in column");
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::InvalidInput(ref msg) if msg.contains("Required")));
    }

    #[test]
    fn test_sqlx_row_not_found() {
        let err = sqlx::Error::RowNotFound;
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::NotFound(_)));
    }

    #[test]
    fn test_sqlx_unknown_db_error() {
        let err = make_db_error("42000", "syntax error");
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::Database(_)));
    }

    #[test]
    fn test_tonic_status_not_found() {
        let status: tonic::Status = Error::NotFound("test".to_string()).into();
        assert_eq!(status.code(), tonic::Code::NotFound);
    }

    #[test]
    fn test_tonic_status_authentication() {
        let status: tonic::Status = Error::Authentication("bad creds".to_string()).into();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);
    }

    #[test]
    fn test_tonic_status_authorization() {
        let status: tonic::Status = Error::Authorization("denied".to_string()).into();
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
    }

    #[test]
    fn test_tonic_status_invalid_input() {
        let status: tonic::Status = Error::InvalidInput("bad field".to_string()).into();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }

    #[test]
    fn test_tonic_status_already_exists() {
        let status: tonic::Status = Error::AlreadyExists("dup".to_string()).into();
        assert_eq!(status.code(), tonic::Code::AlreadyExists);
    }

    #[test]
    fn test_tonic_status_optimistic_lock() {
        let status: tonic::Status = Error::OptimisticLockConflict.into();
        assert_eq!(status.code(), tonic::Code::Aborted);
    }

    #[test]
    fn test_tonic_status_internal_errors() {
        let status: tonic::Status = Error::Internal("boom".to_string()).into();
        assert_eq!(status.code(), tonic::Code::Internal);

        let status: tonic::Status =
            Error::Serialization(serde_json::from_str::<serde_json::Value>("invalid").unwrap_err())
                .into();
        assert_eq!(status.code(), tonic::Code::Internal);
    }

    #[test]
    fn test_display_trait() {
        assert_eq!(
            Error::NotFound("room 123".to_string()).to_string(),
            "Not found: room 123"
        );
        assert_eq!(
            Error::AlreadyExists("user".to_string()).to_string(),
            "Already exists: user"
        );
        assert_eq!(
            Error::Authentication("expired".to_string()).to_string(),
            "Authentication error: expired"
        );
        assert_eq!(
            Error::Authorization("forbidden".to_string()).to_string(),
            "Authorization error: forbidden"
        );
        assert_eq!(
            Error::InvalidInput("bad".to_string()).to_string(),
            "Invalid input: bad"
        );
        assert_eq!(
            Error::Internal("oops".to_string()).to_string(),
            "Internal error: oops"
        );
        assert_eq!(
            Error::OptimisticLockConflict.to_string(),
            "Optimistic lock conflict"
        );
    }

    #[test]
    fn test_email_not_verified_display() {
        assert_eq!(Error::EmailNotVerified.to_string(), "Email not verified");
    }

    #[test]
    fn test_email_not_verified_to_tonic_status() {
        let status: tonic::Status = Error::EmailNotVerified.into();
        assert_eq!(
            status.code(),
            tonic::Code::PermissionDenied,
            "EmailNotVerified should map to PermissionDenied (403), not Unauthenticated (401)"
        );
        assert!(
            status.message().contains("verify your email"),
            "Error message should tell the user to verify their email"
        );
    }

    #[test]
    fn test_email_not_verified_is_distinct_from_authentication() {
        // EmailNotVerified must be distinguishable from generic Authentication errors
        let auth_err = Error::Authentication("Authentication failed".to_string());
        let email_err = Error::EmailNotVerified;

        let auth_status: tonic::Status = auth_err.into();
        let email_status: tonic::Status = email_err.into();

        // Different gRPC codes: Unauthenticated vs PermissionDenied
        assert_ne!(auth_status.code(), email_status.code());
    }

    #[test]
    fn test_provider_error_network_converts_to_timeout() {
        let provider_err =
            crate::provider::ProviderError::NetworkError("connection refused".to_string());
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::Timeout(_)));
        assert!(core_err.to_string().contains("network error"));
    }

    #[test]
    fn test_provider_error_auth_required_converts_to_authentication() {
        let provider_err = crate::provider::ProviderError::AuthRequired;
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::Authentication(_)));
    }

    #[test]
    fn test_provider_error_credential_required_converts_to_authentication() {
        let provider_err = crate::provider::ProviderError::CredentialRequired;
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::Authentication(_)));
    }

    #[test]
    fn test_provider_error_invalid_config_converts_to_invalid_input() {
        let provider_err =
            crate::provider::ProviderError::InvalidConfig("missing host".to_string());
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::InvalidInput(_)));
        assert!(core_err
            .to_string()
            .contains("Invalid provider configuration"));
    }

    #[test]
    fn test_provider_error_not_found_converts_to_not_found() {
        let provider_err = crate::provider::ProviderError::NotFound;
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::NotFound(_)));
    }

    #[test]
    fn test_provider_error_instance_not_found_converts_to_not_found() {
        let provider_err =
            crate::provider::ProviderError::InstanceNotFound("bilibili_main".to_string());
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::NotFound(_)));
    }

    #[test]
    fn test_provider_error_invalid_url_converts_to_invalid_input() {
        let provider_err = crate::provider::ProviderError::InvalidUrl("bad url".to_string());
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::InvalidInput(_)));
        assert!(core_err.to_string().contains("Invalid URL"));
    }

    #[test]
    fn test_provider_error_upstream_http_401_converts_to_authentication() {
        let provider_err = crate::provider::ProviderError::UpstreamHttp {
            status: 401,
            url: "https://api.example.com/video".to_string(),
        };
        let core_err: Error = provider_err.into();
        assert!(
            matches!(core_err, Error::Authentication(ref msg) if msg == "Provider authentication failed")
        );
    }

    #[test]
    fn test_provider_error_upstream_http_403_converts_to_authentication() {
        let provider_err = crate::provider::ProviderError::UpstreamHttp {
            status: 403,
            url: "https://api.example.com/video".to_string(),
        };
        let core_err: Error = provider_err.into();
        assert!(
            matches!(core_err, Error::Authentication(ref msg) if msg == "Provider authentication failed")
        );
    }

    #[test]
    fn test_provider_error_upstream_http_404_converts_to_not_found() {
        let provider_err = crate::provider::ProviderError::UpstreamHttp {
            status: 404,
            url: "https://api.example.com/video".to_string(),
        };
        let core_err: Error = provider_err.into();
        assert!(
            matches!(core_err, Error::NotFound(ref msg) if msg == "Provider resource not found")
        );
    }

    #[test]
    fn test_provider_error_upstream_http_500_converts_to_timeout() {
        let provider_err = crate::provider::ProviderError::UpstreamHttp {
            status: 500,
            url: "https://api.example.com/video".to_string(),
        };
        let core_err: Error = provider_err.into();
        // 5xx errors are treated as transient/timeout
        assert!(
            matches!(core_err, Error::Timeout(ref msg) if msg == "Upstream provider service is temporarily unavailable.")
        );
    }

    #[test]
    fn test_provider_error_upstream_http_408_converts_to_timeout() {
        let provider_err = crate::provider::ProviderError::UpstreamHttp {
            status: 408,
            url: "https://api.example.com/video?token=secret".to_string(),
        };
        let core_err: Error = provider_err.into();
        assert!(
            matches!(core_err, Error::Timeout(ref msg) if msg == "Upstream provider service is temporarily unavailable.")
        );
    }

    #[test]
    fn test_provider_error_upstream_http_429_converts_to_timeout() {
        let provider_err = crate::provider::ProviderError::UpstreamHttp {
            status: 429,
            url: "https://api.example.com/video?token=secret".to_string(),
        };
        let core_err: Error = provider_err.into();
        assert!(
            matches!(core_err, Error::Timeout(ref msg) if msg == "Upstream provider service is temporarily unavailable.")
        );
    }

    #[test]
    fn test_provider_error_upstream_http_400_converts_to_invalid_input() {
        let provider_err = crate::provider::ProviderError::UpstreamHttp {
            status: 400,
            url: "https://api.example.com/video".to_string(),
        };
        let core_err: Error = provider_err.into();
        assert!(
            matches!(core_err, Error::InvalidInput(ref msg) if msg == "Upstream provider rejected the request.")
        );
    }

    #[test]
    fn test_provider_error_encryption_required_converts_to_invalid_input() {
        let provider_err = crate::provider::ProviderError::EncryptionRequired("bilibili");
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::InvalidInput(_)));
        assert!(core_err
            .to_string()
            .contains("Credential encryption required"));
    }

    #[test]
    fn test_provider_error_api_error_converts_to_internal() {
        let provider_err = crate::provider::ProviderError::ApiError("rate limited".to_string());
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::Internal(_)));
    }

    #[test]
    fn test_provider_error_unsupported_format_converts_to_invalid_input() {
        let provider_err = crate::provider::ProviderError::UnsupportedFormat("avi".to_string());
        let core_err: Error = provider_err.into();
        assert!(matches!(core_err, Error::InvalidInput(_)));
    }

    #[test]
    fn test_provider_error_to_tonic_status_preserves_error_type() {
        // Network error -> Timeout -> should map to something appropriate
        let provider_err = crate::provider::ProviderError::NetworkError("timeout".to_string());
        let core_err: Error = provider_err.into();
        let status: tonic::Status = core_err.into();
        // Timeout errors map to DeadlineExceeded
        assert_eq!(status.code(), tonic::Code::DeadlineExceeded);
        // Auth error -> Authentication -> Unauthenticated
        let provider_err = crate::provider::ProviderError::AuthRequired;
        let core_err: Error = provider_err.into();
        let status: tonic::Status = core_err.into();
        assert_eq!(status.code(), tonic::Code::Unauthenticated);

        // Invalid config -> InvalidInput -> InvalidArgument
        let provider_err = crate::provider::ProviderError::InvalidConfig("bad".to_string());
        let core_err: Error = provider_err.into();
        let status: tonic::Status = core_err.into();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
    }
}
