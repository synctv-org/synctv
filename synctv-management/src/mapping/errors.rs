use synctv_adapter::error::{classified_error_to_tonic_status, ClassifiedError, ErrorKind};
use tonic::Status;

pub(crate) fn map_api_error(error: &impl ClassifiedError) -> Status {
    if error.classify() == ErrorKind::Internal {
        tracing::error!(
            "Management API adapter operation failed: {}",
            error.message()
        );
    }

    classified_error_to_tonic_status(error)
}

pub(crate) fn map_api_result<T, E>(result: Result<T, E>) -> Result<T, Status>
where
    E: ClassifiedError,
{
    result.map_err(|error| map_api_error(&error))
}

pub(crate) fn map_classified_result<T, E>(result: Result<T, E>) -> Result<T, Status>
where
    E: ClassifiedError,
{
    result.map_err(|error| map_api_error(&error))
}

pub(crate) fn map_management_user_lookup_error(err: synctv_core::Error) -> Status {
    match err {
        synctv_core::Error::NotFound(_) => Status::not_found("User not found"),
        synctv_core::Error::InvalidInput(message) => Status::invalid_argument(message),
        synctv_core::Error::Authentication(message) => Status::unauthenticated(message),
        synctv_core::Error::Authorization(message) => Status::permission_denied(message),
        synctv_core::Error::KickCooldownDenied => {
            Status::permission_denied(synctv_core::Error::kick_cooldown_denied_message())
        }
        synctv_core::Error::AlreadyExists(message) => Status::already_exists(message),
        synctv_core::Error::RateLimited(message) => Status::resource_exhausted(message),
        synctv_core::Error::ServiceUnavailable(message) => Status::unavailable(message),
        synctv_core::Error::Timeout(message) => Status::deadline_exceeded(message),
        synctv_core::Error::OptimisticLockConflict => {
            Status::aborted("management actor user was modified concurrently")
        }
        synctv_core::Error::LockConflict(message) => Status::aborted(message),
        other => {
            tracing::error!("Management user lookup failed: {other}");
            Status::internal("Internal error")
        }
    }
}

pub(crate) fn map_core_error(error: synctv_core::Error) -> Status {
    match error {
        synctv_core::Error::NotFound(message) => Status::not_found(message),
        synctv_core::Error::InvalidInput(message) => Status::invalid_argument(message),
        synctv_core::Error::Authentication(message) => Status::unauthenticated(message),
        synctv_core::Error::Authorization(message) => Status::permission_denied(message),
        synctv_core::Error::KickCooldownDenied => {
            Status::permission_denied(synctv_core::Error::kick_cooldown_denied_message())
        }
        synctv_core::Error::AlreadyExists(message) => Status::already_exists(message),
        synctv_core::Error::Conflict(message) | synctv_core::Error::LockConflict(message) => {
            Status::aborted(message)
        }
        synctv_core::Error::RateLimited(message) => Status::resource_exhausted(message),
        synctv_core::Error::ServiceUnavailable(message) => Status::unavailable(message),
        synctv_core::Error::Timeout(message) => Status::deadline_exceeded(message),
        synctv_core::Error::OptimisticLockConflict => {
            Status::aborted("resource was modified concurrently")
        }
        synctv_core::Error::RangeNotSatisfiable { total_size } => {
            Status::out_of_range(format!("range not satisfiable: total size {total_size}"))
        }
        other => {
            tracing::error!("Management core operation failed: {other}");
            Status::internal("Internal error")
        }
    }
}
