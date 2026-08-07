use super::RemoteProviderConnection;
use crate::ProviderClientError;
use std::time::Duration;
use synctv_common::ExecutionControl;
use tonic::metadata::MetadataValue as TransportMetadataValue;
use tonic::Request as TransportRequest;
use tonic_health::pb::{health_client::HealthClient, HealthCheckRequest};

pub(crate) fn build_remote_request<T>(
    auth_secret: Option<&str>,
    payload: T,
) -> Result<TransportRequest<T>, ProviderClientError> {
    let mut request = TransportRequest::new(payload);
    let Some(auth_secret) = auth_secret
        .map(str::trim)
        .filter(|secret| !secret.is_empty())
    else {
        return Ok(request);
    };

    let metadata_value = auth_secret.parse().map_err(|e| {
        ProviderClientError::InvalidHeader(format!("invalid x-provider-secret metadata value: {e}"))
    })?;

    request
        .metadata_mut()
        .insert("x-provider-secret", metadata_value);
    Ok(request)
}

fn build_health_check_request(
    instance_name: &str,
    auth_secret: Option<&str>,
) -> Result<TransportRequest<HealthCheckRequest>, ProviderClientError> {
    let mut request = TransportRequest::new(HealthCheckRequest {
        service: String::new(),
    });
    let secret = auth_secret.ok_or_else(|| {
        ProviderClientError::InvalidConfig(format!(
            "Remote provider instance '{instance_name}' requires a non-empty jwt_secret for health checks"
        ))
    })?;
    let metadata_value = secret.parse().map_err(|e| {
        ProviderClientError::InvalidConfig(format!(
            "Remote provider instance '{instance_name}' jwt_secret must be valid ASCII remote transport metadata: {e}"
        ))
    })?;
    request
        .metadata_mut()
        .insert("x-provider-secret", metadata_value);
    Ok(request)
}

pub async fn execute_health_check(
    instance_name: &str,
    connection: &RemoteProviderConnection,
    control: &ExecutionControl,
    timeout: Duration,
) -> Result<i32, ProviderClientError> {
    let mut client = connection.build_provider_client(HealthClient::new);
    let request = build_health_check_request(instance_name, connection.auth_secret())?;

    let response = control
        .run(client.check(request))
        .await
        .map_err(|err| match err {
            synctv_common::ExecutionControlError::DeadlineExceeded => {
                ProviderClientError::Network(format!(
                    "Remote provider instance '{instance_name}' connectivity validation timed out after {}s",
                    timeout.as_secs()
                ))
            }
            other => ProviderClientError::Network(other.to_string()),
        })?
        .map_err(|status| {
            ProviderClientError::Network(format!(
                "Remote provider instance '{instance_name}' health check failed: {status}"
            ))
        })?;

    Ok(response.into_inner().status)
}

pub fn validate_auth_secret(
    auth_secret: Option<&str>,
) -> Result<Option<&str>, ProviderClientError> {
    match auth_secret.map(str::trim) {
        Some("") => Err(ProviderClientError::InvalidConfig(
            "remote provider auth secret must not be empty".to_string(),
        )),
        Some(secret) => {
            if !secret.is_ascii() {
                return Err(ProviderClientError::InvalidConfig(
                    "remote provider auth secret must be valid ASCII remote transport metadata"
                        .to_string(),
                ));
            }
            TransportMetadataValue::try_from(secret).map_err(|_| {
                ProviderClientError::InvalidConfig(
                    "remote provider auth secret must be valid ASCII remote transport metadata"
                        .to_string(),
                )
            })?;
            Ok(Some(secret))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_remote_request_inserts_x_provider_secret() {
        let request =
            build_remote_request(Some("shared-secret"), 42_u32).expect("request should build");
        assert_eq!(request.get_ref(), &42_u32);
        assert_eq!(
            request.metadata().get("x-provider-secret"),
            Some(&TransportMetadataValue::from_static("shared-secret"))
        );
    }

    #[test]
    fn test_build_remote_request_omits_header_when_secret_is_blank() {
        let request = build_remote_request(Some("   "), 42_u32).expect("request should build");
        assert_eq!(request.get_ref(), &42_u32);
        assert!(
            request.metadata().get("x-provider-secret").is_none(),
            "blank secrets must not produce a malformed header"
        );
    }

    #[test]
    fn test_validate_auth_secret_rejects_empty_secret() {
        let error = validate_auth_secret(Some("   ")).expect_err("empty secret must fail");
        assert!(matches!(
            error,
            ProviderClientError::InvalidConfig(message)
                if message.contains("auth secret must not be empty")
        ));
    }

    #[test]
    fn test_validate_auth_secret_allows_absent_secret_only_for_non_remote_callers() {
        assert_eq!(
            validate_auth_secret(None).expect("operation should succeed"),
            None
        );
        assert_eq!(
            validate_auth_secret(Some("  shared-secret  ")).expect("operation should succeed"),
            Some("shared-secret")
        );
    }

    #[test]
    fn test_validate_auth_secret_rejects_non_ascii_secret() {
        let error = validate_auth_secret(Some("sëcret")).expect_err("non-ASCII secret must fail");
        assert!(matches!(
            error,
            ProviderClientError::InvalidConfig(message)
                if message.contains("valid ASCII remote transport metadata")
        ));
    }

    #[test]
    fn test_validate_auth_secret_rejects_control_characters() {
        let error =
            validate_auth_secret(Some("shared\nsecret")).expect_err("control chars must fail");
        assert!(matches!(
            error,
            ProviderClientError::InvalidConfig(message)
                if message.contains("valid ASCII remote transport metadata")
        ));
    }
}
