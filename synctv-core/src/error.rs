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

    #[error("Authorization error: {0}")]
    Authorization(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Already exists: {0}")]
    AlreadyExists(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Internal error: {0}")]
    Internal(String),

    #[error("Optimistic lock conflict")]
    OptimisticLockConflict,
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
                        let detail = db_err.message().to_string();
                        if detail.contains("username") {
                            Self::AlreadyExists("Username already taken".to_string())
                        } else if detail.contains("email") {
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
        Self::Internal(err.to_string())
    }
}

impl From<Error> for tonic::Status {
    fn from(err: Error) -> Self {
        match err {
            Error::NotFound(msg) => Self::not_found(msg),
            Error::Authentication(msg) => Self::unauthenticated(msg),
            Error::Authorization(msg) => Self::permission_denied(msg),
            Error::InvalidInput(msg) => Self::invalid_argument(msg),
            Error::AlreadyExists(msg) => Self::already_exists(msg),
            Error::OptimisticLockConflict => Self::aborted("Resource modified concurrently"),
            other => {
                tracing::error!("Internal error: {other}");
                Self::internal("Internal error")
            }
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a sqlx::Error from a DatabaseError with a specific code
    fn make_db_error(code: &str, message: &str) -> sqlx::Error {
        use std::borrow::Cow;

        #[derive(Debug)]
        struct FakeDbError {
            code: String,
            message: String,
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
        }))
    }

    #[test]
    fn test_sqlx_unique_violation_username() {
        let err = make_db_error("23505", "duplicate key violates username constraint");
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::AlreadyExists(ref msg) if msg.contains("Username")));
    }

    #[test]
    fn test_sqlx_unique_violation_email() {
        let err = make_db_error("23505", "duplicate key violates email constraint");
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::AlreadyExists(ref msg) if msg.contains("Email")));
    }

    #[test]
    fn test_sqlx_unique_violation_generic() {
        let err = make_db_error("23505", "duplicate key violates unique constraint");
        let core_err: Error = err.into();
        assert!(matches!(core_err, Error::AlreadyExists(ref msg) if msg.contains("already exists")));
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

        let status: tonic::Status = Error::Serialization(
            serde_json::from_str::<serde_json::Value>("invalid").unwrap_err()
        ).into();
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
}
