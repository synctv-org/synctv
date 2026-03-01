//! Tests for `StreamRelayServiceImpl` authentication logic.
//!
//! These tests verify the `authenticate()` method behavior with
//! different cluster secret configurations.

#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use synctv_livestream::relay::InMemoryStreamRegistry;
use tokio_util::sync::CancellationToken;
use tonic::metadata::MetadataValue;
use tonic::Request;

/// Helper to create a `StreamRelayServiceImpl` for testing.
/// Uses `InMemoryStreamRegistry` to avoid Redis dependency.
fn create_test_service() -> synctv_livestream::grpc::StreamRelayServiceImpl {
    let registry = Arc::new(InMemoryStreamRegistry::new());
    let (event_sender, _rx) = tokio::sync::mpsc::channel(64);
    let cancel_token = CancellationToken::new();

    synctv_livestream::grpc::StreamRelayServiceImpl::new(
        registry,
        "test-node".to_string(),
        event_sender,
        cancel_token,
    )
}

#[tokio::test]
async fn test_authenticate_no_secret_allows_all() {
    let service = create_test_service();
    // No cluster_secret configured, so authentication should pass for any request
    let request: Request<()> = Request::new(());
    let result = service.authenticate(&request);
    assert!(
        result.is_ok(),
        "No secret configured should allow all requests"
    );
}

#[tokio::test]
async fn test_authenticate_matching_secret_passes() {
    let service = create_test_service().with_cluster_secret("my-secret-key");

    let mut request: Request<()> = Request::new(());
    request.metadata_mut().insert(
        "x-cluster-secret",
        MetadataValue::from_static("my-secret-key"),
    );

    let result = service.authenticate(&request);
    assert!(result.is_ok(), "Matching secret should pass authentication");
}

#[tokio::test]
async fn test_authenticate_wrong_secret_rejected() {
    let service = create_test_service().with_cluster_secret("correct-secret");

    let mut request: Request<()> = Request::new(());
    request.metadata_mut().insert(
        "x-cluster-secret",
        MetadataValue::from_static("wrong-secret"),
    );

    let result = service.authenticate(&request);
    assert!(result.is_err(), "Wrong secret should be rejected");

    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn test_authenticate_missing_secret_rejected() {
    let service = create_test_service().with_cluster_secret("my-secret");

    // Request with no metadata
    let request: Request<()> = Request::new(());

    let result = service.authenticate(&request);
    assert!(
        result.is_err(),
        "Missing secret should be rejected when one is configured"
    );

    let status = result.unwrap_err();
    assert_eq!(status.code(), tonic::Code::Unauthenticated);
}

#[tokio::test]
async fn test_authenticate_empty_secret_rejected() {
    let service = create_test_service().with_cluster_secret("non-empty-secret");

    let mut request: Request<()> = Request::new(());
    request
        .metadata_mut()
        .insert("x-cluster-secret", MetadataValue::from_static(""));

    let result = service.authenticate(&request);
    assert!(
        result.is_err(),
        "Empty secret should not match non-empty expected secret"
    );
}
