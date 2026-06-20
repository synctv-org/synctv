use synctv_core::models::{UserAuthFactors, UserId, UserPreferences, UserRole};
use synctv_core::service::UserService;

use super::{user_notification_preferences_to_proto, usize_to_i32_api, ApiError};

pub(in crate::impls::admin) struct BatchResultsAccumulator {
    results: Vec<synctv_proto::admin::BatchResultItem>,
    succeeded: i32,
    failed: i32,
}

impl BatchResultsAccumulator {
    pub fn new(capacity: usize) -> Self {
        Self {
            results: Vec::with_capacity(capacity),
            succeeded: 0,
            failed: 0,
        }
    }

    pub fn record_ok(&mut self, id: String) {
        self.results.push(synctv_proto::admin::BatchResultItem {
            id,
            success: true,
            error: String::new(),
        });
        self.succeeded += 1;
    }

    pub fn record_err(&mut self, id: String, error: impl Into<ApiError>) {
        self.results.push(synctv_proto::admin::BatchResultItem {
            id,
            success: false,
            error: map_batch_result_error(error),
        });
        self.failed += 1;
    }

    pub fn into_parts(self) -> (Vec<synctv_proto::admin::BatchResultItem>, i32, i32) {
        (self.results, self.succeeded, self.failed)
    }
}

pub(in crate::impls::admin) fn map_batch_result_error(error: impl Into<ApiError>) -> String {
    let error = error.into();
    match error.classify() {
        crate::impls::ErrorKind::Internal => {
            "Operation failed due to an internal error".to_string()
        }
        crate::impls::ErrorKind::ServiceUnavailable => {
            "Operation failed because the service is temporarily unavailable".to_string()
        }
        _ => error.message().to_string(),
    }
}

pub(in crate::impls::admin) fn auth_factors_to_proto(
    factors: &UserAuthFactors,
) -> Result<synctv_proto::client::UserAuthFactors, ApiError> {
    Ok(synctv_proto::client::UserAuthFactors {
        password: factors.password,
        webauthn: factors.webauthn,
        email: factors.email,
        eligible_count: usize_to_i32_api(factors.eligible_count(), "auth factor count")?,
    })
}

pub(in crate::impls::admin) fn user_preferences_to_proto(
    preferences: &UserPreferences,
) -> Result<synctv_proto::client::UserPreferences, ApiError> {
    Ok(synctv_proto::client::UserPreferences {
        two_factor_enabled: preferences.two_factor_enabled,
        notifications: Some(user_notification_preferences_to_proto(
            &preferences.notifications,
        )),
        settings: serde_json::to_vec(&preferences.settings).map_err(|error| {
            ApiError::Internal(format!("Failed to serialize settings: {error}"))
        })?,
    })
}

pub(in crate::impls::admin) fn live_streaming_unavailable_error() -> ApiError {
    ApiError::ServiceUnavailable("Live streaming is not available on this server.".to_string())
}

pub(in crate::impls::admin) fn parse_batch_user_ids(
    user_ids: &[String],
    public_id_codec: &synctv_core::PublicIdCodec,
) -> Result<Vec<UserId>, ApiError> {
    if user_ids.is_empty() {
        return Err(ApiError::InvalidInput(
            "user_ids cannot be empty".to_string(),
        ));
    }
    if user_ids.len() > UserService::BATCH_SIZE_LIMIT {
        return Err(ApiError::InvalidInput(format!(
            "Batch size {} exceeds limit of {}",
            user_ids.len(),
            UserService::BATCH_SIZE_LIMIT
        )));
    }

    user_ids
        .iter()
        .map(|user_id| crate::impls::parse_user_id_param(user_id, "user_ids", public_id_codec))
        .collect()
}

pub(in crate::impls::admin) fn normalize_non_empty_filter(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed.to_string())
}

pub(in crate::impls::admin) fn check_role_hierarchy(
    caller_role: UserRole,
    target_role: UserRole,
    action: &str,
) -> Result<(), ApiError> {
    if target_role == UserRole::Root && caller_role != UserRole::Root {
        return Err(ApiError::Authorization(format!(
            "Only root users can {action} root users"
        )));
    }
    if target_role == UserRole::Admin && caller_role != UserRole::Root {
        return Err(ApiError::Authorization(format!(
            "Only root users can {action} admin users"
        )));
    }
    Ok(())
}
