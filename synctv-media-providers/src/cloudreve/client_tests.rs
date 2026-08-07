use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::CloudreveClient;

fn client(server: &MockServer) -> CloudreveClient {
    CloudreveClient::with_http_client(&server.uri(), reqwest::Client::new())
        .expect("wiremock URL should be valid")
}

#[tokio::test]
async fn login_uses_cloudreve_v4_token_contract() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v4/session/token"))
        .and(body_json(serde_json::json!({
            "email": "alice@example.com",
            "password": "secret"
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 0,
            "data": {
                "user": {
                    "id": "user-id",
                    "nickname": "Alice"
                },
                "token": {
                    "access_token": "access-token",
                    "refresh_token": "refresh-token",
                    "access_expires": "2026-07-11T11:00:00Z",
                    "refresh_expires": "2026-08-11T10:00:00Z"
                }
            },
            "msg": ""
        })))
        .mount(&server)
        .await;

    let token = client(&server)
        .login("alice@example.com", "secret")
        .await
        .expect("login should succeed");

    assert_eq!(token.access_token, "access-token");
    assert_eq!(token.refresh_token, "refresh-token");
}

#[tokio::test]
async fn list_maps_cloudreve_files_and_pagination() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/file"))
        .and(header("authorization", "Bearer access-token"))
        .and(query_param("uri", "cloudreve://my/Movies"))
        .and(query_param("page", "2"))
        .and(query_param("next_page_token", "cursor-1"))
        .and(query_param("page_size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 0,
            "data": {
                "files": [{
                    "id": "file-id",
                    "name": "movie.mp4",
                    "path": "cloudreve://my/Movies/movie.mp4",
                    "size": 1024,
                    "type": 0,
                    "updated_at": "2026-07-11T10:00:00Z",
                    "metadata": {"thumb": "https://example.com/thumb.jpg"}
                }],
                "pagination": {
                    "next_token": "cursor-2",
                    "is_cursor": true
                }
            },
            "msg": ""
        })))
        .mount(&server)
        .await;

    let response = client(&server)
        .list("access-token", "/Movies", 2, Some("cursor-1"), 20)
        .await
        .expect("list should succeed");

    assert_eq!(response.files.len(), 1);
    assert_eq!(response.files[0].name, "movie.mp4");
    assert!(!response.files[0].is_dir());
    let pagination = response.pagination.expect("pagination");
    assert_eq!(pagination.next_token, "cursor-2");
    assert!(pagination.is_cursor);
}

#[tokio::test]
async fn list_supports_offset_pagination_without_a_cursor() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/file"))
        .and(query_param("uri", "cloudreve://my/Movies"))
        .and(query_param("page", "3"))
        .and(query_param("page_size", "10"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 0,
            "data": {
                "files": [],
                "pagination": {
                    "page": 3,
                    "page_size": 10,
                    "total_items": 25
                }
            },
            "msg": ""
        })))
        .mount(&server)
        .await;

    let response = client(&server)
        .list("access-token", "/Movies", 3, None, 10)
        .await
        .expect("offset list should succeed");
    let pagination = response.pagination.expect("pagination");
    assert!(!pagination.is_cursor);
    assert_eq!(pagination.total_items, 25);
}

#[tokio::test]
async fn list_normalizes_an_empty_path_to_the_cloudreve_root() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/file"))
        .and(query_param("uri", "cloudreve://my/"))
        .and(query_param("page", "1"))
        .and(query_param("page_size", "20"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 0,
            "data": {
                "files": [],
                "pagination": {
                    "next_token": "",
                    "is_cursor": true
                }
            },
            "msg": ""
        })))
        .mount(&server)
        .await;

    client(&server)
        .list("access-token", "", 1, None, 20)
        .await
        .expect("root list should succeed");
}

#[tokio::test]
async fn file_url_requests_current_signed_playback_url() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v4/file/url"))
        .and(header("authorization", "Bearer access-token"))
        .and(body_json(serde_json::json!({
            "uris": ["cloudreve://my/Movies/movie.mp4"],
            "download": false,
            "redirect": false
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 0,
            "data": {
                "urls": [{
                    "Url": "https://cdn.example.com/movie.mp4?sign=1",
                    "ExpireAt": "2026-07-11T11:00:00Z"
                }],
                "expires": "2026-07-11T11:00:00Z"
            },
            "msg": ""
        })))
        .mount(&server)
        .await;

    let response = client(&server)
        .file_url("access-token", "cloudreve://my/Movies/movie.mp4")
        .await
        .expect("file URL should succeed");

    assert_eq!(
        response.urls[0].url,
        "https://cdn.example.com/movie.mp4?sign=1"
    );
    assert_eq!(
        response.expires.expect("expires").timestamp(),
        1_783_767_600
    );
}

#[tokio::test]
async fn thumbnail_requests_current_signed_thumbnail_url() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/v4/file/thumb"))
        .and(header("authorization", "Bearer access-token"))
        .and(query_param("uri", "cloudreve://my/Movies/movie.mp4"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "code": 0,
            "data": {
                "url": "https://cdn.example.com/thumb.jpg?sign=1",
                "expires": "2026-07-11T11:00:00Z"
            },
            "msg": ""
        })))
        .mount(&server)
        .await;

    let response = client(&server)
        .thumbnail("access-token", "cloudreve://my/Movies/movie.mp4")
        .await
        .expect("thumbnail should succeed");

    assert_eq!(response.url, "https://cdn.example.com/thumb.jpg?sign=1");
    assert_eq!(
        response.expires.expect("expires").timestamp(),
        1_783_767_600
    );
}
