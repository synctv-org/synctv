//! Alist provider tests
//!
//! Tests for path validation, client creation, and HTTP API interactions using wiremock.

use synctv_media_providers::AlistClient;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// === Path validation tests ===
// validate_path is a private function, but we test it indirectly through the public API
// (fs_get, fs_list, etc.) which call validate_path internally.

#[tokio::test]
async fn test_validate_path_url_encoded_dotdot_rejected() {
    // "%2e%2e" decodes to ".." which should be rejected
    let client = AlistClient::with_token("https://alist.example.com", "token123").unwrap();
    let result = client.fs_get("/movies/%2e%2e/etc/passwd", None).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("traversal"),
        "Expected traversal error, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_validate_path_url_encoded_slash_dotdot_rejected() {
    // "%2F%2e%2e" decodes to "/.." which should be rejected
    let client = AlistClient::with_token("https://alist.example.com", "token123").unwrap();
    let result = client.fs_get("/movies%2F%2e%2e/secret", None).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("traversal"),
        "Expected traversal error, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_validate_path_normal_paths_accepted() {
    // Normal paths should not trigger validation errors (they will fail on network instead)
    // We use wiremock to verify the request goes through path validation
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": "success",
                "data": {
                    "name": "video.mp4",
                    "size": 1024,
                    "is_dir": false,
                    "modified": 0,
                    "created": 0,
                    "raw_url": "https://cdn.example.com/video.mp4",
                    "provider": "local",
                    "related": []
                }
            })),
        )
        .mount(&server)
        .await;

    let client = AlistClient::with_token(&server.uri(), "token123").unwrap();
    let result = client.fs_get("/movies/video.mp4", None).await;
    assert!(result.is_ok(), "Normal path should be accepted: {result:?}");
}

#[tokio::test]
async fn test_validate_path_dotfiles_accepted() {
    // Dotfiles like ".hidden" should be allowed (only ".." and "." are rejected)
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": "success",
                "data": {
                    "name": ".hidden",
                    "size": 0,
                    "is_dir": true,
                    "modified": 0,
                    "created": 0,
                    "raw_url": "",
                    "provider": "local",
                    "related": []
                }
            })),
        )
        .mount(&server)
        .await;

    let client = AlistClient::with_token(&server.uri(), "token123").unwrap();
    let result = client.fs_get("/movies/.hidden", None).await;
    assert!(result.is_ok(), "Dotfile path should be accepted: {result:?}");
}

// === Wiremock HTTP API tests ===

#[tokio::test]
async fn test_alist_client_login_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": "success",
                "data": {"token": "test-jwt-token-abc123"}
            })),
        )
        .mount(&server)
        .await;

    let mut client = AlistClient::new(&server.uri()).unwrap();
    let token = client.login("admin", "password123", false).await.unwrap();
    assert_eq!(token, "test-jwt-token-abc123");
    assert!(client.has_token());
}

#[tokio::test]
async fn test_alist_client_login_wrong_password() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 400,
                "message": "wrong password",
                "data": null
            })),
        )
        .mount(&server)
        .await;

    let mut client = AlistClient::new(&server.uri()).unwrap();
    let result = client.login("admin", "wrong_password", false).await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("wrong password") || err_msg.contains("400"),
        "Expected wrong password error, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_alist_client_fs_get_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": "success",
                "data": {
                    "name": "movie.mkv",
                    "size": 2_000_000_000_u64,
                    "is_dir": false,
                    "modified": 1700000000,
                    "created": 1699000000,
                    "sign": "sig123",
                    "thumb": "",
                    "type": 6,
                    "raw_url": "https://cdn.example.com/movie.mkv",
                    "provider": "s3",
                    "related": []
                }
            })),
        )
        .mount(&server)
        .await;

    let client = AlistClient::with_token(&server.uri(), "token123").unwrap();
    let resp = client.fs_get("/movies/movie.mkv", None).await.unwrap();
    assert_eq!(resp.name, "movie.mkv");
    assert_eq!(resp.size, 2_000_000_000);
    assert!(!resp.is_dir);
    assert_eq!(resp.raw_url, "https://cdn.example.com/movie.mkv");
}

#[tokio::test]
async fn test_alist_client_fs_list_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/list"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": "success",
                "data": {
                    "content": [
                        {"name": "file1.mp4", "size": 100, "is_dir": false, "modified": 0, "sign": "", "thumb": "", "type": 2},
                        {"name": "folder1", "size": 0, "is_dir": true, "modified": 0, "sign": "", "thumb": "", "type": 1}
                    ],
                    "total": 2,
                    "readme": "",
                    "write": false,
                    "provider": "local"
                }
            })),
        )
        .mount(&server)
        .await;

    let client = AlistClient::with_token(&server.uri(), "token123").unwrap();
    let resp = client.fs_list("/movies", 1, 20, None).await.unwrap();
    assert_eq!(resp.total, 2);
    assert_eq!(resp.content.len(), 2);
    assert_eq!(resp.content[0].name, "file1.mp4");
    assert!(!resp.content[0].is_dir);
    assert!(resp.content[1].is_dir);
}

#[tokio::test]
async fn test_alist_client_5xx_retries() {
    let server = MockServer::start().await;

    // First call returns 500, second returns success.
    // with_retry will retry on 5xx errors.
    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "code": 200,
                "message": "success",
                "data": {
                    "name": "video.mp4",
                    "size": 1024,
                    "is_dir": false,
                    "modified": 0,
                    "created": 0,
                    "raw_url": "https://cdn.example.com/video.mp4",
                    "provider": "local",
                    "related": []
                }
            })),
        )
        .mount(&server)
        .await;

    let client = AlistClient::with_token(&server.uri(), "token123").unwrap();
    let result = client.fs_get("/movies/video.mp4", None).await;
    assert!(
        result.is_ok(),
        "Should succeed after retry: {result:?}"
    );
    assert_eq!(result.unwrap().name, "video.mp4");
}
