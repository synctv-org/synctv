//! Integration tests for synctv-core services
//!
//! These tests verify end-to-end functionality across multiple service layers.
//!
//! Run with: cargo test --test `integration_tests`
#![allow(clippy::unwrap_used)]

use synctv_core::{models::UserId, service::auth::TokenType};
use synctv_core_testing::create_test_jwt_service;

/// Helper to create a test JWT service with a test secret
#[tokio::test]
async fn test_user_registration_and_authentication() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate access token (role is intentionally NOT in JWT claims;
    // it's fetched from the database on each request for security)
    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    let claims = jwt_service.verify_access_token(&access_token).unwrap();
    assert_eq!(claims.sub, user_id.as_str());
    assert!(claims.is_access_token());

    // Generate refresh token
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .unwrap();

    let claims = jwt_service.verify_refresh_token(&refresh_token).unwrap();
    assert_eq!(claims.sub, user_id.as_str());
    assert!(claims.is_refresh_token());
}

#[tokio::test]
async fn test_jwt_token_expiration() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    let claims = jwt_service.verify_token(&token).unwrap();
    assert!(claims.exp > claims.iat);

    // For access tokens (1 hour)
    let expected_exp = claims.iat + 3600;
    assert_eq!(claims.exp, expected_exp);
}

#[tokio::test]
async fn test_jwt_invalid_token() {
    let jwt_service = create_test_jwt_service();

    // Invalid token format
    let result = jwt_service.verify_token("invalid.token");
    assert!(result.is_err());

    // Tampered token
    let user_id = UserId::new();
    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    let parts: Vec<&str> = token.split('.').collect();
    let tampered_token = format!("{}.{}.tampered", parts[0], parts[1]);

    let result = jwt_service.verify_token(&tampered_token);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_error_handling() {
    use synctv_core::Error;

    let auth_error = Error::Authentication("Invalid token".to_string());
    assert!(matches!(auth_error, Error::Authentication(_)));

    let not_found_error = Error::NotFound("User not found".to_string());
    assert!(matches!(not_found_error, Error::NotFound(_)));

    let error_msg = format!("{auth_error}");
    assert!(error_msg.contains("Invalid token"));
}

#[tokio::test]
async fn test_concurrent_operations() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    let counter = Arc::new(AtomicUsize::new(0));
    let mut handles = vec![];

    for _ in 0..10 {
        let counter = counter.clone();
        let handle = tokio::spawn(async move {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap();
    }

    assert_eq!(counter.load(Ordering::SeqCst), 10);
}

// ==================== Publish Key Service Tests ====================

#[tokio::test]
async fn test_publish_key_generation_and_validation() {
    use synctv_core::models::{MediaId, RoomId};
    use synctv_core::service::PublishKeyService;

    let jwt_service = create_test_jwt_service();
    let publish_key_service = PublishKeyService::with_default_ttl(jwt_service);

    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = publish_key_service
        .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
        .await
        .expect("Failed to generate publish key");

    assert_eq!(key.room_id, room_id.as_str());
    assert_eq!(key.media_id, media_id.as_str());
    assert_eq!(key.user_id, user_id.as_str());
    assert!(key.expires_at > 0);
    assert!(!key.token.is_empty());

    let claims = publish_key_service
        .validate_publish_key(&key.token)
        .await
        .expect("Failed to validate publish key");

    assert_eq!(claims.room_id, room_id.as_str());
    assert_eq!(claims.media_id, media_id.as_str());
    assert_eq!(claims.user_id, user_id.as_str());
    assert!(claims.perm_start_live);
}

#[tokio::test]
async fn test_publish_key_room_media_verification() {
    use synctv_core::models::{MediaId, RoomId};
    use synctv_core::service::PublishKeyService;

    let jwt_service = create_test_jwt_service();
    let publish_key_service = PublishKeyService::with_default_ttl(jwt_service);

    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    let key = publish_key_service
        .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
        .await
        .unwrap();

    // Verify for correct room/media
    let result = publish_key_service
        .verify_publish_key_for_stream(&key.token, &room_id, &media_id)
        .await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().as_str(), user_id.as_str());

    // Verify for wrong room
    let wrong_room = RoomId::new();
    let result = publish_key_service
        .verify_publish_key_for_stream(&key.token, &wrong_room, &media_id)
        .await;
    assert!(result.is_err());

    // Verify for wrong media
    let wrong_media = MediaId::new();
    let result = publish_key_service
        .verify_publish_key_for_stream(&key.token, &room_id, &wrong_media)
        .await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_publish_key_invalid_token() {
    use synctv_core::service::PublishKeyService;

    let jwt_service = create_test_jwt_service();
    let publish_key_service = PublishKeyService::with_default_ttl(jwt_service);

    let result = publish_key_service
        .validate_publish_key("invalid.token.here")
        .await;
    assert!(result.is_err());
}

// ==================== Permission Bitmask Tests ====================

#[test]
fn test_permission_bits_operations() {
    use synctv_core::models::PermissionBits;

    let mut perms = PermissionBits(0);
    assert!(!perms.has(PermissionBits::SEND_CHAT));

    perms.grant(PermissionBits::SEND_CHAT);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(!perms.has(PermissionBits::ADD_MEDIA));

    perms.grant(PermissionBits::ADD_MEDIA);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MEDIA));

    perms.revoke(PermissionBits::SEND_CHAT);
    assert!(!perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MEDIA));
}

#[test]
fn test_permission_default_roles() {
    use synctv_core::models::PermissionBits;

    let member = PermissionBits(PermissionBits::DEFAULT_MEMBER);
    assert!(member.has(PermissionBits::SEND_CHAT));
    assert!(member.has(PermissionBits::ADD_MEDIA));
    assert!(!member.has(PermissionBits::MANAGE_ADMIN));

    let admin = PermissionBits(PermissionBits::DEFAULT_ADMIN);
    assert!(admin.has(PermissionBits::SEND_CHAT));
    assert!(admin.has(PermissionBits::KICK_MEMBER));
    assert!(admin.has(PermissionBits::SET_ROOM_SETTINGS));

    let guest = PermissionBits(PermissionBits::DEFAULT_GUEST);
    assert!(guest.has(PermissionBits::VIEW_PLAYLIST));
    assert!(!guest.has(PermissionBits::SEND_CHAT));

    let none = PermissionBits(PermissionBits::NONE);
    assert!(!none.has(PermissionBits::SEND_CHAT));
    assert!(!none.has(PermissionBits::VIEW_PLAYLIST));

    let all = PermissionBits(PermissionBits::ALL);
    assert!(all.has(PermissionBits::SEND_CHAT));
    assert!(all.has(PermissionBits::MANAGE_ADMIN));
    assert!(all.has(PermissionBits::DELETE_ROOM));
}

// ==================== Playlist Model Tests ====================

#[test]
fn test_playlist_model() {
    use synctv_core::models::{Playlist, PlaylistId, RoomId};

    let root = Playlist {
        id: PlaylistId::new(),
        room_id: RoomId::new(),
        creator_id: Some(UserId::new()),
        name: String::new(),
        parent_id: None,
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };
    assert!(root.is_root());
    assert!(!root.is_dynamic());
    assert!(root.is_static());

    let folder = Playlist {
        id: PlaylistId::new(),
        room_id: RoomId::new(),
        creator_id: Some(UserId::new()),
        name: "My Folder".to_string(),
        parent_id: Some(root.id.clone()),
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };
    assert!(!folder.is_root());
    assert!(!folder.is_dynamic());
    assert!(folder.is_static());

    let dynamic = Playlist {
        id: PlaylistId::new(),
        room_id: RoomId::new(),
        creator_id: Some(UserId::new()),
        name: "Alist Folder".to_string(),
        parent_id: Some(root.id),
        position: 1,
        source_provider: Some("alist".to_string()),
        source_config: Some(serde_json::json!({"url": "http://example.com"})),
        provider_instance_name: Some("my_alist".to_string()),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };
    assert!(!dynamic.is_root());
    assert!(dynamic.is_dynamic());
    assert!(!dynamic.is_static());
}

// ==================== ID Model Tests ====================

#[test]
fn test_id_generation_uniqueness() {
    use synctv_core::models::{MediaId, PlaylistId, RoomId};

    let id1 = RoomId::new();
    let id2 = RoomId::new();
    assert_ne!(id1.as_str(), id2.as_str());

    let mid1 = MediaId::new();
    let mid2 = MediaId::new();
    assert_ne!(mid1.as_str(), mid2.as_str());

    let pid1 = PlaylistId::new();
    let pid2 = PlaylistId::new();
    assert_ne!(pid1.as_str(), pid2.as_str());

    let room_id = RoomId::from_string("test_room_123".to_string());
    assert_eq!(room_id.as_str(), "test_room_123");
}

// ==================== JWT Token Tests ====================
// Note: Role is intentionally NOT stored in JWT claims (security design).
// Role is fetched from the database on every request to ensure immediate
// propagation of role changes.

#[tokio::test]
async fn test_jwt_token_types() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Generate access token
    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    let claims = jwt_service.verify_access_token(&access_token).unwrap();
    assert_eq!(claims.sub, user_id.as_str());
    assert!(claims.is_access_token());

    // Generate refresh token
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .unwrap();

    let claims = jwt_service.verify_refresh_token(&refresh_token).unwrap();
    assert_eq!(claims.sub, user_id.as_str());
    assert!(claims.is_refresh_token());
}

#[tokio::test]
async fn test_jwt_access_and_refresh_tokens_different() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();
    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .unwrap();

    // Tokens should be different
    assert_ne!(access_token, refresh_token);

    // Access token should not validate as refresh
    let result = jwt_service.verify_refresh_token(&access_token);
    assert!(result.is_err());

    // Refresh token should not validate as access
    let result = jwt_service.verify_access_token(&refresh_token);
    assert!(result.is_err());
}

// ==================== Error Type Tests ====================

#[test]
fn test_error_types() {
    use synctv_core::Error;

    let errors = vec![
        Error::Authentication("auth failed".to_string()),
        Error::Authorization("not authorized".to_string()),
        Error::NotFound("resource missing".to_string()),
        Error::InvalidInput("bad input".to_string()),
        Error::Authorization("access denied".to_string()),
        Error::Internal("internal error".to_string()),
    ];

    for error in &errors {
        let msg = format!("{error}");
        assert!(!msg.is_empty());
    }

    assert!(matches!(errors[0], Error::Authentication(_)));
    assert!(matches!(errors[1], Error::Authorization(_)));
    assert!(matches!(errors[2], Error::NotFound(_)));
    assert!(matches!(errors[3], Error::InvalidInput(_)));
    assert!(matches!(errors[4], Error::Authorization(_)));
    assert!(matches!(errors[5], Error::Internal(_)));
}

// ==================== End-to-End Test Suite ====================

#[tokio::test]
async fn test_e2e_user_auth_flow() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    // Step 1: User logs in and gets access + refresh tokens
    let access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to generate access token");

    let refresh_token = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .expect("Failed to generate refresh token");

    // Step 2: Verify access token for API requests
    let access_claims = jwt_service
        .verify_access_token(&access_token)
        .expect("Failed to verify access token");

    assert_eq!(access_claims.sub, user_id.as_str());
    assert!(access_claims.is_access_token());

    // Step 3: Access token expired (simulated), use refresh token
    let refresh_claims = jwt_service
        .verify_refresh_token(&refresh_token)
        .expect("Failed to verify refresh token");

    assert_eq!(refresh_claims.sub, user_id.as_str());
    assert!(refresh_claims.is_refresh_token());

    // Step 4: Generate new access token using refresh token
    let new_access_token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .expect("Failed to generate new access token");

    let new_claims = jwt_service
        .verify_access_token(&new_access_token)
        .expect("Failed to verify new access token");

    assert_eq!(new_claims.sub, user_id.as_str());
    assert!(new_claims.exp > new_claims.iat);
}

#[tokio::test]
async fn test_e2e_publish_key_workflow() {
    use synctv_core::models::{MediaId, RoomId};
    use synctv_core::service::PublishKeyService;

    let jwt_service = create_test_jwt_service();
    let publish_key_service = PublishKeyService::with_default_ttl(jwt_service);

    let room_id = RoomId::new();
    let media_id = MediaId::new();
    let user_id = UserId::new();

    // Test 1: validate_publish_key
    let publish_key = publish_key_service
        .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
        .await
        .expect("Failed to generate publish key");

    assert_eq!(publish_key.room_id, room_id.as_str());
    assert_eq!(publish_key.media_id, media_id.as_str());
    assert_eq!(publish_key.user_id, user_id.as_str());
    assert!(!publish_key.token.is_empty());

    let claims = publish_key_service
        .validate_publish_key(&publish_key.token)
        .await
        .expect("Failed to validate publish key");

    assert_eq!(claims.room_id, room_id.as_str());
    assert_eq!(claims.media_id, media_id.as_str());
    assert!(claims.perm_start_live);

    // Test 2: verify_publish_key_for_stream (requires a new token since publish keys are single-use)
    let publish_key2 = publish_key_service
        .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
        .await
        .expect("Failed to generate second publish key");

    let verified_user_id = publish_key_service
        .verify_publish_key_for_stream(&publish_key2.token, &room_id, &media_id)
        .await
        .expect("Failed to verify publish key for stream");

    assert_eq!(verified_user_id.as_str(), user_id.as_str());

    // Test 3: verify that room mismatch fails
    let wrong_room = RoomId::new();
    let publish_key3 = publish_key_service
        .generate_publish_key(room_id.clone(), media_id.clone(), user_id.clone())
        .await
        .expect("Failed to generate third publish key");
    let result = publish_key_service
        .verify_publish_key_for_stream(&publish_key3.token, &wrong_room, &media_id)
        .await;
    assert!(result.is_err(), "Should fail with wrong room");
}

#[tokio::test]
async fn test_e2e_permission_checks() {
    use synctv_core::models::PermissionBits;

    let mut member_perms = PermissionBits(PermissionBits::DEFAULT_MEMBER);

    assert!(member_perms.has(PermissionBits::SEND_CHAT));
    assert!(member_perms.has(PermissionBits::ADD_MEDIA));
    assert!(!member_perms.has(PermissionBits::KICK_MEMBER));

    member_perms.grant(PermissionBits::KICK_MEMBER);
    assert!(member_perms.has(PermissionBits::KICK_MEMBER));
    assert!(member_perms.has(PermissionBits::SEND_CHAT));

    member_perms.revoke(PermissionBits::KICK_MEMBER);
    assert!(!member_perms.has(PermissionBits::KICK_MEMBER));
    assert!(member_perms.has(PermissionBits::SEND_CHAT));
}

#[tokio::test]
async fn test_e2e_playlist_hierarchy() {
    use synctv_core::models::{Playlist, PlaylistId, RoomId};

    let room_id = RoomId::new();
    let creator_id = UserId::new();

    let root = Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: Some(creator_id.clone()),
        name: String::new(),
        parent_id: None,
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };

    assert!(root.is_root());
    assert!(!root.is_dynamic());
    assert!(root.is_static());

    let static_folder = Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: Some(creator_id.clone()),
        name: "Movies".to_string(),
        parent_id: Some(root.id.clone()),
        position: 0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };

    assert!(!static_folder.is_root());
    assert!(!static_folder.is_dynamic());
    assert!(static_folder.is_static());
    assert_eq!(static_folder.parent_id.unwrap(), root.id);

    let dynamic_folder = Playlist {
        id: PlaylistId::new(),
        room_id,
        creator_id: Some(creator_id),
        name: "Alist Movies".to_string(),
        parent_id: Some(root.id.clone()),
        position: 1,
        source_provider: Some("alist".to_string()),
        source_config: Some(serde_json::json!({
            "url": "http://alist.example.com",
            "path": "/movies"
        })),
        provider_instance_name: Some("main_alist".to_string()),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 0,
    };

    assert!(!dynamic_folder.is_root());
    assert!(dynamic_folder.is_dynamic());
    assert!(!dynamic_folder.is_static());
    assert_eq!(dynamic_folder.parent_id.unwrap(), root.id);
}

#[tokio::test]
async fn test_e2e_multiple_users_concurrent_auth() {
    use std::sync::Arc;

    let jwt_service = Arc::new(create_test_jwt_service());
    let mut handles = vec![];

    for _ in 0..10 {
        let jwt = jwt_service.clone();
        let handle = tokio::spawn(async move {
            let user_id = UserId::new();

            let token = jwt
                .sign_token(&user_id, TokenType::Access, 0)
                .expect("Failed to sign token");

            let claims = jwt
                .verify_access_token(&token)
                .expect("Failed to verify token");

            assert_eq!(claims.sub, user_id.as_str());
            (user_id, claims)
        });
        handles.push(handle);
    }

    let results: Vec<_> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    assert_eq!(results.len(), 10);
}

#[tokio::test]
async fn test_e2e_permission_inheritance() {
    use synctv_core::models::PermissionBits;

    let admin = PermissionBits(PermissionBits::DEFAULT_ADMIN);
    assert!(admin.has(PermissionBits::SEND_CHAT));
    assert!(admin.has(PermissionBits::ADD_MEDIA));
    assert!(admin.has(PermissionBits::KICK_MEMBER));
    assert!(admin.has(PermissionBits::SET_ROOM_SETTINGS));

    let guest = PermissionBits(PermissionBits::DEFAULT_GUEST);
    assert!(guest.has(PermissionBits::VIEW_PLAYLIST));
    assert!(!guest.has(PermissionBits::SEND_CHAT));
    assert!(!guest.has(PermissionBits::ADD_MEDIA));
}

#[tokio::test]
async fn test_e2e_token_type_validation() {
    let jwt_service = create_test_jwt_service();
    let user_id = UserId::new();

    let access = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
        .unwrap();

    let refresh = jwt_service
        .sign_token(&user_id, TokenType::Refresh, 0)
        .unwrap();

    assert!(jwt_service.verify_access_token(&access).is_ok());
    assert!(jwt_service.verify_refresh_token(&access).is_err());

    assert!(jwt_service.verify_refresh_token(&refresh).is_ok());
    assert!(jwt_service.verify_access_token(&refresh).is_err());
}

#[tokio::test]
async fn test_e2e_id_generation_collision_resistance() {
    use std::collections::HashSet;
    use synctv_core::models::{MediaId, PlaylistId, RoomId};

    let mut room_ids = HashSet::new();
    let mut media_ids = HashSet::new();
    let mut playlist_ids = HashSet::new();

    for _ in 0..1000 {
        let room = RoomId::new();
        let media = MediaId::new();
        let playlist = PlaylistId::new();

        assert!(room_ids.insert(room.as_str().to_string()));
        assert!(media_ids.insert(media.as_str().to_string()));
        assert!(playlist_ids.insert(playlist.as_str().to_string()));
    }

    assert_eq!(room_ids.len(), 1000);
    assert_eq!(media_ids.len(), 1000);
    assert_eq!(playlist_ids.len(), 1000);
}

#[tokio::test]
async fn test_e2e_error_propagation() {
    use synctv_core::Error;

    let errors = vec![
        Error::Authentication("Invalid credentials".to_string()),
        Error::Authorization("Permission denied".to_string()),
        Error::NotFound("Room not found".to_string()),
        Error::InvalidInput("Invalid room name".to_string()),
        Error::Authorization("Cannot kick admin".to_string()),
        Error::Internal("Database error".to_string()),
    ];

    for error in errors {
        let msg = format!("{error}");
        assert!(!msg.is_empty());

        match error {
            Error::Authentication(_) => assert!(msg.contains("Invalid credentials")),
            Error::Authorization(ref m) if m.contains("Cannot kick admin") => {
                assert!(msg.contains("Cannot kick admin"));
            }
            Error::Authorization(_) => {
                assert!(msg.contains("Permission denied") || msg.contains("Cannot kick admin"));
            }
            Error::NotFound(_) => assert!(msg.contains("Room not found")),
            Error::InvalidInput(_) => assert!(msg.contains("Invalid room name")),
            Error::Internal(_) => assert!(msg.contains("Database error")),
            _ => panic!("Unexpected error type"),
        }
    }
}

// ============================================================================
// Permission Cache Invalidation Tests
// ============================================================================

/// Tests that permission cache invalidation messages are correctly generated
/// and can be deserialized for cross-replica propagation.
#[tokio::test]
async fn test_permission_cache_invalidation_message_serialization() {
    use synctv_core::cache::InvalidationMessage;

    // Test UserPermission invalidation
    let user_perm = InvalidationMessage::UserPermission {
        room_id: "room_123".to_string(),
        user_id: "user_456".to_string(),
    };
    let json = serde_json::to_string(&user_perm).expect("Should serialize UserPermission");
    let decoded: InvalidationMessage =
        serde_json::from_str(&json).expect("Should deserialize UserPermission");
    assert_eq!(decoded, user_perm);

    // Test RoomPermission invalidation
    let room_perm = InvalidationMessage::RoomPermission {
        room_id: "room_789".to_string(),
    };
    let json = serde_json::to_string(&room_perm).expect("Should serialize RoomPermission");
    let decoded: InvalidationMessage =
        serde_json::from_str(&json).expect("Should deserialize RoomPermission");
    assert_eq!(decoded, room_perm);

    // Test User invalidation
    let user = InvalidationMessage::User {
        user_id: "user_abc".to_string(),
    };
    let json = serde_json::to_string(&user).expect("Should serialize User");
    let decoded: InvalidationMessage =
        serde_json::from_str(&json).expect("Should deserialize User");
    assert_eq!(decoded, user);

    // Test Room invalidation
    let room = InvalidationMessage::Room {
        room_id: "room_xyz".to_string(),
    };
    let json = serde_json::to_string(&room).expect("Should serialize Room");
    let decoded: InvalidationMessage =
        serde_json::from_str(&json).expect("Should deserialize Room");
    assert_eq!(decoded, room);

    // Test All invalidation
    let all = InvalidationMessage::All;
    let json = serde_json::to_string(&all).expect("Should serialize All");
    let decoded: InvalidationMessage = serde_json::from_str(&json).expect("Should deserialize All");
    assert_eq!(decoded, all);
}

/// Tests that `CacheInvalidationService` can be created and used without Redis
/// (local-only mode for single-node deployments).
#[tokio::test]
async fn test_cache_invalidation_service_local_only() {
    use synctv_core::cache::CacheInvalidationService;

    // Create a local-only cache invalidation service (no Redis)
    let service = CacheInvalidationService::new(
        None,
        "local_node".to_string(),
        "test:local:cache".to_string(),
    );

    // Subscribe to local invalidations
    let mut rx = service.subscribe();

    // Broadcast a local invalidation
    service
        .broadcast_local(synctv_core::cache::InvalidationMessage::User {
            user_id: "local_user".to_string(),
        })
        .expect("Should broadcast locally");

    // Should receive the invalidation locally
    let msg = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("Should receive local invalidation")
        .expect("Channel not closed");

    match msg {
        synctv_core::cache::InvalidationMessage::User { user_id } => {
            assert_eq!(user_id, "local_user");
        }
        other => panic!("Expected User invalidation, got: {other:?}"),
    }
}

/// Tests that cache invalidation broadcast channel works correctly
/// with multiple subscribers.
#[tokio::test]
async fn test_cache_invalidation_multiple_subscribers() {
    use synctv_core::cache::CacheInvalidationService;

    let service = CacheInvalidationService::new(
        None,
        "multi_sub_node".to_string(),
        "test:multi:cache".to_string(),
    );

    // Create multiple subscribers
    let mut rx1 = service.subscribe();
    let mut rx2 = service.subscribe();
    let mut rx3 = service.subscribe();

    // Broadcast an invalidation
    service
        .broadcast_local(synctv_core::cache::InvalidationMessage::Room {
            room_id: "shared_room".to_string(),
        })
        .expect("Should broadcast locally");

    // All subscribers should receive the message
    let msg1 = tokio::time::timeout(std::time::Duration::from_secs(2), rx1.recv())
        .await
        .expect("Subscriber 1 should receive")
        .expect("Channel not closed");

    let msg2 = tokio::time::timeout(std::time::Duration::from_secs(2), rx2.recv())
        .await
        .expect("Subscriber 2 should receive")
        .expect("Channel not closed");

    let msg3 = tokio::time::timeout(std::time::Duration::from_secs(2), rx3.recv())
        .await
        .expect("Subscriber 3 should receive")
        .expect("Channel not closed");

    // All should be Room invalidations
    assert!(matches!(
        msg1,
        synctv_core::cache::InvalidationMessage::Room { .. }
    ));
    assert!(matches!(
        msg2,
        synctv_core::cache::InvalidationMessage::Room { .. }
    ));
    assert!(matches!(
        msg3,
        synctv_core::cache::InvalidationMessage::Room { .. }
    ));
}

// ============================================================================
// Concurrent Permission Check Tests
// ============================================================================

/// Tests that permission checks can be performed concurrently without
/// data races or corruption.
#[tokio::test]
async fn test_concurrent_permission_checks() {
    use std::sync::Arc;
    use synctv_core::models::PermissionBits;

    let perms = Arc::new(std::sync::atomic::AtomicU64::new(
        PermissionBits::DEFAULT_MEMBER,
    ));

    // Spawn multiple concurrent readers
    let mut read_handles = vec![];
    for _ in 0..10 {
        let perms = perms.clone();
        let handle = tokio::spawn(async move {
            let current = perms.load(std::sync::atomic::Ordering::SeqCst);
            let bits = PermissionBits(current);
            bits.has(PermissionBits::SEND_CHAT)
        });
        read_handles.push(handle);
    }

    // Spawn concurrent modifiers
    let mut write_handles = vec![];
    for _ in 0..5 {
        let perms = perms.clone();
        let handle = tokio::spawn(async move {
            perms.fetch_or(
                PermissionBits::KICK_MEMBER,
                std::sync::atomic::Ordering::SeqCst,
            );
        });
        write_handles.push(handle);
    }

    // Wait for all operations to complete
    for handle in read_handles {
        let _ = handle.await;
    }
    for handle in write_handles {
        let _ = handle.await;
    }

    // Verify final state includes KICK_MEMBER
    let final_perms = PermissionBits(perms.load(std::sync::atomic::Ordering::SeqCst));
    assert!(final_perms.has(PermissionBits::KICK_MEMBER));
    assert!(final_perms.has(PermissionBits::SEND_CHAT)); // Original permission preserved
}

// ============================================================================
// Token Blacklist Tests
// ============================================================================

/// Tests token blacklist behavior with concurrent modifications.
#[tokio::test]
async fn test_token_blacklist_concurrent_operations() {
    use parking_lot::RwLock;
    use std::collections::HashSet;
    use std::sync::Arc;

    // Simulate a simple in-memory blacklist
    let blacklist: Arc<RwLock<HashSet<String>>> = Arc::new(RwLock::new(HashSet::new()));

    // Concurrent adds
    let mut add_handles = vec![];
    for i in 0..100 {
        let blacklist = blacklist.clone();
        let handle = tokio::spawn(async move {
            let token = format!("token_{i}");
            blacklist.write().insert(token);
        });
        add_handles.push(handle);
    }

    // Concurrent checks
    let mut check_handles = vec![];
    for i in 0..50 {
        let blacklist = blacklist.clone();
        let handle = tokio::spawn(async move {
            let token = format!("token_{i}");
            blacklist.read().contains(&token)
        });
        check_handles.push(handle);
    }

    for handle in add_handles {
        let _ = handle.await;
    }
    for handle in check_handles {
        let _ = handle.await;
    }

    // Verify all 100 tokens were added
    let final_blacklist = blacklist.read();
    assert_eq!(final_blacklist.len(), 100);
}

// ============================================================================
// Playback State Concurrency Tests
// ============================================================================

/// Tests that playback state updates are thread-safe.
#[tokio::test]
async fn test_playback_state_concurrent_updates() {
    use parking_lot::Mutex;
    use std::sync::Arc;
    use synctv_core::models::{id::RoomId, playback::RoomPlaybackState};

    let state = Arc::new(Mutex::new(RoomPlaybackState::new(RoomId::new())));

    let mut handles = vec![];

    // Concurrent position updates
    for i in 0..50 {
        let state = state.clone();
        let handle = tokio::spawn(async move {
            let mut s = state.lock();
            s.current_time = f64::from(i * 1000);
        });
        handles.push(handle);
    }

    // Concurrent playing status updates
    for _ in 0..25 {
        let state = state.clone();
        let handle = tokio::spawn(async move {
            let mut s = state.lock();
            s.is_playing = !s.is_playing;
        });
        handles.push(handle);
    }

    for handle in handles {
        let _ = handle.await;
    }

    // State should be valid (not corrupted)
    let final_state = state.lock();
    // Position should be some value < 50000
    assert!(final_state.current_time < 50000.0);
}

// ============================================================================
// Permission Bits Stress Test
// ============================================================================

/// Stress test for permission bit operations.
#[test]
fn test_permission_bits_stress() {
    use std::sync::Arc;
    use std::thread;
    use synctv_core::models::PermissionBits;

    let perms = Arc::new(std::sync::atomic::AtomicU64::new(
        PermissionBits::DEFAULT_MEMBER,
    ));
    let mut handles = vec![];

    for _ in 0..4 {
        let perms = perms.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..10000 {
                // Read
                let current = perms.load(std::sync::atomic::Ordering::SeqCst);
                let bits = PermissionBits(current);

                // Check some permissions
                let _ = bits.has(PermissionBits::SEND_CHAT);

                // Grant and revoke
                perms.fetch_or(
                    PermissionBits::ADD_MEDIA,
                    std::sync::atomic::Ordering::SeqCst,
                );
                perms.fetch_and(
                    !PermissionBits::ADD_MEDIA,
                    std::sync::atomic::Ordering::SeqCst,
                );
            }
        }));
    }

    for handle in handles {
        handle.join().expect("Thread should complete");
    }

    // Final state should be valid
    let final_perms = PermissionBits(perms.load(std::sync::atomic::Ordering::SeqCst));
    assert!(final_perms.has(PermissionBits::SEND_CHAT)); // Should still have original permission
}
