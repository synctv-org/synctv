use serde_json::Value;

use crate::impls::ApiError;

pub(crate) fn validate_passkey_session_id(session_id: &str) -> Result<(), ApiError> {
    if session_id.is_empty() {
        return Err(ApiError::InvalidInput(
            "session_id must not be empty".to_string(),
        ));
    }
    if session_id.len() > 128 {
        return Err(ApiError::InvalidInput(
            "session_id must be at most 128 characters".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn passkey_options_to_value(options_json: &[u8]) -> Result<Value, ApiError> {
    serde_json::from_slice(options_json)
        .map_err(|error| ApiError::Internal(format!("Invalid passkey challenge JSON: {error}")))
}

pub(crate) fn passkey_credential_to_json_bytes(credential: &Value) -> Result<Vec<u8>, ApiError> {
    let bytes = serde_json::to_vec(credential)
        .map_err(|error| ApiError::InvalidInput(format!("Invalid passkey credential: {error}")))?;
    if bytes.len() > 65_536 {
        return Err(ApiError::InvalidInput(
            "credential JSON must be at most 65536 bytes".to_string(),
        ));
    }
    Ok(bytes)
}
