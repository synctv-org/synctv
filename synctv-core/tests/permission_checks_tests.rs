//! `PermissionService` integration tests for batch checks and role checks.

mod permission_test_support;

use synctv_core::{
    cache::{CacheDomain, ConsistencyCoordinator, RedisVersionFenceStore, VersionFenceStore},
    models::{
        room_settings::MemberRemovedPermissions, RoomId, RoomMember, RoomMemberPermissionBits,
        RoomPermission, RoomRole, UserId,
    },
    repository::{RoomMemberRepository, RoomSettingsRepository, UserRepository},
    service::RoomServiceOptions,
};
use synctv_core_testing::create_test_pool;
use synctv_core_testing::{TestOptionExt, TestResultExt};

use permission_test_support::{make_room_service, make_user};

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_redis_batch_fence_read_hides_pending_permission_reservation() {
    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = std::sync::Arc::new(RedisVersionFenceStore::new(
        synctv_core::direct_runtime(redis_conn),
        "test:perm-pending-batch:",
    ));
    let coordinator = ConsistencyCoordinator::new(fence.clone());
    let room_id = RoomId::expect_positive(91_001);
    let user_id = UserId::expect_positive(91_002);
    let permission_domain = CacheDomain::Permission { room_id, user_id };
    let settings_domain = CacheDomain::RoomSettings { room_id };

    fence
        .set_version_at_least(&permission_domain, 5)
        .await
        .checked("test operation should succeed");
    fence
        .set_version_at_least(&settings_domain, 3)
        .await
        .checked("test operation should succeed");
    let reservation = fence
        .begin_write(&permission_domain, 5)
        .await
        .checked("test operation should succeed");

    assert_eq!(
        fence
            .current_versions(&[permission_domain.clone(), settings_domain.clone()])
            .await
            .checked("test operation should succeed"),
        vec![None, Some(3)],
        "Redis batch fence reads must not expose committed permission version while a write is pending"
    );
    assert_eq!(
        coordinator
            .current_versions(&[permission_domain.clone(), settings_domain.clone()])
            .await
            .checked("test operation should succeed"),
        vec![None, Some(3)],
        "coordinator batch reads must preserve the same pending semantics"
    );

    fence
        .commit_write(&permission_domain, &reservation)
        .await
        .checked("test operation should succeed");
    assert_eq!(
        fence
            .current_versions(&[permission_domain, settings_domain])
            .await
            .checked("test operation should succeed"),
        vec![Some(6), Some(3)]
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_strong_permission_read_rejects_stale_l1_after_fence_bump() {
    let (_container, pool) = create_test_pool().await;
    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = std::sync::Arc::new(RedisVersionFenceStore::new(
        synctv_core::direct_runtime(redis_conn),
        "test:perm-fence:",
    ));

    let user_service = permission_test_support::make_user_service(&pool);
    let room_service = synctv_core::service::RoomService::new_with_options(
        pool.clone(),
        user_service,
        RoomServiceOptions {
            version_fence: fence.clone(),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .checked("room service should build");
    let user_repo = UserRepository::new(pool.clone());
    let creator = user_repo
        .create(&make_user("perm_fence_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("perm_fence_member"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Permission Fence Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let initial = perm_service
        .get_user_permissions_strong(&room.id, &member.id)
        .await
        .checked("test operation should succeed");
    assert!(initial.has(RoomPermission::CHAT));

    let updated = room_service
        .set_member_permission(
            room.id,
            creator.id,
            member.id,
            0,
            RoomMemberPermissionBits::CHAT,
        )
        .await
        .checked("test operation should succeed");
    assert_eq!(updated.removed_permissions, RoomMemberPermissionBits::CHAT);

    let fence_version = fence
        .current_version(&CacheDomain::Permission {
            room_id: room.id,
            user_id: member.id,
        })
        .await
        .checked("test operation should succeed")
        .checked("permission fence should exist after mutation");
    assert!(fence_version > 0);

    let strong = perm_service
        .get_user_permissions_strong(&room.id, &member.id)
        .await
        .checked("test operation should succeed");
    assert!(
        !strong.has(RoomPermission::CHAT),
        "strong permission read must reject stale L1 after fence bump"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_strong_permission_read_rejects_stale_l1_after_room_settings_fence_bump() {
    let (_container, pool) = create_test_pool().await;
    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = std::sync::Arc::new(RedisVersionFenceStore::new(
        synctv_core::direct_runtime(redis_conn),
        "test:perm-room-fence:",
    ));

    let user_service = permission_test_support::make_user_service(&pool);
    let room_service = synctv_core::service::RoomService::new_with_options(
        pool.clone(),
        user_service,
        RoomServiceOptions {
            version_fence: fence.clone(),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .checked("room service should build");
    let user_repo = UserRepository::new(pool.clone());
    let creator = user_repo
        .create(&make_user("perm_room_fence_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("perm_room_fence_member"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Permission Room Fence Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let initial = perm_service
        .get_user_permissions_strong(&room.id, &member.id)
        .await
        .checked("test operation should succeed");
    assert!(initial.has(RoomPermission::CHAT));

    let mut settings = room_service
        .get_room_settings(&room.id)
        .await
        .checked("test operation should succeed");
    settings.member_removed_permissions = MemberRemovedPermissions(RoomMemberPermissionBits::CHAT);
    room_service
        .set_room_settings(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    let settings_fence = fence
        .current_version(&CacheDomain::RoomSettings { room_id: room.id })
        .await
        .checked("test operation should succeed")
        .checked("room settings fence should exist after settings mutation");
    assert!(settings_fence > 0);

    let strong = perm_service
        .get_user_permissions_strong(&room.id, &member.id)
        .await
        .checked("test operation should succeed");
    assert!(
        !strong.has(RoomPermission::CHAT),
        "strong permission read must reject stale L1 after room settings fence bump"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_eventual_permission_cache_preserves_room_settings_version() {
    let (_container, pool) = create_test_pool().await;
    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = std::sync::Arc::new(RedisVersionFenceStore::new(
        synctv_core::direct_runtime(redis_conn),
        "test:perm-eventual-settings-version:",
    ));

    let user_service = permission_test_support::make_user_service(&pool);
    let room_service = synctv_core::service::RoomService::new_with_options(
        pool.clone(),
        user_service,
        RoomServiceOptions {
            version_fence: fence.clone(),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .checked("room service should build");
    let user_repo = UserRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let settings_repo = RoomSettingsRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("perm_eventual_settings_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("perm_eventual_settings_member"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Permission Eventual Settings Version Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    let mut settings = room_service
        .get_room_settings(&room.id)
        .await
        .checked("test operation should succeed");
    settings.member_removed_permissions = MemberRemovedPermissions(0);
    room_service
        .set_room_settings(&room.id, &settings)
        .await
        .checked("test operation should succeed");

    let member_row = member_repo
        .get(&room.id, &member.id)
        .await
        .checked("test operation should succeed")
        .checked("member should exist");
    let (_settings, settings_version) = settings_repo
        .get_with_version(&room.id)
        .await
        .checked("test operation should succeed");
    fence
        .set_version_at_least(
            &CacheDomain::Permission {
                room_id: room.id,
                user_id: member.id,
            },
            member_row.version,
        )
        .await
        .checked("test operation should succeed");
    fence
        .set_version_at_least(
            &CacheDomain::RoomSettings { room_id: room.id },
            settings_version,
        )
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let cached = perm_service
        .get_user_permissions_eventually_consistent(&room.id, &member.id)
        .await
        .checked("eventual permission read should populate L1");
    assert!(cached.has(RoomPermission::CHAT));

    member_repo
        .update_permissions(
            &room.id,
            &member.id,
            0,
            RoomMemberPermissionBits::CHAT,
            member_row.version,
        )
        .await
        .checked("test operation should succeed");

    let strong = perm_service
        .get_user_permissions_strong(&room.id, &member.id)
        .await
        .checked("strong read should be able to trust the freshly populated L1 entry");
    assert!(
        strong.has(RoomPermission::CHAT),
        "eventual cache population must store the room settings version so strong reads can trust the entry"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_strong_permission_read_treats_missing_fences_as_cache_miss() {
    let (_container, pool) = create_test_pool().await;
    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = std::sync::Arc::new(RedisVersionFenceStore::new(
        synctv_core::direct_runtime(redis_conn.clone()),
        "test:perm-missing-fence:",
    ));

    let user_service = permission_test_support::make_user_service(&pool);
    let room_service = synctv_core::service::RoomService::new_with_options(
        pool.clone(),
        user_service,
        RoomServiceOptions {
            version_fence: fence.clone(),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .checked("room service should build");
    let user_repo = UserRepository::new(pool.clone());
    let creator = user_repo
        .create(&make_user("perm_missing_fence_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("perm_missing_fence_member"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Permission Missing Fence Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    RoomMemberRepository::new(pool.clone())
        .add(&RoomMember::new(room.id, member.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let cached = perm_service
        .get_user_permissions_eventually_consistent(&room.id, &member.id)
        .await
        .checked("eventual permission read should populate source caches");
    assert!(cached.has(RoomPermission::CHAT));

    room_service
        .set_member_permission(
            room.id,
            creator.id,
            member.id,
            0,
            RoomMemberPermissionBits::CHAT,
        )
        .await
        .checked("test operation should succeed");
    let permission_domain = CacheDomain::Permission {
        room_id: room.id,
        user_id: member.id,
    };
    let settings_domain = CacheDomain::RoomSettings { room_id: room.id };
    let mut raw_redis = redis_conn;
    let _: () = redis::cmd("DEL")
        .arg(format!(
            "test:perm-missing-fence:cache:fence:{permission_domain}"
        ))
        .arg(format!(
            "test:perm-missing-fence:cache:fence:{settings_domain}"
        ))
        .query_async(&mut raw_redis)
        .await
        .checked("test operation should succeed");

    let strong = perm_service
        .get_user_permissions_strong(&room.id, &member.id)
        .await
        .checked("strong permission read should fall back to DB when fences are missing");
    assert!(
        !strong.has(RoomPermission::CHAT),
        "missing authoritative fences must not allow stale L1 authorization data"
    );

    let member_version = RoomMemberRepository::new(pool.clone())
        .get(&room.id, &member.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed")
        .version;
    let (_settings, settings_version) = RoomSettingsRepository::new(pool.clone())
        .get_with_version(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(
        fence
            .current_version(&permission_domain)
            .await
            .checked("test operation should succeed"),
        Some(member_version)
    );
    assert_eq!(
        fence
            .current_version(&settings_domain)
            .await
            .checked("test operation should succeed"),
        Some(settings_version)
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_permissions_batch_all_present() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_perm_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Batch Perm Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let result = perm_service
        .check_permissions(
            &room.id,
            &creator.id,
            &[
                RoomPermission::CHAT,
                RoomPermission::CREATE_MEDIA_RESOURCE,
                RoomPermission::SET_ROOM_SETTINGS,
            ],
        )
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_permissions_batch_one_missing_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("batch_miss_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("batch_miss_member"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Batch Miss Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let result = perm_service
        .check_permissions(
            &room.id,
            &member.id,
            &[RoomPermission::CHAT, RoomPermission::SET_ROOM_SETTINGS],
        )
        .await;

    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_permissions_batch_rejects_stale_l1_after_room_settings_fence_bump() {
    let (_container, pool) = create_test_pool().await;
    let (_redis_container, redis_conn) = synctv_core_testing::start_redis().await;
    let fence = std::sync::Arc::new(RedisVersionFenceStore::new(
        synctv_core::direct_runtime(redis_conn),
        "test:batch-perm-fence:",
    ));

    let user_service = permission_test_support::make_user_service(&pool);
    let room_service = synctv_core::service::RoomService::new_with_options(
        pool.clone(),
        user_service,
        RoomServiceOptions {
            version_fence: fence.clone(),
            ..RoomServiceOptions::test_defaults_with_settings(pool.clone())
        },
    )
    .checked("room service should build");
    let user_repo = UserRepository::new(pool.clone());
    let creator = user_repo
        .create(&make_user("batch_stale_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("batch_stale_member"))
        .await
        .checked("test operation should succeed");
    let (room, _) = room_service
        .create_room(
            "Batch Stale Permission Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");
    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let cached = perm_service
        .get_user_permissions_eventually_consistent(&room.id, &member.id)
        .await
        .checked("eventual permission read should populate stale L1 fixture");
    assert!(cached.has(RoomPermission::CHAT));

    let mut settings = room_service
        .get_room_settings(&room.id)
        .await
        .checked("test operation should succeed");
    settings.member_removed_permissions = MemberRemovedPermissions(RoomMemberPermissionBits::CHAT);
    room_service
        .set_room_settings(&room.id, &settings)
        .await
        .checked("test operation should succeed");
    fence
        .set_version_at_least(&CacheDomain::RoomSettings { room_id: room.id }, 2)
        .await
        .checked("test operation should succeed");

    let result = perm_service
        .check_permissions(&room.id, &member.id, &[RoomPermission::CHAT])
        .await;

    assert!(
        result.is_err(),
        "batch permission checks must reject stale L1 after room settings fence advances"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_role_creator_passes() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("checkrole_creator"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Check Role Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let result = perm_service
        .check_role(&room.id, &creator.id, RoomRole::Creator)
        .await;

    assert!(result.is_ok());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_role_member_not_creator_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("checkrole2_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("checkrole2_member"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Check Role 2 Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    let result = perm_service
        .check_role(&room.id, &member.id, RoomRole::Creator)
        .await;

    assert!(result.is_err());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_check_permission_bypasses_stale_l1_after_membership_removed() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_service = make_room_service(pool.clone());

    let creator = user_repo
        .create(&make_user("strong_perm_creator"))
        .await
        .checked("test operation should succeed");
    let member = user_repo
        .create(&make_user("strong_perm_member"))
        .await
        .checked("test operation should succeed");

    let (room, _) = room_service
        .create_room(
            "Strong Permission Room".to_string(),
            String::new(),
            creator.id,
            None,
            None,
        )
        .await
        .checked("test operation should succeed");

    room_service
        .join_room(room.id, member.id, None)
        .await
        .checked("test operation should succeed");

    let perm_service = room_service.permission_service();
    perm_service
        .get_user_permissions_eventually_consistent(&room.id, &member.id)
        .await
        .checked("eventual permission read should populate L1");

    RoomMemberRepository::new(pool.clone())
        .remove(&room.id, &member.id)
        .await
        .checked("test operation should succeed");

    let result = perm_service
        .check_permission(&room.id, &member.id, RoomPermission::CHAT)
        .await;

    assert!(
        result.is_err(),
        "strong permission checks must not authorize from stale L1 after membership removal"
    );
}
