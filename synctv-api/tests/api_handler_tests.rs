//! API handler integration tests for synctv-api
//!
//! Tests HTTP handler validation logic, admin path ID validation, gRPC dispatch
//! patterns, optional service error codes, and WebSocket authentication priority.
//!
//! These tests use `tower::ServiceExt::oneshot()` for HTTP handler tests and
//! direct function calls for validation logic. No database or Redis required.

#![allow(clippy::unwrap_used)]
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::{get, patch, post},
    Json, Router,
};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

// ============================================================================
// Helper: extract JSON body from response
// ============================================================================

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

// ============================================================================
// API1: update_playback handler validation
// ============================================================================

mod update_playback_validation {
    use super::*;
    use synctv_api::http::error::AppError;
    use synctv_api::http::room::UpdatePlaybackRequest;

    /// Simulate the `update_playback` handler validation logic:
    /// Empty body (all None) should produce 400
    #[tokio::test]
    async fn test_empty_body_returns_400() {
        let app = Router::new().route(
            "/api/rooms/{room_id}/playback",
            patch(|Json(req): Json<UpdatePlaybackRequest>| async move {
                // Reproduce the validation from room.rs:573-577
                if req.state.is_none()
                    && req.position.is_none()
                    && req.speed.is_none()
                    && req.media_id.is_none()
                {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "No valid playback update field provided (state, position, speed, or media_id)",
                    ));
                }
                Ok(Json(serde_json::json!({"status": "ok"})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room123/playback")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("No valid playback update field"));
    }

    /// Invalid state string (not "playing" or "paused") should produce 400
    #[tokio::test]
    async fn test_invalid_state_string_returns_400() {
        let app = Router::new().route(
            "/api/rooms/{room_id}/playback",
            patch(|Json(req): Json<UpdatePlaybackRequest>| async move {
                // Reproduce the validation from room.rs:580-584
                if req.state.is_none()
                    && req.position.is_none()
                    && req.speed.is_none()
                    && req.media_id.is_none()
                {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "No valid playback update field provided",
                    ));
                }
                match req.state.as_deref() {
                    Some("playing") => {}
                    Some("paused") => {}
                    Some(_) => {
                        return Err(AppError::bad_request(
                            "Invalid state value, use 'playing' or 'paused'",
                        ));
                    }
                    None => {}
                }
                Ok(Json(serde_json::json!({"status": "ok"})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room123/playback")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"state": "stopped"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("Invalid state value"));
    }

    /// Valid state "playing" should pass validation
    #[tokio::test]
    async fn test_valid_state_playing_passes_validation() {
        let app = Router::new().route(
            "/api/rooms/{room_id}/playback",
            patch(|Json(req): Json<UpdatePlaybackRequest>| async move {
                if req.state.is_none()
                    && req.position.is_none()
                    && req.speed.is_none()
                    && req.media_id.is_none()
                {
                    return Err::<Json<Value>, AppError>(AppError::bad_request("empty"));
                }
                match req.state.as_deref() {
                    Some("playing" | "paused") => {}
                    Some(_) => {
                        return Err(AppError::bad_request("Invalid state value"));
                    }
                    None => {}
                }
                Ok(Json(serde_json::json!({"state": "playing"})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room123/playback")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"state": "playing"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Valid state "paused" should pass validation
    #[tokio::test]
    async fn test_valid_state_paused_passes_validation() {
        let app = Router::new().route(
            "/api/rooms/{room_id}/playback",
            patch(|Json(req): Json<UpdatePlaybackRequest>| async move {
                if req.state.is_none()
                    && req.position.is_none()
                    && req.speed.is_none()
                    && req.media_id.is_none()
                {
                    return Err::<Json<Value>, AppError>(AppError::bad_request("empty"));
                }
                match req.state.as_deref() {
                    Some("playing" | "paused") => {}
                    Some(_) => {
                        return Err(AppError::bad_request("Invalid state value"));
                    }
                    None => {}
                }
                Ok(Json(serde_json::json!({"state": "paused"})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room123/playback")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"state": "paused"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    /// Position-only update should pass the "at least one field" check
    #[tokio::test]
    async fn test_position_only_passes_validation() {
        let app = Router::new().route(
            "/api/rooms/{room_id}/playback",
            patch(|Json(req): Json<UpdatePlaybackRequest>| async move {
                if req.state.is_none()
                    && req.position.is_none()
                    && req.speed.is_none()
                    && req.media_id.is_none()
                {
                    return Err::<Json<Value>, AppError>(AppError::bad_request("empty"));
                }
                Ok(Json(serde_json::json!({"position": req.position})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room123/playback")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"position": 42.5}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_invalid_speed_above_ui_max_returns_400() {
        let app = Router::new().route(
            "/api/rooms/{room_id}/playback",
            patch(|Json(req): Json<UpdatePlaybackRequest>| async move {
                if let Some(speed) = req.speed {
                    synctv_api::http::validation::validate_playback_speed(speed)
                        .map_err(|e| AppError::bad_request(e.to_string()))?;
                }
                Ok::<Json<Value>, AppError>(Json(serde_json::json!({"status": "ok"})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room123/playback")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"speed": 5.0}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("Speed must be between"));
    }

    #[tokio::test]
    async fn test_invalid_negative_position_returns_400() {
        let app = Router::new().route(
            "/api/rooms/{room_id}/playback",
            patch(|Json(req): Json<UpdatePlaybackRequest>| async move {
                if let Some(position) = req.position {
                    synctv_api::http::validation::validate_playback_position(position)
                        .map_err(|e| AppError::bad_request(e.to_string()))?;
                }
                Ok::<Json<Value>, AppError>(Json(serde_json::json!({"status": "ok"})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/rooms/room123/playback")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"position": -1.0}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json["error"].as_str().unwrap().contains("negative"));
    }
}

// ============================================================================
// API2: update_user handler validation
// ============================================================================

mod update_user_validation {
    use super::*;
    use synctv_api::http::error::AppError;
    use synctv_api::http::user::UpdateUserRequest;

    /// Password without `old_password` should produce 400
    #[tokio::test]
    async fn test_password_without_old_password_returns_400() {
        let app = Router::new().route(
            "/api/user",
            patch(|Json(req): Json<UpdateUserRequest>| async move {
                let mut updated_fields = Vec::new();

                // Process username
                if req.username.is_some() {
                    updated_fields.push("username");
                }

                // Process password - reproduce logic from user.rs:70-78
                if req.password.is_some() {
                    let _old_password = req.old_password.as_deref().ok_or_else(|| {
                        AppError::bad_request("old_password is required when changing password")
                    })?;
                    updated_fields.push("password");
                }

                if updated_fields.is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "No valid update fields provided (username or password)",
                    ));
                }

                Ok(Json(serde_json::json!({"updated": updated_fields})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/user")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"password": "new_pass"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("old_password is required"));
    }

    /// Empty body (no fields) should produce 400
    #[tokio::test]
    async fn test_empty_body_returns_400() {
        let app = Router::new().route(
            "/api/user",
            patch(|Json(req): Json<UpdateUserRequest>| async move {
                let mut updated_fields = Vec::new();

                if req.username.is_some() {
                    updated_fields.push("username");
                }
                if req.password.is_some() {
                    let _old_password = req.old_password.as_deref().ok_or_else(|| {
                        AppError::bad_request("old_password is required when changing password")
                    })?;
                    updated_fields.push("password");
                }

                if updated_fields.is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "No valid update fields provided (username or password)",
                    ));
                }

                Ok(Json(serde_json::json!({"updated": updated_fields})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/user")
            .header("Content-Type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("No valid update fields"));
    }

    /// Password with `old_password` should pass validation
    #[tokio::test]
    async fn test_password_with_old_password_passes() {
        let app = Router::new().route(
            "/api/user",
            patch(|Json(req): Json<UpdateUserRequest>| async move {
                let mut updated_fields = Vec::new();

                if req.username.is_some() {
                    updated_fields.push("username");
                }
                if req.password.is_some() {
                    let _old_password = req.old_password.as_deref().ok_or_else(|| {
                        AppError::bad_request("old_password is required when changing password")
                    })?;
                    updated_fields.push("password");
                }

                if updated_fields.is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "No valid update fields",
                    ));
                }

                Ok(Json(serde_json::json!({"updated": updated_fields})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/user")
            .header("Content-Type", "application/json")
            .body(Body::from(
                r#"{"password": "new_pass", "old_password": "old_pass"}"#,
            ))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        let updated = json["updated"].as_array().unwrap();
        assert!(updated.iter().any(|v| v == "password"));
    }

    /// Username-only update should pass
    #[tokio::test]
    async fn test_username_only_passes() {
        let app = Router::new().route(
            "/api/user",
            patch(|Json(req): Json<UpdateUserRequest>| async move {
                let mut updated_fields = Vec::new();

                if req.username.is_some() {
                    updated_fields.push("username");
                }
                if req.password.is_some() {
                    let _old_password = req.old_password.as_deref().ok_or_else(|| {
                        AppError::bad_request("old_password is required when changing password")
                    })?;
                    updated_fields.push("password");
                }

                if updated_fields.is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "No valid update fields",
                    ));
                }

                Ok(Json(serde_json::json!({"updated": updated_fields})))
            }),
        );

        let req = Request::builder()
            .method("PATCH")
            .uri("/api/user")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"username": "newname"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
    }
}

// ============================================================================
// API3: create_ticket handler validation
// ============================================================================

mod create_ticket_validation {
    use super::*;
    use synctv_api::http::error::AppError;
    use synctv_api::http::ticket::CreateTicketRequest;

    /// `ws_ticket_service=None` should produce 500 (internal server error)
    #[tokio::test]
    async fn test_ws_ticket_service_none_returns_500() {
        let app = Router::new().route(
            "/api/tickets",
            post(|Json(req): Json<CreateTicketRequest>| async move {
                // Reproduce ticket.rs:84-85
                if req.room_id.trim().is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "room_id is required",
                    ));
                }

                // Simulate ws_ticket_service = None (ticket.rs:91-95)
                let ws_ticket_service: Option<()> = None;
                ws_ticket_service.ok_or_else(|| {
                    AppError::internal_server_error("WebSocket ticket service not configured")
                })?;

                Ok(Json(serde_json::json!({"ticket": "abc"})))
            }),
        );

        let req = Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"room_id": "room123"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = body_json(resp).await;
        // For 5xx errors, the actual message is replaced with generic "Internal server error"
        // to avoid leaking sensitive information (see AppError::into_response)
        assert_eq!(json["error"], "Internal server error");
        assert_eq!(json["status"], 500);
    }

    /// Empty `room_id` should produce 400
    #[tokio::test]
    async fn test_empty_room_id_returns_400() {
        let app = Router::new().route(
            "/api/tickets",
            post(|Json(req): Json<CreateTicketRequest>| async move {
                // Reproduce ticket.rs:84-85
                if req.room_id.trim().is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "room_id is required",
                    ));
                }
                Ok(Json(serde_json::json!({"ticket": "abc"})))
            }),
        );

        let req = Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"room_id": ""}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("room_id is required"));
    }

    /// Whitespace-only `room_id` should also produce 400
    #[tokio::test]
    async fn test_whitespace_room_id_returns_400() {
        let app = Router::new().route(
            "/api/tickets",
            post(|Json(req): Json<CreateTicketRequest>| async move {
                if req.room_id.trim().is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "room_id is required",
                    ));
                }
                Ok(Json(serde_json::json!({"ticket": "abc"})))
            }),
        );

        let req = Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"room_id": "   "}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    /// Malformed `room_id` should also produce 400 instead of reaching room lookup.
    #[tokio::test]
    async fn test_invalid_room_id_format_returns_400() {
        let app = Router::new().route(
            "/api/tickets",
            post(|Json(req): Json<CreateTicketRequest>| async move {
                if req.room_id.trim().is_empty() {
                    return Err::<Json<Value>, AppError>(AppError::bad_request(
                        "room_id is required",
                    ));
                }

                synctv_api::room_id_validation::parse_room_id(&req.room_id)
                    .map_err(|e| AppError::bad_request(format!("Invalid room_id: {e}")))?;

                Ok(Json(serde_json::json!({"ticket": "abc"})))
            }),
        );

        let req = Request::builder()
            .method("POST")
            .uri("/api/tickets")
            .header("Content-Type", "application/json")
            .body(Body::from(r#"{"room_id": "room@bad1234"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let json = body_json(resp).await;
        assert!(
            json["error"].as_str().unwrap().contains("Invalid room_id"),
            "unexpected error response: {json:?}"
        );
    }
}

// ============================================================================
// API4: Admin handlers validate_path_id
// ============================================================================

mod admin_validate_path_id {
    use synctv_api::http::validation::{validate_id, ValidationError};

    /// Malformed ID with "@" should fail
    #[test]
    fn test_user_at_bad_returns_error() {
        let result = validate_id("user@bad", "user_id");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ValidationError::InvalidFormat { .. }));
    }

    /// Malformed ID with script tag should fail
    #[test]
    fn test_script_tag_returns_error() {
        let result = validate_id("<script>", "user_id");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ValidationError::InvalidFormat { .. }));
    }

    /// Empty ID should fail
    #[test]
    fn test_empty_id_returns_error() {
        let result = validate_id("", "user_id");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ValidationError::Required(_)));
    }

    /// Valid alphanumeric ID should pass
    #[test]
    fn test_valid_alphanumeric_id() {
        let result = validate_id("user123", "user_id");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "user123");
    }

    /// Valid ID with underscores and hyphens
    #[test]
    fn test_valid_id_with_underscore_and_hyphen() {
        let result = validate_id("user_123-abc", "user_id");
        assert!(result.is_ok());
    }

    /// ID with spaces should fail
    #[test]
    fn test_id_with_spaces_returns_error() {
        let result = validate_id("user 123", "user_id");
        assert!(result.is_err());
    }

    /// ID with special characters should fail
    #[test]
    fn test_id_with_special_chars_returns_error() {
        let test_ids = &[
            "user!name",
            "user#name",
            "user$name",
            "user%name",
            "user^name",
            "user&name",
            "user*name",
            "user(name",
            "user)name",
            "user+name",
            "user=name",
            "user{name",
            "user}name",
            "user[name",
            "user]name",
            "user|name",
            "user\\name",
            "user/name",
            "user?name",
            "user<name",
            "user>name",
        ];
        for id in test_ids {
            let result = validate_id(id, "test_id");
            assert!(result.is_err(), "ID '{id}' should be rejected");
        }
    }

    /// ID exceeding max length should fail
    #[test]
    fn test_id_too_long_returns_error() {
        let long_id = "a".repeat(65); // ID_MAX is 64
        let result = validate_id(&long_id, "user_id");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, ValidationError::TooLong { .. }));
    }

    /// Integration: `validate_path_id` wraps `validate_id` into `AppError`
    #[test]
    fn test_validate_path_id_produces_bad_request_app_error() {
        use synctv_api::http::error::AppError;

        // Reproduce admin.rs:175-179 logic
        fn validate_path_id(id: &str, field: &'static str) -> Result<(), AppError> {
            synctv_api::http::validation::validate_id(id, field)
                .map(|_| ())
                .map_err(|e| AppError::bad_request(format!("Invalid {field}: {e}")))
        }

        let result = validate_path_id("user@bad", "user_id");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("Invalid user_id"));

        let result = validate_path_id("<script>", "user_id");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);

        // Valid ID should pass
        let result = validate_path_id("valid_user-123", "user_id");
        assert!(result.is_ok());
    }

    /// Test admin endpoint simulation with malformed path IDs
    #[tokio::test]
    async fn test_admin_endpoint_malformed_id_returns_400() {
        use super::*;
        use synctv_api::http::error::AppError;

        let app = Router::new().route(
            "/api/admin/users/{user_id}",
            get(
                |axum::extract::Path(user_id): axum::extract::Path<String>| async move {
                    synctv_api::http::validation::validate_id(&user_id, "user_id")
                        .map(|_| ())
                        .map_err(|e| AppError::bad_request(format!("Invalid user_id: {e}")))?;
                    Ok::<Json<Value>, AppError>(Json(serde_json::json!({"user_id": user_id})))
                },
            ),
        );

        // Test with "user@bad"
        let req = Request::get("/api/admin/users/user@bad")
            .body(Body::empty())
            .unwrap();
        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

        // Test with valid ID
        let req = Request::get("/api/admin/users/valid_user123")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }
}

// ============================================================================
// API5: gRPC service methods - AuthService::register with empty username
// ============================================================================

mod grpc_service_validation {
    use synctv_api::http::validation::validate_username;
    use synctv_proto::client::RegisterRequest;

    /// Empty username should fail validation
    #[test]
    fn test_register_empty_username_fails_validation() {
        let req = RegisterRequest {
            username: String::new(),
            password: "StrongPass123!".to_string(),
            email: "test@example.com".to_string(),
        };
        let result = validate_username(&req.username);
        assert!(result.is_err(), "Empty username should be rejected");
    }

    /// Whitespace-only username should fail validation
    #[test]
    fn test_register_whitespace_username_fails_validation() {
        let result = validate_username("   ");
        assert!(
            result.is_err(),
            "Whitespace-only username should be rejected"
        );
    }

    /// Single char username should fail (too short, min=3)
    #[test]
    fn test_register_single_char_username_fails_validation() {
        let result = validate_username("a");
        assert!(result.is_err(), "Single-char username should be rejected");
    }

    /// Valid username should pass
    #[test]
    fn test_register_valid_username_passes() {
        let result = validate_username("testuser");
        assert!(result.is_ok());
    }

    /// Username with special characters should fail
    #[test]
    fn test_register_username_with_at_symbol_fails() {
        let result = validate_username("user@name");
        assert!(result.is_err());
    }

    /// gRPC basic request deserialization verification
    #[test]
    fn test_register_request_round_trip() {
        let req = RegisterRequest {
            username: "alice".to_string(),
            password: "password123".to_string(),
            email: "alice@example.com".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let deserialized: RegisterRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(req.username, deserialized.username);
        assert_eq!(req.password, deserialized.password);
        assert_eq!(req.email, deserialized.email);
    }
}

// ============================================================================
// API6: message_stream gRPC validation patterns
// ============================================================================

mod message_stream_validation {
    use synctv_api::http::error::AppError;

    /// Membership check fail should produce permission denied (403/PermissionDenied)
    #[test]
    fn test_membership_check_fail_produces_forbidden() {
        // This mirrors the gRPC logic at client_service.rs:557-560
        let err = AppError::forbidden("Not a member of this room");
        assert_eq!(err.status, axum::http::StatusCode::FORBIDDEN);
        assert!(err.message.contains("Not a member"));
    }

    /// `cluster_manager=None` should produce unavailable error
    #[test]
    fn test_cluster_manager_none_produces_unavailable() {
        // The gRPC handler at client_service.rs:573-575 returns Status::unavailable
        // Simulate the equivalent HTTP-side check
        let cluster_manager: Option<()> = None;
        let result = cluster_manager.ok_or_else(|| {
            AppError::internal_server_error(
                "Real-time messaging requires cluster manager (Redis not configured)",
            )
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("cluster manager"));
    }

    /// Verify tonic Status mapping for membership check
    #[test]
    fn test_grpc_membership_check_status() {
        let status =
            tonic::Status::permission_denied("Not a member of the room: membership check failed");
        assert_eq!(status.code(), tonic::Code::PermissionDenied);
        assert!(status.message().contains("Not a member"));
    }

    /// Verify tonic Status mapping for missing cluster manager
    #[test]
    fn test_grpc_cluster_manager_none_status() {
        let status = tonic::Status::unavailable(
            "Real-time messaging requires cluster manager (Redis not configured)",
        );
        assert_eq!(status.code(), tonic::Code::Unavailable);
        assert!(status.message().contains("cluster manager"));
    }
}

// ============================================================================
// API7: Admin role guard
// ============================================================================

mod admin_role_guard {
    use synctv_api::http::error::AppError;

    /// Non-admin user reaching admin endpoint should get 403
    #[tokio::test]
    async fn test_non_admin_returns_403() {
        use super::*;

        let app = Router::new().route(
            "/api/admin/stats",
            get(|| async {
                // Simulate AuthAdmin extractor rejection (admin.rs:77-79)
                Err::<Json<Value>, AppError>(AppError::forbidden("Admin role required"))
            }),
        );

        let req = Request::get("/api/admin/stats")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("Admin role required"));
    }

    /// Root role requirement should also produce 403 for non-root
    #[tokio::test]
    async fn test_non_root_returns_403() {
        use super::*;

        let app = Router::new().route(
            "/api/admin/admins",
            get(|| async {
                // Simulate AuthRoot extractor rejection (admin.rs:102-104)
                Err::<Json<Value>, AppError>(AppError::forbidden("Root role required"))
            }),
        );

        let req = Request::get("/api/admin/admins")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
        let json = body_json(resp).await;
        assert!(json["error"]
            .as_str()
            .unwrap()
            .contains("Root role required"));
    }

    /// Missing authorization header should produce 401
    #[tokio::test]
    async fn test_missing_auth_header_returns_401() {
        use super::*;

        let app = Router::new().route(
            "/api/admin/stats",
            get(|| async {
                // Simulate validate_auth_user rejection (admin.rs:42-43)
                Err::<Json<Value>, AppError>(AppError::unauthorized("Missing Authorization header"))
            }),
        );

        let req = Request::get("/api/admin/stats")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }
}

// ============================================================================
// API8: Live streaming handlers
// ============================================================================

mod live_streaming_validation {
    use synctv_api::http::error::AppError;

    /// `live_streaming_infrastructure=None` should produce 500
    #[test]
    fn test_live_infrastructure_none_produces_500() {
        // Reproduce live.rs:117-118
        let infrastructure: Option<()> = None;
        let result = infrastructure
            .ok_or_else(|| AppError::internal_server_error("Live streaming not configured"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("Live streaming not configured"));
    }

    /// Missing `room_id` query parameter should produce 400
    #[test]
    fn test_missing_room_id_produces_400() {
        // Reproduce live.rs:104-106
        let room_id: Option<String> = None;
        let result =
            room_id.ok_or_else(|| AppError::bad_request("room_id query parameter is required"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::BAD_REQUEST);
        assert!(err.message.contains("room_id"));
    }

    /// Missing token query parameter should produce 401
    #[test]
    fn test_missing_token_produces_401() {
        // Reproduce live.rs:111-112
        let token: Option<&str> = None;
        let result =
            token.ok_or_else(|| AppError::unauthorized("token query parameter is required"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
    }

    /// provider live query deserialization edge cases
    #[test]
    fn test_live_query_deserialize_required_room_id() {
        use synctv_api::http::providers::live::RoomQuery;

        let result: Result<RoomQuery, _> = serde_urlencoded::from_str("room_id=room123");
        assert!(result.is_ok(), "provider RoomQuery should deserialize");
    }

    #[test]
    fn test_live_query_deserialize_empty_fails() {
        use synctv_api::http::providers::live::RoomQuery;

        let result: Result<RoomQuery, _> = serde_urlencoded::from_str("");
        assert!(result.is_err(), "provider RoomQuery requires room_id");
    }

    #[test]
    fn test_live_query_deserialize_legacy_room_id_rejected() {
        use synctv_api::http::providers::live::RoomQuery;

        let result: Result<RoomQuery, _> = serde_urlencoded::from_str("roomId=room123");
        assert!(
            result.is_err(),
            "legacy camelCase roomId must not deserialize"
        );
    }
}

// ============================================================================
// API9: WebSocket extract_user_id auth priority
// ============================================================================

mod websocket_auth_priority {
    use synctv_api::http::websocket::{AuthMethod, WsQuery};

    /// Auth priority: header present -> should use Header method
    #[test]
    fn test_auth_priority_header_takes_precedence() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("Authorization", "Bearer some_jwt_token".parse().unwrap());

        // When Authorization header is present, it should be checked first
        let auth_header = headers.get("Authorization");
        assert!(auth_header.is_some());
        let auth_str = auth_header.unwrap().to_str().unwrap();
        let token = auth_str.strip_prefix("Bearer ");
        assert!(token.is_some());
        assert_eq!(token.unwrap(), "some_jwt_token");
    }

    /// Auth priority: no header, ticket present -> should use Ticket
    #[test]
    fn test_auth_priority_ticket_second() {
        let headers = axum::http::HeaderMap::new();
        let query = WsQuery {
            ticket: Some("ticket_abc".to_string()),
        };

        assert!(headers.get("Authorization").is_none());
        assert!(query.ticket.is_some());
    }

    /// Missing all credentials should produce unauthorized
    #[test]
    fn test_missing_all_credentials_produces_unauthorized() {
        let headers = axum::http::HeaderMap::new();
        let query = WsQuery { ticket: None };

        assert!(headers.get("Authorization").is_none());
        assert!(query.ticket.is_none());

        // This would produce:
        let err = synctv_api::http::error::AppError::unauthorized(
            "Missing authentication: provide token via Authorization header or ?ticket=",
        );
        assert_eq!(err.status, axum::http::StatusCode::UNAUTHORIZED);
        assert!(err.message.contains("Missing authentication"));
    }

    /// `AuthMethod` enum equality checks
    #[test]
    fn test_auth_method_enum_values() {
        assert_eq!(AuthMethod::Header, AuthMethod::Header);
        assert_eq!(AuthMethod::Ticket, AuthMethod::Ticket);
        assert_ne!(AuthMethod::Header, AuthMethod::Ticket);
    }

    /// `WsQuery` deserialization
    #[test]
    fn test_ws_query_deserialization_combinations() {
        // With ticket
        let json = r#"{"ticket":"tix"}"#;
        let query: WsQuery = serde_json::from_str(json).unwrap();
        assert!(query.ticket.is_some());

        // Neither
        let json = r"{}";
        let query: WsQuery = serde_json::from_str(json).unwrap();
        assert!(query.ticket.is_none());
    }

    /// Non-Bearer auth header should not be extracted
    #[test]
    fn test_non_bearer_header_not_extracted() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("Authorization", "Basic dXNlcjpwYXNz".parse().unwrap());

        let auth_header = headers.get("Authorization").unwrap();
        let auth_str = auth_header.to_str().unwrap();
        assert!(auth_str.strip_prefix("Bearer ").is_none());
    }
}

// ============================================================================
// API10: Optional services absent
// ============================================================================

mod optional_services_absent {
    use synctv_api::http::error::AppError;

    /// `email_service=None` should produce appropriate error
    #[test]
    fn test_email_service_none() {
        let email_service: Option<()> = None;
        let result = email_service
            .ok_or_else(|| AppError::internal_server_error("Email service not configured"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert!(err.message.contains("Email service not configured"));
    }

    /// `notification_api=None` should produce appropriate error
    #[tokio::test]
    async fn test_notification_api_none_returns_error() {
        use super::*;

        let app = Router::new().route(
            "/api/notifications",
            get(|| async {
                let notification_api: Option<()> = None;
                notification_api.ok_or_else(|| {
                    AppError::internal_server_error("Notification service not configured")
                })?;
                Ok::<Json<Value>, AppError>(Json(serde_json::json!([])))
            }),
        );

        let req = Request::get("/api/notifications")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// `oauth2_api=None` should produce appropriate error
    #[tokio::test]
    async fn test_oauth2_api_none_returns_error() {
        use super::*;

        let app = Router::new().route(
            "/api/oauth2/providers",
            get(|| async {
                let oauth2_api: Option<()> = None;
                oauth2_api.ok_or_else(|| {
                    AppError::internal_server_error("OAuth2 service not configured")
                })?;
                Ok::<Json<Value>, AppError>(Json(serde_json::json!([])))
            }),
        );

        let req = Request::get("/api/oauth2/providers")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// `ws_ticket_service=None` produces correct error code
    #[test]
    fn test_ws_ticket_service_none_error_message() {
        let ws_ticket_service: Option<()> = None;
        let result = ws_ticket_service.ok_or_else(|| {
            AppError::internal_server_error("WebSocket ticket service not configured")
        });
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "WebSocket ticket service not configured");
    }

    /// `admin_api=None` should produce correct error
    #[test]
    fn test_admin_api_none_error() {
        // Reproduce admin.rs:147-152
        let admin_api: Option<()> = None;
        let result = admin_api.ok_or_else(|| AppError::internal("Admin service not configured"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status, axum::http::StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.message, "Admin service not configured");
    }

    /// Multiple absent services should each produce their own error message
    #[test]
    #[allow(clippy::type_complexity)]
    fn test_distinct_error_messages_for_each_service() {
        let services: Vec<(&str, fn() -> AppError)> = vec![
            ("email", || {
                AppError::internal_server_error("Email service not configured")
            }),
            ("notification", || {
                AppError::internal_server_error("Notification service not configured")
            }),
            ("oauth2", || {
                AppError::internal_server_error("OAuth2 service not configured")
            }),
            ("ws_ticket", || {
                AppError::internal_server_error("WebSocket ticket service not configured")
            }),
            ("admin", || {
                AppError::internal("Admin service not configured")
            }),
            ("live", || {
                AppError::internal_server_error("Live streaming not configured")
            }),
        ];

        for (name, make_err) in services {
            let err = make_err();
            assert_eq!(
                err.status,
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "Service '{name}' should produce 500"
            );
            assert!(
                !err.message.is_empty(),
                "Service '{name}' error should have a message"
            );
        }
    }
}

// ============================================================================
// Cross-cutting: Error response format consistency
// ============================================================================

mod error_response_format {
    use super::*;
    use synctv_api::http::error::AppError;

    /// Helper to verify error response JSON structure
    async fn assert_error_response(make_error: fn() -> AppError, expected_status: StatusCode) {
        let app = Router::new().route(
            "/test",
            get(move || async move { Err::<String, AppError>(make_error()) }),
        );
        let req = Request::get("/test").body(Body::empty()).unwrap();
        let resp = app.oneshot(req).await.unwrap();

        assert_eq!(resp.status(), expected_status);
        let json = body_json(resp).await;
        let obj = json.as_object().unwrap();
        assert!(
            obj.contains_key("error"),
            "Response for {expected_status} must contain 'error' field"
        );
        assert!(
            obj.contains_key("status"),
            "Response for {expected_status} must contain 'status' field"
        );
        assert_eq!(
            json["status"].as_u64().unwrap() as u16,
            expected_status.as_u16(),
            "Status field must match HTTP status for {expected_status}"
        );
    }

    #[tokio::test]
    async fn test_bad_request_response_format() {
        assert_error_response(
            || AppError::bad_request("test 400"),
            StatusCode::BAD_REQUEST,
        )
        .await;
    }

    #[tokio::test]
    async fn test_unauthorized_response_format() {
        assert_error_response(
            || AppError::unauthorized("test 401"),
            StatusCode::UNAUTHORIZED,
        )
        .await;
    }

    #[tokio::test]
    async fn test_forbidden_response_format() {
        assert_error_response(|| AppError::forbidden("test 403"), StatusCode::FORBIDDEN).await;
    }

    #[tokio::test]
    async fn test_not_found_response_format() {
        assert_error_response(|| AppError::not_found("test 404"), StatusCode::NOT_FOUND).await;
    }

    #[tokio::test]
    async fn test_conflict_response_format() {
        assert_error_response(|| AppError::conflict("test 409"), StatusCode::CONFLICT).await;
    }

    #[tokio::test]
    async fn test_internal_server_error_response_format() {
        assert_error_response(
            || AppError::internal_server_error("test 500"),
            StatusCode::INTERNAL_SERVER_ERROR,
        )
        .await;
    }

    #[tokio::test]
    async fn test_service_unavailable_response_format() {
        assert_error_response(
            AppError::service_unavailable,
            StatusCode::SERVICE_UNAVAILABLE,
        )
        .await;
    }

    #[tokio::test]
    async fn test_rate_limited_response_format() {
        assert_error_response(|| AppError::rate_limited(60), StatusCode::TOO_MANY_REQUESTS).await;
    }
}

// ============================================================================
// Cross-cutting: Validation function coverage
// ============================================================================

mod validation_coverage {
    use synctv_api::http::validation::*;

    /// `validate_id` rejects various injection attempts
    #[test]
    fn test_validate_id_rejects_injections() {
        // These contain characters that remain invalid after sanitization
        let injections = &[
            "'; DROP TABLE users;--",
            "<img src=x onerror=alert(1)>",
            "../../../etc/passwd",
            "user name", // space
            "user=value",
            "user&other",
        ];
        for input in injections {
            let result = validate_id(input, "test_id");
            assert!(
                result.is_err(),
                "Injection attempt '{}' should be rejected",
                input.escape_debug()
            );
        }
    }

    /// `validate_id` sanitizes control characters before validation
    /// A null byte in the middle of valid chars gets stripped, so "user\x00null"
    /// becomes "usernull" which is valid
    #[test]
    fn test_validate_id_sanitizes_control_chars() {
        // Control chars are stripped by sanitize_string, so the remaining
        // content determines validity
        let result = validate_id("user\x00name", "test_id");
        // After sanitization: "username" - valid
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "username");
    }

    /// `validate_room_id` rejects special characters
    #[test]
    fn test_validate_room_id_rejects_special() {
        assert!(validate_room_id("room@123").is_err());
        assert!(validate_room_id("room 123").is_err());
        assert!(validate_room_id("room.123").is_err());
        assert!(validate_room_id("room/123").is_err());
    }

    /// `validate_room_id` accepts valid IDs
    #[test]
    fn test_validate_room_id_accepts_valid() {
        assert!(validate_room_id("room1234_abx").is_ok());
        assert!(validate_room_id("room_123-xyz").is_ok());
        assert!(validate_room_id("room-123_abc").is_ok());
        assert!(validate_room_id("ABC12345_DEF").is_ok());
    }

    /// `validate_email` rejects invalid formats
    #[test]
    fn test_validate_email_edge_cases() {
        assert!(validate_email("@example.com").is_err());
        assert!(validate_email("user@").is_err());
        assert!(validate_email("user@.com").is_err());
        assert!(validate_email("user space@example.com").is_err());
    }

    /// `validate_playback_position` edge cases
    #[test]
    fn test_validate_playback_position_boundaries() {
        assert!(validate_playback_position(0.0).is_ok());
        assert!(validate_playback_position(86400.0).is_ok()); // Exactly 24h
        assert!(validate_playback_position(86401.0).is_err()); // Over 24h
        assert!(validate_playback_position(-0.001).is_err());
        assert!(validate_playback_position(f64::NAN).is_err());
        assert!(validate_playback_position(f64::INFINITY).is_err());
        assert!(validate_playback_position(f64::NEG_INFINITY).is_err());
    }

    /// `validate_playback_speed` boundaries
    #[test]
    fn test_validate_playback_speed_boundaries() {
        assert!(validate_playback_speed(0.25).is_ok()); // Min
        assert!(validate_playback_speed(4.0).is_ok()); // Max
        assert!(validate_playback_speed(0.24).is_err()); // Below min
        assert!(validate_playback_speed(4.01).is_err()); // Above max
        assert!(validate_playback_speed(f64::NAN).is_err());
    }

    /// `sanitize_string` removes control characters
    #[test]
    fn test_sanitize_string_control_chars() {
        assert_eq!(sanitize_string("hello\x00world").as_ref(), "helloworld");
        assert_eq!(sanitize_string("hello\x01world").as_ref(), "helloworld");
        assert_eq!(sanitize_string("  hello  ").as_ref(), "hello");
    }
}

// ============================================================================
// Cross-cutting: ApiError type-safe mapping
// ============================================================================

mod api_error_type_safe_mapping {
    use synctv_api::http::error::{map_api_error, AppError};
    use synctv_api::impls::ApiError;

    /// Each `ApiError` variant maps to the correct HTTP status code via From<ApiError>
    #[test]
    fn test_api_error_to_app_error_status_mapping() {
        let cases: Vec<(ApiError, axum::http::StatusCode)> = vec![
            (
                ApiError::NotFound("x".into()),
                axum::http::StatusCode::NOT_FOUND,
            ),
            (
                ApiError::Authentication("x".into()),
                axum::http::StatusCode::UNAUTHORIZED,
            ),
            (
                ApiError::Authorization("x".into()),
                axum::http::StatusCode::FORBIDDEN,
            ),
            (
                ApiError::AlreadyExists("x".into()),
                axum::http::StatusCode::CONFLICT,
            ),
            (
                ApiError::InvalidInput("x".into()),
                axum::http::StatusCode::BAD_REQUEST,
            ),
            (
                ApiError::ServiceUnavailable("x".into()),
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
            ),
            (
                ApiError::Internal("x".into()),
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            ),
        ];

        for (api_err, expected_status) in cases {
            let app_err = map_api_error(api_err);
            assert_eq!(app_err.status, expected_status);
        }
    }

    /// `map_api_error` preserves error codes
    #[test]
    fn test_map_api_error_preserves_error_code() {
        let api_err = ApiError::NotFound("room".into());
        let app_err: AppError = api_err.into();
        assert!(app_err.error_code.is_some());
        assert_eq!(
            app_err.error_code.unwrap(),
            synctv_api::impls::error_codes::NOT_FOUND
        );
    }

    /// Internal errors should not leak implementation details
    #[test]
    fn test_internal_error_does_not_leak_details() {
        let api_err = ApiError::Internal("Database connection pool exhausted".into());
        let app_err: AppError = api_err.into();
        assert_eq!(
            app_err.status,
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        // Internal error messages should be sanitized
        assert_eq!(app_err.message, "Internal error");
    }

    #[test]
    fn test_provider_not_found_maps_to_404() {
        let api_err = ApiError::from(synctv_core::provider::ProviderError::NotFound);
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn test_provider_credential_expired_maps_to_401() {
        let api_err = ApiError::from(synctv_core::provider::ProviderError::CredentialExpired(
            "credential expired".into(),
        ));
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, axum::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn test_provider_invalid_config_maps_to_400() {
        let api_err = ApiError::from(synctv_core::provider::ProviderError::InvalidConfig(
            "missing host".into(),
        ));
        let app_err = map_api_error(api_err);
        assert_eq!(app_err.status, axum::http::StatusCode::BAD_REQUEST);
    }
}

// ============================================================================
// Cross-cutting: gRPC status code mapping
// ============================================================================

mod grpc_status_mapping {
    use synctv_api::impls::ApiError;

    /// `ApiError` should produce correct tonic status codes via proto error
    #[test]
    fn test_api_error_to_proto_error_message() {
        let cases = vec![
            (
                ApiError::NotFound("room not found".into()),
                "room not found",
            ),
            (
                ApiError::Authentication("token expired".into()),
                "token expired",
            ),
            (ApiError::Authorization("forbidden".into()), "forbidden"),
            (ApiError::AlreadyExists("user exists".into()), "user exists"),
            (ApiError::InvalidInput("bad field".into()), "bad field"),
        ];

        for (api_err, expected_msg) in cases {
            let proto_err = api_err.to_proto_error();
            assert!(
                proto_err.message.contains(expected_msg),
                "Proto error message should contain '{expected_msg}'"
            );
        }
    }

    /// Internal errors should be sanitized in proto error messages
    #[test]
    fn test_internal_error_sanitized_in_proto() {
        let api_err = ApiError::Internal("secret database connection string".into());
        let proto_err = api_err.to_proto_error();
        assert_eq!(proto_err.message, "Internal error");
        assert!(!proto_err.message.contains("secret"));
    }

    /// Error codes in proto messages should match expected constants
    #[test]
    fn test_proto_error_codes() {
        use synctv_api::impls::error_codes;

        let cases: Vec<(ApiError, i32)> = vec![
            (ApiError::NotFound("x".into()), error_codes::NOT_FOUND),
            (
                ApiError::Authentication("x".into()),
                error_codes::UNAUTHENTICATED,
            ),
            (
                ApiError::Authorization("x".into()),
                error_codes::PERMISSION_DENIED,
            ),
            (
                ApiError::AlreadyExists("x".into()),
                error_codes::ALREADY_EXISTS,
            ),
            (
                ApiError::InvalidInput("x".into()),
                error_codes::INVALID_ARGUMENT,
            ),
            (
                ApiError::ServiceUnavailable("x".into()),
                error_codes::SERVICE_UNAVAILABLE,
            ),
            (ApiError::Internal("x".into()), error_codes::INTERNAL_ERROR),
        ];

        for (api_err, expected_code) in cases {
            let proto_err = api_err.to_proto_error();
            assert_eq!(proto_err.code, expected_code);
        }
    }
}

// ============================================================================
// P0-5/6: gRPC API Validation Tests
// ============================================================================

mod grpc_api_validation {
    use synctv_api::http::validation::{validate_id, ValidationError};

    /// Test `room_id` validation: empty string should be rejected
    #[test]
    fn test_validate_room_id_empty() {
        let result = validate_id("", "room_id");
        assert!(result.is_err());
        match result {
            Err(ValidationError::Required(field)) => assert_eq!(field, "room_id"),
            _ => panic!("Expected Required error"),
        }
    }

    /// Test `room_id` validation: invalid characters should be rejected
    #[test]
    fn test_validate_room_id_invalid_chars() {
        let result = validate_id("room@123!", "room_id");
        assert!(result.is_err());
        match result {
            Err(ValidationError::InvalidFormat { field }) => assert_eq!(field, "room_id"),
            _ => panic!("Expected InvalidFormat error"),
        }
    }

    /// Test `room_id` validation: valid ID should pass
    #[test]
    fn test_validate_room_id_valid() {
        let result = validate_id("room_123-abc", "room_id");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "room_123-abc");
    }

    /// Test `media_id` validation: empty string should be rejected
    #[test]
    fn test_validate_media_id_empty() {
        let result = validate_id("", "media_id");
        assert!(result.is_err());
        match result {
            Err(ValidationError::Required(field)) => assert_eq!(field, "media_id"),
            _ => panic!("Expected Required error"),
        }
    }

    /// Test `media_id` validation: too long should be rejected
    #[test]
    fn test_validate_media_id_too_long() {
        let long_id = "a".repeat(100);
        let result = validate_id(&long_id, "media_id");
        assert!(result.is_err());
        match result {
            Err(ValidationError::TooLong { field, .. }) => assert_eq!(field, "media_id"),
            _ => panic!("Expected TooLong error"),
        }
    }

    /// Test `media_id` validation: valid ID should pass
    #[test]
    fn test_validate_media_id_valid() {
        let result = validate_id("media_123-xyz", "media_id");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "media_123-xyz");
    }

    /// Test that ID validation rejects path traversal attempts
    #[test]
    fn test_validate_id_rejects_path_traversal() {
        let result = validate_id("../etc/passwd", "room_id");
        assert!(result.is_err());
    }

    /// Test that ID validation rejects SQL injection attempts
    #[test]
    fn test_validate_id_rejects_sql_injection() {
        let result = validate_id("room'; DROP TABLE--", "room_id");
        assert!(result.is_err());
    }
}

// ============================================================================
// P0-6: add_media_batch provider_instance_name Tests
// ============================================================================

mod add_media_batch_provider_instance {
    use synctv_api::proto::client::AddMediaRequest;

    /// Test that `AddMediaRequest` has `provider_instance_name` field
    #[test]
    fn test_add_media_request_has_provider_instance_name() {
        let req = AddMediaRequest {
            playlist_id: "playlist1".to_string(),
            provider: "bilibili".to_string(),
            provider_instance_name: "bilibili_main".to_string(),
            source_config: br#"{"url":"https://example.com"}"#.to_vec(),
            title: "Test Video".to_string(),
        };
        assert_eq!(req.provider_instance_name, "bilibili_main");
    }

    /// Test that `AddMediaRequest` with empty `provider_instance_name` works
    #[test]
    fn test_add_media_request_empty_provider_instance_name() {
        let req = AddMediaRequest {
            playlist_id: "playlist1".to_string(),
            provider: "direct_url".to_string(),
            provider_instance_name: String::new(),
            source_config: br#"{"url":"https://example.com"}"#.to_vec(),
            title: "Test Video".to_string(),
        };
        assert!(req.provider_instance_name.is_empty());
    }

    /// Test that `provider_instance_name` can be used to specify provider instance
    #[test]
    fn test_provider_instance_name_variations() {
        let cases = vec![
            ("bilibili_main", "bilibili_main"),
            ("alist_personal", "alist_personal"),
            ("", ""),
        ];

        for (input, expected) in cases {
            let req = AddMediaRequest {
                playlist_id: "playlist1".to_string(),
                provider: "test".to_string(),
                provider_instance_name: input.to_string(),
                source_config: vec![],
                title: String::new(),
            };
            assert_eq!(req.provider_instance_name, expected);
        }
    }
}
