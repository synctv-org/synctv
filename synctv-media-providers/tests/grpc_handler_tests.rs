//! gRPC server handler tests
//!
//! Tests for gRPC service validation layers (input validation before dispatching to HTTP clients).

#![allow(clippy::unwrap_used)]
use synctv_media_providers::grpc::alist::alist_server::Alist;
use synctv_media_providers::grpc::alist::{
    login_req, FsGetReq, FsListReq, FsOtherReq, FsSearchReq, LoginReq, MeReq,
};
use synctv_media_providers::grpc::AlistService;
use tonic::{Code, Request};

#[tokio::test]
async fn test_alist_grpc_login_rejects_missing_username() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let status = service
        .login(Request::new(LoginReq {
            host: "http://127.0.0.1:5244".to_string(),
            username: " ".to_string(),
            credential: Some(login_req::Credential::Password("secret".to_string())),
            otp_code: String::new(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("username"));
}

#[tokio::test]
async fn test_alist_grpc_login_rejects_missing_credential() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let status = service
        .login(Request::new(LoginReq {
            host: "http://127.0.0.1:5244".to_string(),
            username: "admin".to_string(),
            credential: None,
            otp_code: String::new(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("password"));
}

#[tokio::test]
async fn test_alist_grpc_me_rejects_missing_token_before_io() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let status = service
        .me(Request::new(MeReq {
            host: "http://127.0.0.1:5244".to_string(),
            token: String::new(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("token"));
}

#[tokio::test]
async fn test_alist_grpc_fs_get_rejects_missing_token_before_io() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let status = service
        .fs_get(Request::new(FsGetReq {
            host: "http://127.0.0.1:5244".to_string(),
            token: String::new(),
            path: "/local/video.mp4".to_string(),
            password: String::new(),
            headers: std::collections::HashMap::new(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("token"));
}

#[tokio::test]
async fn test_alist_grpc_fs_list_rejects_missing_token_before_io() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let status = service
        .fs_list(Request::new(FsListReq {
            host: "http://127.0.0.1:5244".to_string(),
            token: String::new(),
            path: "/local".to_string(),
            password: String::new(),
            page: 1,
            per_page: 20,
            refresh: false,
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("token"));
}

#[tokio::test]
async fn test_alist_grpc_fs_other_rejects_missing_method_before_io() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let status = service
        .fs_other(Request::new(FsOtherReq {
            host: "http://127.0.0.1:5244".to_string(),
            token: "token".to_string(),
            path: "/local/video.mp4".to_string(),
            method: String::new(),
            password: String::new(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("method"));
}

#[tokio::test]
async fn test_alist_grpc_fs_search_rejects_missing_token_before_io() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let status = service
        .fs_search(Request::new(FsSearchReq {
            host: "http://127.0.0.1:5244".to_string(),
            token: String::new(),
            parent: "/local".to_string(),
            keywords: "video".to_string(),
            scope: 1,
            page: 1,
            per_page: 20,
            password: String::new(),
        }))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::InvalidArgument);
    assert!(status.message().contains("token"));
}
