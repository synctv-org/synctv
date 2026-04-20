//! gRPC transport helper tests for synctv-api.
//!
//! The production gRPC stack now performs auth, blacklist, rate limiting, and
//! timeout handling explicitly in impls. The transport layer retains only the
//! cluster shared-secret guard, which is covered here.

#![allow(clippy::unwrap_used)]

#[test]
fn test_cluster_auth_interceptor_correct_secret() {
    use synctv_api::grpc::ClusterAuthInterceptor;

    let interceptor = ClusterAuthInterceptor::new("my-cluster-secret".to_string());
    let mut request = tonic::Request::new(());
    request
        .metadata_mut()
        .insert("x-cluster-secret", "my-cluster-secret".parse().unwrap());

    let result = interceptor.validate(request);
    assert!(result.is_ok(), "Correct secret should pass validation");
}

#[test]
fn test_cluster_auth_interceptor_wrong_secret() {
    use synctv_api::grpc::ClusterAuthInterceptor;

    let interceptor = ClusterAuthInterceptor::new("my-cluster-secret".to_string());
    let mut request = tonic::Request::new(());
    request
        .metadata_mut()
        .insert("x-cluster-secret", "wrong-secret".parse().unwrap());

    let result = interceptor.validate(request);
    assert!(result.is_err(), "Wrong secret should fail validation");
    assert_eq!(
        result.unwrap_err().code(),
        tonic::Code::Unauthenticated,
        "Wrong secret should return UNAUTHENTICATED"
    );
}

#[test]
fn test_cluster_auth_interceptor_missing_header() {
    use synctv_api::grpc::ClusterAuthInterceptor;

    let interceptor = ClusterAuthInterceptor::new("my-cluster-secret".to_string());
    let request = tonic::Request::new(());

    let result = interceptor.validate(request);
    assert!(result.is_err(), "Missing header should fail validation");
    assert_eq!(
        result.unwrap_err().code(),
        tonic::Code::Unauthenticated,
        "Missing header should return UNAUTHENTICATED"
    );
}

#[test]
fn test_logout_blacklist_failure_maps_to_grpc_internal() {
    let api_err = synctv_api::impls::ApiError::Internal("Blacklist store unavailable".to_string());
    let proto_err = api_err.to_proto_error();
    assert_eq!(
        proto_err.code,
        synctv_api::impls::ErrorKind::Internal.to_code()
    );
    assert_eq!(proto_err.message, "Internal error");
}
