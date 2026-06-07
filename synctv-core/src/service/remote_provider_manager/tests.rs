use super::*;
use crate::cache::CacheInvalidationService;
use crate::models::ProviderInstance;
use chrono::Utc;

fn remote_instance(endpoint: &str) -> ProviderInstance {
    ProviderInstance {
        name: "remote".to_string(),
        endpoint: endpoint.to_string(),
        comment: None,
        jwt_secret: Some("remote-provider-test-secret".to_string()),
        custom_ca: None,
        timeout: "5s".to_string(),
        tls: false,
        insecure_tls: false,
        providers: vec!["alist".to_string()],
        enabled: true,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[tokio::test(start_paused = true)]
async fn start_invalidation_listener_does_not_wait_for_synthetic_readiness() {
    let invalidation = Arc::new(CacheInvalidationService::new(
        "test-node".to_string(),
        "test:provider:invalidate".to_string(),
    ));
    let manager =
        RemoteProviderManager::new_with_store(empty_provider_instance_store(), Some(invalidation));

    let start = tokio::time::Instant::now();
    manager
        .start_invalidation_listener()
        .await
        .expect("listener should start");

    assert_eq!(
        tokio::time::Instant::now().duration_since(start),
        Duration::ZERO,
        "listener startup should not advance time via a synthetic readiness sleep"
    );

    manager.shutdown().await;
}

#[test]
fn validate_config_accepts_http_endpoint_scheme() {
    let config = remote_instance("http://provider.example.com:50051");

    RemoteProviderManager::validate_config(&config).expect("http:// endpoint should remain valid");
}

#[test]
fn validate_config_rejects_invalid_provider_instance_name() {
    let mut config = remote_instance("http://provider.example.com:50051");
    config.name = "bad name".to_string();

    let err = RemoteProviderManager::validate_config(&config)
        .expect_err("provider instance names must match the core naming contract");

    assert!(
        err.to_string().contains("provider instance name"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_config_rejects_unsupported_provider_type() {
    let mut config = remote_instance("http://provider.example.com:50051");
    config.providers = vec!["custom_local".to_string()];

    let err = RemoteProviderManager::validate_config(&config)
        .expect_err("unsupported remote provider types must be rejected");

    assert!(
        err.to_string().contains("unsupported provider"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_config_rejects_http_endpoint_with_tls_enabled() {
    let mut config = remote_instance("http://provider.example.com:50051");
    config.tls = true;

    let err = RemoteProviderManager::validate_config(&config)
        .expect_err("plaintext http endpoints must require tls=false");

    assert!(
        err.to_string().contains("tls=false"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_config_rejects_https_endpoint_without_tls() {
    let config = remote_instance("https://provider.example.com:50051");

    let err = RemoteProviderManager::validate_config(&config)
        .expect_err("https endpoints must require tls=true");

    assert!(
        err.to_string().contains("tls=true"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_config_rejects_custom_ca_without_tls() {
    let mut config = remote_instance("http://provider.example.com:50051");
    config.custom_ca =
        Some("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----".to_string());

    let err = RemoteProviderManager::validate_config(&config)
        .expect_err("custom CA must not be accepted for plaintext endpoints");

    assert!(
        err.to_string().contains("custom_ca requires tls=true"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_config_rejects_insecure_tls_with_custom_ca() {
    let mut config = remote_instance("https://provider.example.com:50051");
    config.tls = true;
    config.insecure_tls = true;
    config.custom_ca =
        Some("-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----".to_string());

    let err = RemoteProviderManager::validate_config(&config)
        .expect_err("custom CA and insecure TLS express conflicting trust policies");

    assert!(
        err.to_string().contains("insecure_tls cannot be combined"),
        "unexpected error: {err}"
    );
}

#[test]
fn validate_config_accepts_https_endpoint_with_tls() {
    let mut config = remote_instance("https://provider.example.com:50051");
    config.tls = true;

    RemoteProviderManager::validate_config(&config)
        .expect("https endpoint with tls=true should pass validation");
}

#[test]
fn normalized_transport_endpoint_preserves_http() {
    let config = remote_instance("http://provider.example.com:50051");

    let normalized = RemoteProviderManager::normalized_transport_endpoint(&config)
        .expect("http:// endpoint should normalize to a tonic transport URL");

    assert_eq!(normalized, "http://provider.example.com:50051");
}

#[test]
fn map_remote_resolution_error_hides_database_details() {
    let err = RemoteProviderManager::map_remote_resolution_error(crate::Error::Database(
        sqlx::Error::PoolTimedOut,
    ));

    assert!(matches!(
        err,
        ProviderError::ApiError(ref message)
            if message == "Provider configuration service is temporarily unavailable."
    ));
}

#[test]
fn map_remote_resolution_error_hides_redis_details() {
    let err = RemoteProviderManager::map_remote_resolution_error(crate::Error::Redis(
        redis::RedisError::from((redis::ErrorKind::Io, "connection reset by peer")),
    ));

    assert!(matches!(
        err,
        ProviderError::ApiError(ref message)
            if message == "Provider configuration service is temporarily unavailable."
    ));
}

#[test]
fn provider_connection_setup_error_hides_invalid_endpoint_details() {
    let err = RemoteProviderManager::provider_connection_setup_error(
        "Remote provider endpoint configuration is invalid.",
        "relative URL without a base",
    );

    assert!(matches!(
        err,
        crate::Error::Internal(ref message)
            if message == "Remote provider endpoint configuration is invalid."
    ));
}

#[test]
fn provider_connection_setup_error_hides_tls_connect_details() {
    let err = RemoteProviderManager::provider_connection_setup_error(
        "Remote provider TLS connection setup failed.",
        "certificate verify failed",
    );

    assert!(matches!(
        err,
        crate::Error::Internal(ref message)
            if message == "Remote provider TLS connection setup failed."
    ));
}

#[test]
fn probe_execution_control_preserves_tighter_parent_deadline_and_cancellation() {
    let cancellation = tokio_util::sync::CancellationToken::new();
    let parent_deadline = std::time::Instant::now() + Duration::from_secs(1);
    let parent = ExecutionControl::from_parts(Some(parent_deadline), cancellation.clone());

    let probe =
        RemoteProviderManager::probe_execution_control(Some(&parent), Duration::from_secs(5));

    assert_eq!(probe.deadline(), Some(parent_deadline));
    cancellation.cancel();
    assert!(matches!(
        probe.check_active(),
        Err(synctv_common::ExecutionControlError::Cancelled)
    ));
}

#[test]
fn probe_execution_control_applies_probe_timeout_without_parent_control() {
    let probe = RemoteProviderManager::probe_execution_control(None, Duration::from_secs(5));

    let remaining = probe
        .remaining_timeout()
        .expect("probe without parent control should still have a deadline");
    assert!(remaining <= Duration::from_secs(5));
    assert!(remaining > Duration::ZERO);
}
