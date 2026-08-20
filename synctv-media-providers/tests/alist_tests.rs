//! Alist provider tests
//!
//! Tests for path validation, client creation, and HTTP API interactions using wiremock.

#![allow(clippy::unwrap_used)]
use std::collections::HashMap;

use serde_json::json;
use synctv_core_testing::{
    start_external_service, ExternalServiceContainer, ExternalServiceRequest,
};
use synctv_media_providers::alist::{AlistInterface, AlistService};
use synctv_media_providers::grpc::alist::{
    alist_client::AlistClient as GrpcAlistClient, alist_server::AlistServer, login_req, FsGetReq,
    FsListReq, FsOtherReq, FsSearchReq, LoginReq, MeReq,
};
use synctv_media_providers::grpc::AlistService as GrpcAlistService;
use synctv_media_providers::AlistClient;
use synctv_media_providers::PROVIDER_USER_AGENT;
use tonic::transport::Server;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

// validate_path is a private function, but we test it indirectly through the public API
// (fs_get, fs_list, etc.) which call validate_path internally.

fn provider_headers() -> HashMap<String, String> {
    HashMap::from([("User-Agent".to_string(), PROVIDER_USER_AGENT.to_string())])
}

#[tokio::test]
async fn test_validate_path_url_encoded_dotdot_rejected() {
    // "%2e%2e" decodes to ".." which should be rejected
    let client = AlistClient::with_token("https://alist.example.com", "token123").unwrap();
    let result = client
        .fs_get("/movies/%2e%2e/etc/passwd", None, &provider_headers())
        .await;
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
    let result = client
        .fs_get("/movies%2F%2e%2e/secret", None, &provider_headers())
        .await;
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let result = client
        .fs_get("/movies/video.mp4", None, &provider_headers())
        .await;
    assert!(result.is_ok(), "Normal path should be accepted: {result:?}");
}

#[tokio::test]
async fn test_validate_path_dotfiles_accepted() {
    // Dotfiles like ".hidden" should be allowed (only ".." and "." are rejected)
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let result = client
        .fs_get("/movies/.hidden", None, &provider_headers())
        .await;
    assert!(
        result.is_ok(),
        "Dotfile path should be accepted: {result:?}"
    );
}

#[tokio::test]
async fn test_alist_client_login_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {"token": "test-jwt-token-abc123"}
        })))
        .mount(&server)
        .await;

    let mut client = AlistClient::new(server.uri()).unwrap();
    let token = client
        .login_with_otp("admin", "password123", false, None)
        .await
        .unwrap();
    assert_eq!(token, "test-jwt-token-abc123");
    assert!(client.has_token());
}

#[tokio::test]
async fn test_alist_client_login_wrong_password() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 400,
            "message": "wrong password",
            "data": null
        })))
        .mount(&server)
        .await;

    let mut client = AlistClient::new(server.uri()).unwrap();
    let result = client
        .login_with_otp("admin", "wrong_password", false, None)
        .await;
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "name": "movie.mkv",
                "size": 2_000_000_000_u64,
                "is_dir": false,
                "modified": 1_700_000_000,
                "created": 1_699_000_000,
                "sign": "sig123",
                "thumb": "",
                "type": 6,
                "raw_url": "https://cdn.example.com/movie.mkv",
                "provider": "s3",
                "related": []
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_get("/movies/movie.mkv", None, &provider_headers())
        .await
        .unwrap();
    assert_eq!(resp.name, "movie.mkv");
    assert_eq!(resp.size, 2_000_000_000);
    assert!(!resp.is_dir);
    assert_eq!(resp.raw_url, "https://cdn.example.com/movie.mkv");
}

#[tokio::test]
async fn test_alist_client_fs_get_sends_auth_headers_and_password() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/get"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 200,
            "message": "success",
            "data": {
                "name": "movie.mkv",
                "size": 4096,
                "is_dir": false,
                "modified": "2026-04-24T19:08:51.041866104Z",
                "created": "2026-04-24T19:08:51.041866104Z",
                "raw_url": "https://cdn.example.com/movie.mkv",
                "provider": "Local",
                "related": null
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let mut headers = provider_headers();
    headers.insert("X-Alist-Test".to_string(), "custom-header".to_string());
    let resp = client
        .fs_get("/protected/movie.mkv", Some("dir-password"), &headers)
        .await
        .unwrap();
    assert_eq!(resp.name, "movie.mkv");
    assert!(resp.related.is_empty());

    let requests = server
        .received_requests()
        .await
        .expect("wiremock should record requests");
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(
        request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some("token123")
    );
    assert_eq!(
        request
            .headers
            .get("origin")
            .and_then(|value| value.to_str().ok()),
        Some(server.uri().as_str())
    );
    assert_eq!(
        request
            .headers
            .get("user-agent")
            .and_then(|value| value.to_str().ok()),
        Some(PROVIDER_USER_AGENT)
    );
    assert_eq!(
        request
            .headers
            .get("x-alist-test")
            .and_then(|value| value.to_str().ok()),
        Some("custom-header")
    );
    let body: serde_json::Value = request.body_json().expect("fs/get body should be JSON");
    assert_eq!(body["path"], json!("/protected/movie.mkv"));
    assert_eq!(body["password"], json!("dir-password"));
    assert_eq!(body["user_agent"], json!(PROVIDER_USER_AGENT));
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

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_list("/movies", 1, 20, None).await.unwrap();
    assert_eq!(resp.total, 2);
    assert_eq!(resp.content.len(), 2);
    assert_eq!(resp.content[0].name, "file1.mp4");
    assert!(!resp.content[0].is_dir);
    assert!(resp.content[1].is_dir);
}

#[tokio::test]
async fn test_alist_client_fs_list_with_refresh_sends_full_request_and_accepts_null_content() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/list"))
        .and(header("authorization", "token123"))
        .and(body_json(json!({
            "path": "/empty",
            "password": "dir-password",
            "page": 2,
            "per_page": 25,
            "refresh": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 200,
            "message": "success",
            "data": {
                "content": null,
                "total": 0,
                "readme": "",
                "write": true,
                "provider": "Local"
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_list_with_refresh("/empty", 2, 25, Some("dir-password"), true)
        .await
        .unwrap();
    assert_eq!(resp.total, 0);
    assert!(resp.content.is_empty());
    assert!(resp.write);
}

#[tokio::test]
async fn test_alist_client_fs_list_accepts_rfc3339_timestamps() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/list"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "content": [
                    {
                        "name": "config.json",
                        "size": 2446,
                        "is_dir": false,
                        "modified": "2026-04-23T12:05:17.760140848Z",
                        "sign": "sig",
                        "thumb": "",
                        "type": 4
                    }
                ],
                "total": 1,
                "readme": "",
                "write": true,
                "provider": "Local",
                "filtered_total": 1,
                "page": 1,
                "per_page": 50,
                "has_more": false,
                "pages_total": 1
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.fs_list("/local", 1, 50, None).await.unwrap();
    assert_eq!(resp.total, 1);
    assert_eq!(resp.content.len(), 1);
    assert_eq!(resp.content[0].name, "config.json");
    assert!(resp.content[0].modified > 0);
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
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
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let result = client
        .fs_get("/movies/video.mp4", None, &provider_headers())
        .await;
    assert!(result.is_ok(), "Should succeed after retry: {result:?}");
    assert_eq!(result.unwrap().name, "video.mp4");
}

#[tokio::test]
async fn test_alist_client_fs_other_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "drive_id": "drive-abc",
                "file_id": "file-xyz",
                "video_preview_play_info": {
                    "category": "live_transcoding",
                    "live_transcoding_subtitle_task_list": [],
                    "live_transcoding_task_list": [
                        {
                            "stage": "finished",
                            "status": "finished",
                            "template_height": 720,
                            "template_id": "264_720p",
                            "template_name": "720P",
                            "template_width": 1280,
                            "url": "https://cdn.example.com/transcode/720p.m3u8"
                        }
                    ],
                    "meta": {
                        "duration": 120.5,
                        "height": 1080,
                        "width": 1920
                    }
                }
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_other("/movies/video.mp4", "video_preview", None)
        .await
        .unwrap();
    assert_eq!(resp.drive_id, "drive-abc");
    assert_eq!(resp.file_id, "file-xyz");
    let preview = resp.video_preview_play_info.unwrap();
    assert_eq!(preview.category, "live_transcoding");
    assert_eq!(preview.live_transcoding_task_list.len(), 1);
    assert_eq!(preview.live_transcoding_task_list[0].template_name, "720P");
    assert_eq!(preview.live_transcoding_task_list[0].template_height, 720);
    let meta = preview.meta.unwrap();
    assert_eq!(meta.width, 1920);
    assert_eq!(meta.height, 1080);
    assert!((meta.duration - 120.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn test_alist_client_fs_other_no_preview() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/other"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "drive_id": "drive-abc",
                "file_id": "file-xyz",
                "video_preview_play_info": null
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_other("/docs/readme.txt", "video_preview", None)
        .await
        .unwrap();
    assert!(resp.video_preview_play_info.is_none());
}

#[tokio::test]
async fn test_alist_client_fs_other_accepts_null_video_preview_lists() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/other"))
        .and(body_json(json!({
            "path": "/movies/video.mp4",
            "method": "video_preview",
            "password": ""
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 200,
            "message": "success",
            "data": {
                "drive_id": "drive-abc",
                "file_id": "file-xyz",
                "video_preview_play_info": {
                    "category": "live_transcoding",
                    "live_transcoding_subtitle_task_list": null,
                    "live_transcoding_task_list": null,
                    "meta": null
                }
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_other("/movies/video.mp4", "video_preview", None)
        .await
        .unwrap();
    let preview = resp.video_preview_play_info.unwrap();
    assert_eq!(preview.category, "live_transcoding");
    assert!(preview.live_transcoding_task_list.is_empty());
    assert!(preview.live_transcoding_subtitle_task_list.is_empty());
}

#[tokio::test]
async fn test_alist_client_fs_search_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "content": [
                    {
                        "parent": "/movies",
                        "name": "inception.mkv",
                        "is_dir": false,
                        "size": 3_000_000_000_u64,
                        "type": 6
                    },
                    {
                        "parent": "/movies/inception",
                        "name": "subtitles.srt",
                        "is_dir": false,
                        "size": 50000,
                        "type": 0
                    }
                ],
                "total": 2
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_search("/movies", "inception", 1, 1, 20, None)
        .await
        .unwrap();
    assert_eq!(resp.total, 2);
    assert_eq!(resp.content.len(), 2);
    assert_eq!(resp.content[0].name, "inception.mkv");
    assert_eq!(resp.content[0].parent, "/movies");
    assert!(!resp.content[0].is_dir);
    assert_eq!(resp.content[1].name, "subtitles.srt");
}

#[tokio::test]
async fn test_alist_client_fs_search_empty_results() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/search"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "content": [],
                "total": 0
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_search("/movies", "nonexistent", 0, 1, 20, None)
        .await
        .unwrap();
    assert_eq!(resp.total, 0);
    assert!(resp.content.is_empty());
}

#[tokio::test]
async fn test_alist_client_fs_search_sends_password_and_accepts_null_content() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/search"))
        .and(header("authorization", "token123"))
        .and(body_json(json!({
            "parent": "/movies",
            "keywords": "clip",
            "scope": 1,
            "page": 3,
            "per_page": 10,
            "password": "search-password"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 200,
            "message": "success",
            "data": {
                "content": null,
                "total": 0
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client
        .fs_search("/movies", "clip", 1, 3, 10, Some("search-password"))
        .await
        .unwrap();
    assert_eq!(resp.total, 0);
    assert!(resp.content.is_empty());
}

#[tokio::test]
async fn test_alist_client_fs_search_traversal_rejected() {
    let client = AlistClient::with_token("https://alist.example.com", "token123").unwrap();
    let result = client
        .fs_search("/movies/../etc", "passwd", 1, 1, 20, None)
        .await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("traversal"),
        "Expected traversal error, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_alist_client_me_success() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "id": 1,
                "username": "admin",
                "base_path": "/",
                "role": 2,
                "disabled": false,
                "permission": 0,
                "sso_id": "",
                "otp": false
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.me().await.unwrap();
    assert_eq!(resp.id, 1);
    assert_eq!(resp.username, "admin");
    assert_eq!(resp.base_path, "/");
    assert_eq!(resp.role, 2);
    assert!(!resp.disabled);
    assert!(!resp.otp);
}

#[tokio::test]
async fn test_alist_client_me_accepts_role_array_response() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {
                "id": 2,
                "username": "admin",
                "password": "",
                "base_path": "/",
                "role": [2],
                "disabled": false,
                "permission": 65535,
                "sso_id": "",
                "otp": false,
                "role_names": ["admin"],
                "permissions": [{"path": "/", "permission": 65535}]
            }
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "token123").unwrap();
    let resp = client.me().await.unwrap();
    assert_eq!(resp.id, 2);
    assert_eq!(resp.username, "admin");
    assert_eq!(resp.base_path, "/");
    assert_eq!(resp.role, 2);
    assert!(!resp.disabled);
    assert!(!resp.otp);
}

#[tokio::test]
async fn test_alist_client_me_unauthorized() {
    let server = MockServer::start().await;

    Mock::given(method("GET"))
        .and(path("/api/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 401,
            "message": "unauthorized",
            "data": null
        })))
        .mount(&server)
        .await;

    let client = AlistClient::with_token(server.uri(), "bad-token").unwrap();
    let result = client.me().await;
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("401") || err_msg.contains("unauthorized"),
        "Expected unauthorized error, got: {err_msg}"
    );
}

#[tokio::test]
async fn test_alist_client_login_hashed_success() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login/hash"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 200,
            "message": "success",
            "data": {"token": "hashed-login-token-xyz"}
        })))
        .mount(&server)
        .await;

    let mut client = AlistClient::new(server.uri()).unwrap();
    let token = client
        .login_with_otp("admin", "sha256_hash_of_password", true, None)
        .await
        .unwrap();
    assert_eq!(token, "hashed-login-token-xyz");
    assert!(client.has_token());
}

#[tokio::test]
async fn test_alist_client_login_forwards_otp_code() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login/hash"))
        .and(body_json(json!({
            "username": "admin",
            "password": "hashed-secret",
            "otp_code": "123456"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 200,
            "message": "success",
            "data": {"token": "otp-token"}
        })))
        .mount(&server)
        .await;

    let mut client = AlistClient::new(server.uri()).unwrap();
    let token = client
        .login_with_otp("admin", "hashed-secret", true, Some("123456"))
        .await
        .unwrap();

    assert_eq!(token, "otp-token");
}

#[tokio::test]
async fn test_alist_client_login_hashed_wrong_password() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login/hash"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 400,
            "message": "wrong password",
            "data": null
        })))
        .mount(&server)
        .await;

    let mut client = AlistClient::new(server.uri()).unwrap();
    let result = client
        .login_with_otp("admin", "wrong_hash", true, None)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_alist_service_login_requires_one_credential() {
    let service = AlistService::new().expect("provider HTTP client should build");
    let err = service
        .login(LoginReq {
            host: "http://127.0.0.1:5244".to_string(),
            username: "admin".to_string(),
            credential: None,
            otp_code: String::new(),
        })
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("password"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_alist_service_login_uses_hashed_password_endpoint() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/auth/login/hash"))
        .and(body_json(json!({
            "username": "admin",
            "password": "hashed-secret",
            "otp_code": ""
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 200,
            "message": "success",
            "data": {"token": "hashed-token"}
        })))
        .mount(&server)
        .await;

    let service = AlistService::new().expect("provider HTTP client should build");
    let token = service
        .login(LoginReq {
            host: server.uri(),
            username: "admin".to_string(),
            credential: Some(login_req::Credential::HashedPassword(
                "hashed-secret".to_string(),
            )),
            otp_code: String::new(),
        })
        .await
        .unwrap();
    assert_eq!(token, "hashed-token");
}

#[tokio::test]
async fn test_alist_service_fs_list_forwards_refresh_password_and_pagination() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/api/fs/list"))
        .and(header("authorization", "token123"))
        .and(body_json(json!({
            "path": "/local",
            "password": "dir-password",
            "page": 4,
            "per_page": 8,
            "refresh": true
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 200,
            "message": "success",
            "data": {
                "content": [
                    {"name": "video.mp4", "size": 15, "is_dir": false, "modified": 0, "sign": "", "thumb": "", "type": 2}
                ],
                "total": 1,
                "readme": "",
                "write": true,
                "provider": "Local"
            }
        })))
        .mount(&server)
        .await;

    let service = AlistService::new().expect("provider HTTP client should build");
    let resp = service
        .fs_list(FsListReq {
            host: server.uri(),
            token: "token123".to_string(),
            path: "/local".to_string(),
            password: "dir-password".to_string(),
            page: 4,
            per_page: 8,
            refresh: true,
        })
        .await
        .unwrap();
    assert_eq!(resp.total, 1);
    assert_eq!(resp.content[0].name, "video.mp4");
}

struct OpenListFixture {
    _container: ExternalServiceContainer,
    host: String,
    token: String,
}

fn openlist_image() -> (String, String) {
    let image = std::env::var("SYNCTV_OPENLIST_IMAGE")
        .unwrap_or_else(|_| "ghcr.io/openlistteam/openlist-git".to_string());
    let tag = std::env::var("SYNCTV_OPENLIST_TAG").unwrap_or_else(|_| "latest".to_string());
    (image, tag)
}

async fn login_openlist_when_ready(host: &str, password: &str) -> String {
    let started_at = tokio::time::Instant::now();
    let timeout = std::time::Duration::from_secs(30);
    let mut last_error = None;

    while started_at.elapsed() < timeout {
        let mut client = AlistClient::new(host).unwrap();
        match client.login_with_otp("admin", password, false, None).await {
            Ok(token) => return token,
            Err(error) => {
                last_error = Some(error.to_string());
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }

    panic!(
        "OpenList did not accept admin login within {:?}; last error: {}",
        timeout,
        last_error.unwrap_or_else(|| "no login attempt completed".to_string())
    );
}

async fn start_openlist_fixture() -> OpenListFixture {
    const ADMIN_PASSWORD: &str = "synctv-openlist-test";
    let (image_name, image_tag) = openlist_image();
    let container = start_external_service(
        ExternalServiceRequest::new("openlist", "synctv-openlist-", image_name, image_tag, 5244)
            .with_stdout_ready_message("start HTTP server")
            .with_user("0:0")
            .with_env("OPENLIST_ADMIN_PASSWORD", ADMIN_PASSWORD)
            .with_copy_to(
                "/srv/openlist-files/video.mp4",
                b"hello-openlist\n".to_vec(),
            )
            .with_copy_to(
                "/srv/openlist-files/folder/subtitle.srt",
                b"subtitle\n".to_vec(),
            )
            .with_post_start_shell_command("mkdir -p /srv/openlist-files/empty"),
    )
    .await;

    let host = container.http_url();
    let token = login_openlist_when_ready(&host, ADMIN_PASSWORD).await;

    let addition = json!({
        "root_folder_path": "/srv/openlist-files",
        "thumbnail": false,
        "thumb_cache_folder": "",
        "show_hidden": true,
        "mkdir_perm": "777"
    })
    .to_string();

    let create_resp: serde_json::Value = reqwest::Client::new()
        .post(format!("{host}/api/admin/storage/create"))
        .header("Authorization", &token)
        .json(&json!({
            "mount_path": "/local",
            "order": 0,
            "remark": "synctv test local",
            "cache_expiration": 30,
            "web_proxy": false,
            "webdav_policy": "native_proxy",
            "down_proxy_url": "",
            "extract_folder": "front",
            "enable_sign": false,
            "driver": "Local",
            "order_by": "name",
            "order_direction": "asc",
            "addition": addition,
            "disabled": false
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        create_resp["code"], 200,
        "storage create failed: {create_resp}"
    );

    OpenListFixture {
        _container: container,
        host,
        token,
    }
}

async fn spawn_alist_grpc_server() -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        Server::builder()
            .add_service(AlistServer::new(
                GrpcAlistService::new().expect("provider HTTP client should build"),
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .unwrap();
    });

    (format!("http://{addr}"), handle)
}

#[tokio::test]
#[ignore = "Requires Docker and the OpenList image"]
async fn test_openlist_container_exercises_real_alist_client_api() {
    let fixture = start_openlist_fixture().await;
    let client = AlistClient::with_token(&fixture.host, &fixture.token).unwrap();

    let me = client.me().await.unwrap();
    assert_eq!(me.username, "admin");
    assert_eq!(me.base_path, "/");
    assert!(!me.disabled);

    let root = client
        .fs_list_with_refresh("/local", 1, 20, None, true)
        .await
        .unwrap();
    assert_eq!(root.total, 3);
    let names: Vec<&str> = root.content.iter().map(|item| item.name.as_str()).collect();
    assert!(names.contains(&"video.mp4"));
    assert!(names.contains(&"folder"));
    assert!(names.contains(&"empty"));

    let empty = client
        .fs_list_with_refresh("/local/empty", 1, 20, None, true)
        .await
        .unwrap();
    assert_eq!(empty.total, 0);
    assert!(empty.content.is_empty());

    let file = client
        .fs_get("/local/video.mp4", None, &provider_headers())
        .await
        .unwrap();
    assert_eq!(file.name, "video.mp4");
    assert_eq!(file.size, 15);
    assert_eq!(file.provider, "Local");
    assert!(file.related.is_empty());
    assert!(!file.raw_url.is_empty());

    let body = reqwest::get(&file.raw_url)
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "hello-openlist\n");

    let search_err = client
        .fs_search("/local", "video", 1, 1, 20, None)
        .await
        .unwrap_err();
    assert!(
        search_err.to_string().contains("search not available")
            || search_err.to_string().contains("404"),
        "unexpected search error: {search_err}"
    );

    let other_err = client
        .fs_other("/local/video.mp4", "video_preview", None)
        .await
        .unwrap_err();
    assert!(
        other_err.to_string().contains("not implement") || other_err.to_string().contains("500"),
        "unexpected fs/other error: {other_err}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker and the OpenList image"]
async fn test_openlist_container_rejects_wrong_password() {
    let fixture = start_openlist_fixture().await;
    let mut client = AlistClient::new(&fixture.host).unwrap();
    let err = client
        .login_with_otp("admin", "wrong-password", false, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("password") || err.to_string().contains("401"),
        "unexpected login error: {err}"
    );
}

#[tokio::test]
#[ignore = "Requires Docker and the OpenList image"]
async fn test_openlist_container_exercises_real_alist_grpc_service() {
    let fixture = start_openlist_fixture().await;
    let (grpc_endpoint, server_handle) = spawn_alist_grpc_server().await;
    let mut client = GrpcAlistClient::connect(grpc_endpoint).await.unwrap();

    let token = client
        .login(LoginReq {
            host: fixture.host.clone(),
            username: "admin".to_string(),
            credential: Some(login_req::Credential::Password(
                "synctv-openlist-test".to_string(),
            )),
            otp_code: String::new(),
        })
        .await
        .unwrap()
        .into_inner()
        .token;
    assert!(!token.is_empty());

    let me = client
        .me(MeReq {
            host: fixture.host.clone(),
            token: token.clone(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(me.username, "admin");
    assert_eq!(me.base_path, "/");

    let list = client
        .fs_list(FsListReq {
            host: fixture.host.clone(),
            token: token.clone(),
            path: "/local".to_string(),
            password: String::new(),
            page: 1,
            per_page: 20,
            refresh: true,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(list.total, 3);
    assert!(list.content.iter().any(|item| item.name == "video.mp4"));

    let file = client
        .fs_get(FsGetReq {
            host: fixture.host.clone(),
            token: token.clone(),
            path: "/local/video.mp4".to_string(),
            password: String::new(),
            headers: HashMap::new(),
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(file.name, "video.mp4");
    assert_eq!(file.size, 15);
    assert_eq!(file.provider, "Local");
    assert!(!file.raw_url.is_empty());

    let search_status = client
        .fs_search(FsSearchReq {
            host: fixture.host.clone(),
            token: token.clone(),
            parent: "/local".to_string(),
            keywords: "video".to_string(),
            scope: 1,
            page: 1,
            per_page: 20,
            password: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(search_status.code(), tonic::Code::NotFound);

    let other_status = client
        .fs_other(FsOtherReq {
            host: fixture.host,
            token,
            path: "/local/video.mp4".to_string(),
            method: "video_preview".to_string(),
            password: String::new(),
        })
        .await
        .unwrap_err();
    assert_eq!(other_status.code(), tonic::Code::Internal);

    server_handle.abort();
}
