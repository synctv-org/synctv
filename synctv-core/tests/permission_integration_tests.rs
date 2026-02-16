//! Permission check integration tests with database (Task #84)
//!
//! Tests verify Allow/Deny permission pattern and integration with database.
//!
//! Run with: cargo test --test permission_integration_tests

use synctv_core::{
    models::{Room, RoomId, RoomMember, RoomRole, UserId, MemberStatus, PermissionBits, RoomSettings},
    repository::{RoomRepository, RoomMemberRepository, RoomSettingsRepository},
    service::permission::PermissionService,
};
use sqlx::PgPool;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

async fn create_test_pool() -> (Cli, testcontainers::Container<'static, Postgres>, PgPool) {
    let docker = Cli::default();
    let postgres = docker.run(Postgres::default());

    let connection_string = format!(
        "postgresql://postgres:postgres@127.0.0.1:{}/postgres",
        postgres.get_host_port_ipv4(5432)
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    (docker, postgres, pool)
}

#[tokio::test]
async fn test_permission_check_with_database_member() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create room
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    // Create member with specific permissions
    let user_id = UserId::new();
    let mut perms = PermissionBits(0);
    perms.grant(PermissionBits::SEND_CHAT);
    perms.grant(PermissionBits::ADD_MEDIA);

    let member = RoomMember {
        room_id: room.id.clone(),
        user_id: user_id.clone(),
        role: RoomRole::Member,
        permissions: perms.0,
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    member_repo.create(&member).await.expect("Failed to create member");

    // Create permission service
    let perm_service = PermissionService::new(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
    );

    // Check permissions
    let has_chat = perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::SEND_CHAT)
        .await
        .expect("Failed to check permission");
    assert!(has_chat, "User should have SEND_CHAT permission");

    let has_media = perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::ADD_MEDIA)
        .await
        .expect("Failed to check permission");
    assert!(has_media, "User should have ADD_MEDIA permission");

    let has_kick = perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::KICK_MEMBER)
        .await
        .expect("Failed to check permission");
    assert!(!has_kick, "User should not have KICK_MEMBER permission");
}

#[tokio::test]
async fn test_permission_allow_deny_pattern() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create room
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    // Create admin member
    let admin_id = UserId::new();
    let admin_member = RoomMember {
        room_id: room.id.clone(),
        user_id: admin_id.clone(),
        role: RoomRole::Admin,
        permissions: PermissionBits::DEFAULT_ADMIN,
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    member_repo.create(&admin_member).await.expect("Failed to create admin");

    // Create guest member with minimal permissions
    let guest_id = UserId::new();
    let guest_member = RoomMember {
        room_id: room.id.clone(),
        user_id: guest_id.clone(),
        role: RoomRole::Guest,
        permissions: PermissionBits::DEFAULT_GUEST,
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    member_repo.create(&guest_member).await.expect("Failed to create guest");

    let perm_service = PermissionService::new(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
    );

    // Admin should have all admin permissions
    let admin_can_kick = perm_service.check_permission_no_cache(&room.id, &admin_id, PermissionBits::KICK_MEMBER)
        .await
        .expect("Failed to check permission");
    assert!(admin_can_kick, "Admin should be able to kick members");

    // Guest should have limited permissions
    let guest_can_chat = perm_service.check_permission_no_cache(&room.id, &guest_id, PermissionBits::SEND_CHAT)
        .await
        .expect("Failed to check permission");
    assert!(!guest_can_chat, "Guest should not be able to send chat");

    let guest_can_view = perm_service.check_permission_no_cache(&room.id, &guest_id, PermissionBits::VIEW_PLAYLIST)
        .await
        .expect("Failed to check permission");
    assert!(guest_can_view, "Guest should be able to view playlist");
}

#[tokio::test]
async fn test_permission_caching_consistency() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create room and member
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    let user_id = UserId::new();
    let member = RoomMember {
        room_id: room.id.clone(),
        user_id: user_id.clone(),
        role: RoomRole::Member,
        permissions: PermissionBits::DEFAULT_MEMBER,
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    member_repo.create(&member).await.expect("Failed to create member");

    let perm_service = PermissionService::new(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
    );

    // Check with cache
    let has_chat = perm_service.check_permission(&room.id, &user_id, PermissionBits::SEND_CHAT)
        .await
        .expect("Failed to check permission");
    assert!(has_chat);

    // Update permissions in database
    let mut updated_member = member.clone();
    updated_member.permissions = 0; // Remove all permissions
    member_repo.update(&updated_member).await.expect("Failed to update member");

    // Invalidate cache
    perm_service.invalidate_user_permission(&room.id, &user_id).await;

    // Check again (should reflect new permissions)
    let has_chat_after = perm_service.check_permission(&room.id, &user_id, PermissionBits::SEND_CHAT)
        .await
        .expect("Failed to check permission");
    assert!(!has_chat_after, "Permission should be revoked after update");
}

#[tokio::test]
async fn test_permission_banned_member_denied() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create room and member
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    let user_id = UserId::new();
    let member = RoomMember {
        room_id: room.id.clone(),
        user_id: user_id.clone(),
        role: RoomRole::Member,
        permissions: PermissionBits::DEFAULT_MEMBER,
        status: MemberStatus::Banned,  // Banned status
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    member_repo.create(&member).await.expect("Failed to create member");

    let perm_service = PermissionService::new(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
    );

    // Banned member should not have permissions
    let result = perm_service.check_permission_no_cache(&room.id, &user_id, PermissionBits::SEND_CHAT)
        .await;

    // Should either return false or error depending on implementation
    match result {
        Ok(has_perm) => assert!(!has_perm, "Banned member should not have permissions"),
        Err(_) => { /* Banned status returns error - also acceptable */ }
    }
}

#[tokio::test]
async fn test_permission_non_member_denied() {
    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create room
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    let perm_service = PermissionService::new(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
    );

    // Non-member should not have permissions
    let non_member_id = UserId::new();
    let result = perm_service.check_permission_no_cache(&room.id, &non_member_id, PermissionBits::SEND_CHAT)
        .await;

    match result {
        Ok(has_perm) => assert!(!has_perm, "Non-member should not have permissions"),
        Err(_) => { /* Non-member returns error - also acceptable */ }
    }
}

#[tokio::test]
async fn test_permission_bit_operations() {
    let mut perms = PermissionBits(0);

    // Grant permissions
    perms.grant(PermissionBits::SEND_CHAT);
    assert!(perms.has(PermissionBits::SEND_CHAT));

    perms.grant(PermissionBits::ADD_MEDIA);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MEDIA));

    // Revoke permission
    perms.revoke(PermissionBits::SEND_CHAT);
    assert!(!perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::ADD_MEDIA));

    // Grant multiple
    perms = PermissionBits(PermissionBits::DEFAULT_ADMIN);
    assert!(perms.has(PermissionBits::SEND_CHAT));
    assert!(perms.has(PermissionBits::KICK_MEMBER));
    assert!(perms.has(PermissionBits::SET_ROOM_SETTINGS));
}

#[tokio::test]
async fn test_concurrent_permission_checks() {
    use std::sync::Arc;

    let (_docker, _container, pool) = create_test_pool().await;

    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    // Create room and member
    let creator_id = UserId::new();
    let room = Room {
        id: RoomId::new(),
        name: "Test Room".to_string(),
        description: "Test".to_string(),
        created_by: creator_id.clone(),
        status: synctv_core::models::RoomStatus::Active,
        is_banned: false,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        deleted_at: None,
    };
    room_repo.create(&room).await.expect("Failed to create room");

    let user_id = UserId::new();
    let member = RoomMember {
        room_id: room.id.clone(),
        user_id: user_id.clone(),
        role: RoomRole::Member,
        permissions: PermissionBits::DEFAULT_MEMBER,
        status: MemberStatus::Active,
        joined_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    };
    member_repo.create(&member).await.expect("Failed to create member");

    let perm_service = Arc::new(PermissionService::new(
        member_repo.clone(),
        room_repo.clone(),
        None,
        1000,
        300,
    ));

    // Concurrent permission checks
    let mut handles = vec![];
    for _ in 0..10 {
        let service = perm_service.clone();
        let room_id = room.id.clone();
        let user_id = user_id.clone();

        let handle = tokio::spawn(async move {
            service.check_permission(&room_id, &user_id, PermissionBits::SEND_CHAT)
                .await
                .expect("Failed to check permission")
        });
        handles.push(handle);
    }

    let results: Vec<bool> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // All checks should return the same result
    assert!(results.iter().all(|&r| r), "All concurrent checks should succeed");
}
