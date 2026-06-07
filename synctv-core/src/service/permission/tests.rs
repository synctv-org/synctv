use super::*;
use crate::cache::CacheKey;
use crate::models::permission::Role as RoomRole;
use crate::models::{
    room_settings::*, RoomAdminPermissionBits, RoomGuestPermissionBits, RoomMember,
    RoomMemberPermissionBits,
};
use async_trait::async_trait;
use std::{collections::HashMap, sync::atomic::Ordering};

#[derive(Default)]
struct RecordingVersionFenceStore {
    versions: parking_lot::Mutex<HashMap<CacheDomain, i64>>,
}

#[async_trait]
impl VersionFenceStore for RecordingVersionFenceStore {
    async fn current_version(&self, domain: &CacheDomain) -> Result<Option<i64>> {
        Ok(self.versions.lock().get(domain).copied())
    }

    async fn current_versions(&self, domains: &[CacheDomain]) -> Result<Vec<Option<i64>>> {
        let versions = self.versions.lock();
        Ok(domains
            .iter()
            .map(|domain| versions.get(domain).copied())
            .collect())
    }

    async fn bump_version(&self, domain: &CacheDomain) -> Result<i64> {
        let mut versions = self.versions.lock();
        let version = versions.entry(domain.clone()).or_insert(0);
        *version += 1;
        Ok(*version)
    }

    async fn set_version_at_least(&self, domain: &CacheDomain, version: i64) -> Result<i64> {
        let mut versions = self.versions.lock();
        let current = versions.entry(domain.clone()).or_insert(0);
        if version > *current {
            *current = version;
        }
        Ok(*current)
    }

    async fn reserve_next_after_observed_version(
        &self,
        domain: &CacheDomain,
        observed_version: i64,
    ) -> Result<i64> {
        let mut versions = self.versions.lock();
        let current = versions.entry(domain.clone()).or_insert(0);
        if *current > observed_version {
            return Err(Error::OptimisticLockConflict);
        }

        *current = observed_version + 1;
        Ok(*current)
    }

    fn is_authoritative(&self) -> bool {
        true
    }
}

fn make_service_with_runtime(runtime: PermissionServiceRuntime) -> PermissionService {
    PermissionService::new_without_repositories_for_tests(PermissionServiceRuntime {
        cache_size: 10,
        cache_ttl_secs: 60,
        member_permission_cache_key_prefix: "member_permission:".to_string(),
        room_settings_cache_key_prefix: "room_settings:".to_string(),
        ..runtime
    })
    .expect("permission service should build")
}

fn make_service() -> PermissionService {
    make_service_with_runtime(PermissionServiceRuntime::default())
}

fn make_service_async_with_runtime(runtime: PermissionServiceRuntime) -> PermissionService {
    make_service_with_runtime(runtime)
}

fn make_service_async() -> PermissionService {
    make_service_async_with_runtime(PermissionServiceRuntime::default())
}

fn make_member(role: RoomRole) -> RoomMember {
    RoomMember::new(RoomId::expect_positive(1), UserId::expect_positive(1), role)
}

fn compiled_role_default(role: &RoomRole, settings: &RoomSettings) -> RoomPermissionSet {
    EffectivePermissionCalculator::compiled_defaults().role_default(role, settings)
}

#[test]
fn test_member_permission_cache_key_generation() {
    let room_id = RoomId::expect_positive(123);
    let user_id = UserId::expect_positive(456);
    let key = MemberPermissionKey::new(room_id, user_id);
    assert_eq!(key.cache_key(), "123:456");
}

#[tokio::test]
async fn standalone_permission_constructors_use_non_authoritative_fences_by_default() {
    let service = make_service_async_with_runtime(PermissionServiceRuntime::default());
    assert!(
        !service.consistency.is_authoritative(),
        "default PermissionService runtime must remain non-authoritative"
    );
}

#[test]
fn test_member_permission_cache_key_different_for_different_users() {
    let room = RoomId::expect_positive(1);
    let u1 = UserId::expect_positive(1);
    let u2 = UserId::expect_positive(2);
    assert_ne!(
        MemberPermissionKey::new(room, u1).cache_key(),
        MemberPermissionKey::new(room, u2).cache_key(),
    );
}

#[tokio::test]
async fn test_removed_member_seed_uses_lifecycle_version_and_invalidation_does_not_advance() {
    let fence = Arc::new(RecordingVersionFenceStore::default());
    let service = make_service_async_with_runtime(PermissionServiceRuntime {
        version_fence: Some(fence.clone()),
        ..PermissionServiceRuntime::default()
    });
    let room_id = RoomId::expect_positive(1);
    let user_id = UserId::expect_positive(2);
    let domain = PermissionService::permission_domain(&room_id, &user_id);

    service
        .seed_permission_fence_to_member_version(&room_id, &user_id, 7)
        .await
        .expect("membership removal fence should seed to lifecycle version");
    service
        .invalidate_removed_member_cache(&room_id, &user_id)
        .await;

    assert_eq!(
        fence
            .current_version(&domain)
            .await
            .expect("fence should be readable"),
        Some(7),
        "post-delete invalidation must not advance beyond the DB lifecycle version"
    );
}

#[tokio::test]
async fn test_added_member_seed_does_not_advance_permission_fence() {
    let fence = Arc::new(RecordingVersionFenceStore::default());
    let service = make_service_async_with_runtime(PermissionServiceRuntime {
        version_fence: Some(fence.clone()),
        ..PermissionServiceRuntime::default()
    });
    let room_id = RoomId::expect_positive(1);
    let user_id = UserId::expect_positive(2);
    let domain = PermissionService::permission_domain(&room_id, &user_id);

    service.seed_added_member_cache(&room_id, &user_id, 0).await;

    assert_eq!(
        fence
            .current_version(&domain)
            .await
            .expect("fence should be readable"),
        Some(0),
        "newly inserted version-0 members must not get an unsatisfiable version-1 fence"
    );
}

#[test]
fn test_member_permission_cache_key_different_for_different_rooms() {
    let r1 = RoomId::expect_positive(1);
    let r2 = RoomId::expect_positive(2);
    let user = UserId::expect_positive(1);
    assert_ne!(
        MemberPermissionKey::new(r1, user).cache_key(),
        MemberPermissionKey::new(r2, user).cache_key(),
    );
}

#[test]
fn test_creator_always_gets_all_permissions() {
    let settings = RoomSettings::default();
    let perms = compiled_role_default(&RoomRole::Creator, &settings);
    assert_eq!(perms.0, RoomPermissionSet::all().0);
}

#[test]
fn test_room_level_add_permissions_for_member() {
    let settings = RoomSettings {
        member_added_permissions: MemberAddedPermissions(RoomMemberPermissionBits::CHAT),
        ..RoomSettings::default()
    };
    let perms = PermissionService::calculate_role_default_permissions_from_base(
        &RoomRole::Member,
        &settings,
        RoomPermissionSet::empty(),
    );
    assert!(perms.has(crate::models::RoomPermission::CHAT));
}

#[test]
fn test_room_level_remove_permissions_for_member() {
    let settings = RoomSettings {
        member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Member, &settings);
    assert!(!perms.has(crate::models::RoomPermission::CHAT));
    assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
}

#[test]
fn test_room_level_add_and_remove_for_admin() {
    let settings = RoomSettings {
        admin_added_permissions: AdminAddedPermissions(RoomAdminPermissionBits::PLAY_CONTROL),
        admin_removed_permissions: AdminRemovedPermissions(RoomAdminPermissionBits::KICK_MEMBER),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Admin, &settings);
    assert!(perms.has(crate::models::RoomPermission::PLAY_CONTROL));
    assert!(!perms.has(crate::models::RoomPermission::KICK_MEMBER));
}

#[test]
fn test_room_overrides_do_not_affect_creator() {
    let settings = RoomSettings {
        admin_removed_permissions: AdminRemovedPermissions(RoomAdminPermissionBits::ALL),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Creator, &settings);
    assert_eq!(perms.0, RoomPermissionSet::all().0);
}

#[test]
fn test_member_allow_pattern() {
    let mut member = make_member(RoomRole::Member);
    member.added_permissions = RoomMemberPermissionBits::CHAT;
    let role_default = RoomPermissionSet::empty();
    let effective = member.effective_permissions(role_default);
    assert!(effective.has(crate::models::RoomPermission::CHAT));
    assert!(!effective.has(crate::models::RoomPermission::KICK_MEMBER));
}

#[test]
fn test_member_deny_pattern() {
    let mut member = make_member(RoomRole::Member);
    member.removed_permissions = RoomMemberPermissionBits::CHAT;
    let role_default = RoomPermissionSet::default_member();
    let effective = member.effective_permissions(role_default);
    assert!(!effective.has(crate::models::RoomPermission::CHAT));
    assert!(effective.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
}

#[test]
fn test_admin_uses_admin_overrides() {
    let mut member = make_member(RoomRole::Admin);
    member.admin_added_permissions = RoomAdminPermissionBits::PLAY_CONTROL;
    member.admin_removed_permissions = RoomAdminPermissionBits::KICK_MEMBER;
    member.added_permissions = RoomMemberPermissionBits::USE_WEBRTC;

    let role_default = RoomPermissionSet::default_admin();
    let effective = member.effective_permissions(role_default);
    assert!(effective.has(crate::models::RoomPermission::PLAY_CONTROL));
    assert!(!effective.has(crate::models::RoomPermission::KICK_MEMBER));
}

#[test]
fn test_creator_ignores_all_overrides() {
    let mut member = make_member(RoomRole::Creator);
    member.removed_permissions = RoomMemberPermissionBits::ALL;
    member.admin_removed_permissions = RoomAdminPermissionBits::ALL;
    let role_default = RoomPermissionSet::empty();
    let effective = member.effective_permissions(role_default);
    assert_eq!(effective.0, RoomPermissionSet::all().0);
}

#[test]
fn test_guest_allow_deny_pattern() {
    let mut member = make_member(RoomRole::Guest);
    member.added_permissions = RoomGuestPermissionBits::USE_WEBRTC;
    let role_default = RoomPermissionSet::default_guest();
    let effective = member.effective_permissions(role_default);
    assert!(effective.has(crate::models::RoomPermission::USE_WEBRTC));
    assert!(!effective.has(crate::models::RoomPermission::CHAT));
    assert!(!effective.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
}

#[test]
fn test_three_layer_permission_chain() {
    // Layer 2: Room adds USE_WEBRTC, removes CHAT
    let settings = RoomSettings {
        member_added_permissions: MemberAddedPermissions(RoomMemberPermissionBits::USE_WEBRTC),
        member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
        ..RoomSettings::default()
    };
    let role_default = PermissionService::calculate_role_default_permissions_from_base(
        &RoomRole::Member,
        &settings,
        RoomPermissionSet(
            RoomAdminPermissionBits::CHAT | RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
        ),
    );
    assert!(role_default.has(crate::models::RoomPermission::USE_WEBRTC));
    assert!(!role_default.has(crate::models::RoomPermission::CHAT));

    // Layer 3: Member re-adds CHAT, removes CREATE_MEDIA_RESOURCE
    let mut member = make_member(RoomRole::Member);
    member.added_permissions = RoomMemberPermissionBits::CHAT;
    member.removed_permissions = RoomMemberPermissionBits::CREATE_MEDIA_RESOURCE;

    let effective = member.effective_permissions(role_default);
    assert!(effective.has(crate::models::RoomPermission::CHAT));
    assert!(!effective.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(effective.has(crate::models::RoomPermission::USE_WEBRTC));
    assert!(!effective.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
}

#[test]
fn test_cache_degraded_flag_toggling() {
    let degraded = AtomicBool::new(false);
    degraded.store(true, Ordering::Release);
    assert!(degraded.load(Ordering::Acquire));
    degraded.store(false, Ordering::Release);
    assert!(!degraded.load(Ordering::Acquire));
}

#[test]
fn test_flush_rate_limit_allows_after_interval() {
    let last_flush =
        parking_lot::Mutex::new(Instant::now().checked_sub(Duration::from_secs(20)).unwrap());
    let elapsed = last_flush.lock().elapsed();
    assert!(elapsed >= Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS));
}

#[test]
fn test_flush_rate_limit_blocks_within_interval() {
    let last_flush = parking_lot::Mutex::new(Instant::now());
    let elapsed = last_flush.lock().elapsed();
    assert!(elapsed < Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS));
}

#[test]
fn test_has_all_requires_all_bits() {
    let perms = RoomPermissionSet(
        crate::models::RoomAdminPermissionBits::CHAT
            | crate::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE,
    );
    assert!(perms.has_all(RoomPermissionSet(
        crate::models::RoomAdminPermissionBits::CHAT
            | crate::models::RoomAdminPermissionBits::CREATE_MEDIA_RESOURCE
    )));
    assert!(!perms.has_all(RoomPermissionSet(
        crate::models::RoomAdminPermissionBits::CHAT
            | crate::models::RoomAdminPermissionBits::KICK_MEMBER
    )));
}

#[test]
fn test_has_any_requires_any_bit() {
    let perms = RoomPermissionSet(crate::models::RoomAdminPermissionBits::CHAT);
    assert!(perms.has_any(RoomPermissionSet(
        crate::models::RoomAdminPermissionBits::CHAT
            | crate::models::RoomAdminPermissionBits::KICK_MEMBER
    )));
    assert!(!perms.has_any(RoomPermissionSet(
        crate::models::RoomAdminPermissionBits::KICK_MEMBER
            | crate::models::RoomAdminPermissionBits::SET_ROOM_SETTINGS
    )));
}

#[test]
fn test_room_rejects_chat_for_guest() {
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(1 << 21),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Guest, &settings);
    assert!(!perms.has(crate::models::RoomPermission::CHAT));
    assert!(!perms.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
}

#[test]
fn test_room_adds_webrtc_for_guest() {
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Guest, &settings);
    assert!(perms.has(crate::models::RoomPermission::USE_WEBRTC));
    assert!(!perms.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
}

#[test]
fn test_room_removes_view_media_resources_for_guest() {
    let settings = RoomSettings {
        guest_removed_permissions: GuestRemovedPermissions(1 << 21),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Guest, &settings);
    assert!(!perms.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
}

#[test]
fn test_empty_permissions_has_nothing() {
    let perms = RoomPermissionSet::empty();
    assert!(!perms.has(crate::models::RoomPermission::CHAT));
    assert!(!perms.has_any(RoomPermissionSet::all()));
    assert!(perms.has_all(RoomPermissionSet::empty())); // vacuously true
}

#[test]
fn test_three_layer_guest_chain() {
    // Layer 1: Global defaults for Guest (no media resource permissions)
    // Layer 2: Room adds WebRTC for guests
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
        ..RoomSettings::default()
    };
    let role_default = compiled_role_default(&RoomRole::Guest, &settings);
    assert!(!role_default.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    assert!(role_default.has(crate::models::RoomPermission::USE_WEBRTC));

    // Layer 3: Per-actor removal can still remove guest-level permissions.
    let mut member = make_member(RoomRole::Guest);
    member.removed_permissions = crate::models::RoomGuestPermissionBits::USE_WEBRTC;
    let effective = member.effective_permissions(role_default);
    assert!(!effective.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    assert!(!effective.has(crate::models::RoomPermission::USE_WEBRTC));
}

#[test]
fn test_three_layer_admin_chain() {
    // Layer 2: Room removes KICK_MEMBER for admins
    let settings = RoomSettings {
        admin_removed_permissions: AdminRemovedPermissions(RoomAdminPermissionBits::KICK_MEMBER),
        ..RoomSettings::default()
    };
    let role_default = compiled_role_default(&RoomRole::Admin, &settings);
    assert!(!role_default.has(crate::models::RoomPermission::KICK_MEMBER));
    assert!(role_default.has(crate::models::RoomPermission::SET_MEMBER_PERMISSIONS));

    // Layer 3: Admin-level re-adds KICK_MEMBER (specific admin override)
    let mut member = make_member(RoomRole::Admin);
    member.admin_added_permissions = RoomAdminPermissionBits::KICK_MEMBER;
    let effective = member.effective_permissions(role_default);
    assert!(effective.has(crate::models::RoomPermission::KICK_MEMBER));
    assert!(effective.has(crate::models::RoomPermission::SET_MEMBER_PERMISSIONS));
}

#[test]
fn test_creator_ignores_member_level_deny() {
    let mut member = make_member(RoomRole::Creator);
    member.removed_permissions = RoomMemberPermissionBits::ALL;
    member.admin_removed_permissions = RoomAdminPermissionBits::ALL;
    member.added_permissions = 0;
    member.admin_added_permissions = 0;

    // Even with everything denied, Creator still has ALL
    let role_default = RoomPermissionSet::empty();
    let effective = member.effective_permissions(role_default);
    assert_eq!(effective.0, RoomPermissionSet::all().0);
}

#[test]
fn test_creator_always_all_regardless_of_room_settings() {
    let settings = RoomSettings {
        admin_removed_permissions: AdminRemovedPermissions(RoomAdminPermissionBits::ALL),
        member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::ALL),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Creator, &settings);
    assert_eq!(perms.0, RoomPermissionSet::all().0);
}

#[test]
fn test_admin_ignores_member_level_added_permissions() {
    let mut member = make_member(RoomRole::Admin);
    // Set member-level overrides (should be ignored for Admin role)
    member.added_permissions = RoomMemberPermissionBits::USE_WEBRTC;
    member.removed_permissions = RoomMemberPermissionBits::CHAT;
    // Admin-level overrides: these should apply
    member.admin_added_permissions = 0;
    member.admin_removed_permissions = 0;

    let role_default = RoomPermissionSet::default_admin();
    let effective = member.effective_permissions(role_default);
    // DEFAULT_MEMBER already includes USE_WEBRTC; member-level grant is redundant and ignored.
    assert!(effective.has(crate::models::RoomPermission::USE_WEBRTC));
    // member-level CHAT deny should NOT apply to admin
    assert!(effective.has(crate::models::RoomPermission::CHAT));
}

#[test]
fn test_member_ignores_admin_level_permissions() {
    let mut member = make_member(RoomRole::Member);
    // Set admin-level overrides (should be ignored for Member role)
    member.admin_added_permissions = 1 << 21;
    member.admin_removed_permissions = RoomAdminPermissionBits::CHAT;
    // Member-level overrides: these should apply
    member.added_permissions = 0;
    member.removed_permissions = 0;

    let role_default = RoomPermissionSet::default_member();
    let effective = member.effective_permissions(role_default);
    // admin-level overrides should NOT apply
    assert!(!effective.has(crate::models::RoomPermission::KICK_MEMBER));
    // admin-level CHAT deny should NOT apply
    assert!(effective.has(crate::models::RoomPermission::CHAT));
}

#[test]
fn test_room_level_add_and_remove_same_permission_for_member() {
    let settings = RoomSettings {
        member_added_permissions: MemberAddedPermissions(RoomMemberPermissionBits::CHAT),
        member_removed_permissions: MemberRemovedPermissions(RoomMemberPermissionBits::CHAT),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Member, &settings);
    // Remove is applied after add, so CHAT should be absent
    assert!(!perms.has(crate::models::RoomPermission::CHAT));
}

#[test]
fn test_room_level_add_and_remove_same_permission_for_guest() {
    let settings = RoomSettings {
        guest_added_permissions: GuestAddedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
        guest_removed_permissions: GuestRemovedPermissions(RoomGuestPermissionBits::USE_WEBRTC),
        ..RoomSettings::default()
    };
    let perms = compiled_role_default(&RoomRole::Guest, &settings);
    // Remove wins over add
    assert!(!perms.has(crate::models::RoomPermission::USE_WEBRTC));
}

#[test]
fn test_permission_bits_grant_revoke() {
    let mut perms = RoomPermissionSet(0);
    perms.grant(crate::models::RoomPermission::CHAT);
    assert!(perms.has(crate::models::RoomPermission::CHAT));

    perms.grant(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE);
    assert!(perms.has(crate::models::RoomPermission::CHAT));
    assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));

    perms.revoke(crate::models::RoomPermission::CHAT);
    assert!(!perms.has(crate::models::RoomPermission::CHAT));
    assert!(perms.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
}

#[test]
fn test_permission_bits_all_contains_every_named_permission() {
    let all = RoomPermissionSet::all();
    assert!(all.has(crate::models::RoomPermission::CHAT));
    assert!(all.has(crate::models::RoomPermission::CREATE_MEDIA_RESOURCE));
    assert!(all.has(crate::models::RoomPermission::KICK_MEMBER));
    assert!(all.has(crate::models::RoomPermission::USE_WEBRTC));
    assert!(all.has(crate::models::RoomPermission::VIEW_MEDIA_RESOURCES));
    assert!(all.has(crate::models::RoomPermission::PLAY_CONTROL));
}

#[test]
fn test_default_runtime_has_no_room_settings_repo() {
    let service = make_service();
    assert!(!service.has_room_settings_repo());
}

#[test]
fn test_invalidation_service_configured_at_construction_propagates_to_clones() {
    let invalidation_service = Arc::new(crate::cache::CacheInvalidationService::new(
        "permission-clone-node".to_string(),
        "permission-clone-stream".to_string(),
    ));
    let service = make_service_with_runtime(PermissionServiceRuntime {
        invalidation_service: Some(invalidation_service),
        ..PermissionServiceRuntime::default()
    });
    let cloned = service.clone();

    assert!(
        service.has_invalidation_service(),
        "original service must observe the injected invalidation service"
    );
    assert!(
        cloned.has_invalidation_service(),
        "cloned permission services must share the injected invalidation service"
    );
}

#[test]
fn test_first_flush_allowed_immediately_after_startup() {
    let service = make_service();

    // Immediately after construction, a flush should be allowed
    // (last_flush_time is initialized to the past, not Instant::now())
    let elapsed = service.last_flush_time.lock().elapsed();
    assert!(
        elapsed >= Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS),
        "First flush should be allowed immediately after startup, \
         but elapsed={elapsed:?} < FLUSH_RATE_LIMIT_SECS={}s",
        PermissionService::FLUSH_RATE_LIMIT_SECS
    );
}

#[test]
fn test_flush_rate_limit_blocks_rapid_second_flush() {
    let service = make_service();

    // Simulate a flush happening "now"
    *service.last_flush_time.lock() = Instant::now();

    // Immediately after, a second flush should be blocked
    let elapsed = service.last_flush_time.lock().elapsed();
    assert!(
        elapsed < Duration::from_secs(PermissionService::FLUSH_RATE_LIMIT_SECS),
        "Second flush immediately after first should be blocked"
    );
}

fn make_service_with_invalidation() -> (
    PermissionService,
    Arc<crate::cache::CacheInvalidationService>,
) {
    use crate::cache::CacheInvalidationService;

    let invalidation_service = Arc::new(CacheInvalidationService::new(
        // No Redis - local only
        "test-node".to_string(),
        "test-stream".to_string(),
    ));

    let service = make_service_with_runtime(PermissionServiceRuntime {
        invalidation_service: Some(invalidation_service.clone()),
        ..PermissionServiceRuntime::default()
    });

    (service, invalidation_service)
}

fn permission_service_with_invalidation(
    invalidation_service: Arc<dyn CacheInvalidationRuntime>,
) -> PermissionService {
    make_service_with_runtime(PermissionServiceRuntime {
        invalidation_service: Some(invalidation_service),
        ..PermissionServiceRuntime::default()
    })
}

#[tokio::test]
async fn test_runtime_invalidation_does_not_start_tasks_until_explicit_start() {
    let (service, _invalidation_service) = make_service_with_invalidation();

    assert!(
        !service.invalidation_tasks_started(),
        "permission service construction must not spawn background tasks"
    );

    service.start().await.expect("start should succeed");

    assert!(
        service.invalidation_tasks_started(),
        "start() must mark invalidation tasks as running"
    );

    service.shutdown().await;

    assert!(
        !service.invalidation_tasks_started(),
        "shutdown() must reset invalidation runtime state"
    );
}

#[tokio::test(start_paused = true)]
async fn test_degraded_mode_auto_recovers_after_timeout_and_flushes_caches() {
    let (service, _invalidation_service) = make_service_with_invalidation();

    service.cache_degraded.store(true, Ordering::Release);
    *service.degradation_started.lock() = Some(
        Instant::now()
            .checked_sub(Duration::from_secs(11))
            .expect("backdating degradation start should succeed"),
    );

    service.start().await.expect("start should succeed");

    tokio::task::yield_now().await;

    assert!(
        !service.cache_degraded.load(Ordering::Acquire),
        "permission source cache should leave degraded mode after the bounded recovery timeout"
    );
    assert!(
        service.degradation_started.lock().is_none(),
        "auto-recovery must clear degradation start time"
    );

    service.shutdown().await;
}

#[tokio::test]
async fn test_degraded_mode_recovers_on_invalidation_message() {
    let (service, invalidation_service) = make_service_with_invalidation();

    service.cache_degraded.store(true, Ordering::Release);
    *service.degradation_started.lock() = Some(Instant::now());

    service.start().await.expect("start should succeed");

    invalidation_service
        .broadcast_all(InvalidationMessage::All)
        .await
        .expect("local invalidation broadcast should succeed");
    tokio::task::yield_now().await;

    assert!(
        !service.cache_degraded.load(Ordering::Acquire),
        "permission cache must leave degraded mode after a real invalidation message arrives"
    );
    assert!(
        service.degradation_started.lock().is_none(),
        "recovery on a real invalidation message must clear degradation start time"
    );

    service.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn test_shutdown_aborts_stuck_invalidation_tasks() {
    let service = make_service_async();
    service
        .invalidation_runtime
        .started
        .store(true, Ordering::Release);

    *service.invalidation_runtime.listener_handle.lock().await = Some(tokio::spawn(async {
        std::future::pending::<()>().await;
    }));

    let shutdown = tokio::spawn({
        let service = service.clone();
        async move {
            service.shutdown().await;
        }
    });

    tokio::task::yield_now().await;
    tokio::time::advance(PermissionService::INVALIDATION_TASK_SHUTDOWN_TIMEOUT).await;
    tokio::task::yield_now().await;

    shutdown
        .await
        .expect("shutdown task should finish after aborting the stuck listener");

    assert!(
        !service.invalidation_runtime.started.load(Ordering::Acquire),
        "shutdown must reset runtime state even when it had to abort a task"
    );
    assert!(
        service
            .invalidation_runtime
            .listener_handle
            .lock()
            .await
            .is_none(),
        "shutdown must drain the stuck listener handle after aborting it"
    );
}

#[tokio::test]
async fn test_start_can_restart_after_shutdown() {
    let (service, invalidation_service) = make_service_with_invalidation();

    service.start().await.expect("initial start should succeed");
    service.shutdown().await;

    service.cache_degraded.store(true, Ordering::Release);
    *service.degradation_started.lock() = Some(Instant::now());

    service
        .start()
        .await
        .expect("restart after shutdown should succeed");

    invalidation_service
        .broadcast_all(InvalidationMessage::All)
        .await
        .expect("local invalidation broadcast should succeed after restart");
    tokio::task::yield_now().await;

    assert!(
        !service.cache_degraded.load(Ordering::Acquire),
        "restart must install fresh listener tasks that can recover degraded mode"
    );

    service.shutdown().await;
}

#[test]
fn test_invalidate_cache_local_clear_works() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (service, _invalidation_service) = make_service_with_invalidation();
        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(1);
        let cache_key = MemberPermissionKey::new(room_id, user_id);

        service
            .member_permission_cache
            .set_if_version_at_least(
                &cache_key,
                CachedMemberPermissionSource {
                    room_id,
                    user_id,
                    role: RoomRole::Member,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    version: 1,
                },
            )
            .await
            .unwrap();
        assert!(service
            .member_permission_cache
            .get_l1(&cache_key)
            .await
            .is_some());

        service.invalidate_cache(&room_id, &user_id).await;

        assert!(service
            .member_permission_cache
            .get_l1(&cache_key)
            .await
            .is_none());
    });
}

#[test]
fn test_invalidate_room_cache_local_clear_works() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (service, invalidation_service) = make_service_with_invalidation();
        let mut receiver = invalidation_service.subscribe();
        let room_id = RoomId::expect_positive(1);

        service.invalidate_room_cache(&room_id).await;

        let result =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv()).await;

        match result {
            Ok(Ok(InvalidationMessage::RoomPermission { room_id: rid })) => {
                assert_eq!(rid, "1");
            }
            Ok(Ok(other)) => {
                panic!("Expected RoomPermission message, got {other:?}");
            }
            Ok(Err(e)) => {
                panic!("Receiver error: {e:?}");
            }
            Err(timeout_error) => {
                panic!("Timeout waiting for broadcast: {timeout_error:?}");
            }
        }
    });
}

#[test]
fn test_clear_cache_local_clear_works() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (service, _invalidation_service) = make_service_with_invalidation();
        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(1);
        let cache_key = MemberPermissionKey::new(room_id, user_id);

        service
            .member_permission_cache
            .set_if_version_at_least(
                &cache_key,
                CachedMemberPermissionSource {
                    room_id,
                    user_id,
                    role: RoomRole::Member,
                    added_permissions: 0,
                    removed_permissions: 0,
                    admin_added_permissions: 0,
                    admin_removed_permissions: 0,
                    version: 1,
                },
            )
            .await
            .unwrap();
        assert!(service
            .member_permission_cache
            .get_l1(&cache_key)
            .await
            .is_some());

        service.clear_cache().await;

        assert!(service
            .member_permission_cache
            .get_l1(&cache_key)
            .await
            .is_none());
    });
}

#[test]
fn test_invalidate_cache_no_panic_without_invalidation_service() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let service = make_service_async();
        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(1);
        service.invalidate_cache(&room_id, &user_id).await;
    });
}

#[test]
fn test_invalidate_cache_receives_broadcast_after_fix() {
    use crate::cache::{CacheInvalidationService, InvalidationMessage};

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // Create a CacheInvalidationService without Redis
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            // No Redis
            "test-node".to_string(),
            "test-stream".to_string(),
        ));

        // Subscribe to receive invalidation messages
        let mut receiver = invalidation_service.subscribe();

        let service = permission_service_with_invalidation(invalidation_service.clone());

        let room_id = RoomId::expect_positive(1);
        let user_id = UserId::expect_positive(1);

        // Invalidate the cache - this should broadcast via invalidation_service
        service.invalidate_cache(&room_id, &user_id).await;

        // Try to receive the broadcast message
        // invalidate_cache broadcasts both locally and to Redis. Since there's
        // no Redis here, only local broadcast happens and should be received.
        let result =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv()).await;

        // After the fix, this should receive the message
        match result {
            Ok(Ok(InvalidationMessage::UserPermission {
                room_id: rid,
                user_id: uid,
            })) => {
                assert_eq!(rid, "1");
                assert_eq!(uid, "1");
                // Success! The broadcast was received.
            }
            Ok(Ok(other)) => {
                panic!("Expected UserPermission message, got {other:?}");
            }
            Ok(Err(e)) => {
                panic!("Receiver error: {e:?}");
            }
            Err(timeout_error) => {
                panic!(
                    "Timeout waiting for broadcast ({timeout_error:?}) - this indicates \
                     invalidate_cache is not broadcasting locally. It should use \
                     invalidate_and_broadcast_user_permission."
                );
            }
        }
    });
}

#[test]
fn test_invalidate_room_cache_receives_broadcast_after_fix() {
    use crate::cache::{CacheInvalidationService, InvalidationMessage};

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // Create a CacheInvalidationService without Redis
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            // No Redis
            "test-node".to_string(),
            "test-stream".to_string(),
        ));

        // Subscribe to receive invalidation messages
        let mut receiver = invalidation_service.subscribe();

        let service = permission_service_with_invalidation(invalidation_service.clone());

        // Invalidate the room cache - this should broadcast via invalidation_service
        let room_id = RoomId::expect_positive(1);
        service.invalidate_room_cache(&room_id).await;

        // Try to receive the broadcast message
        let result =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv()).await;

        // After the fix, this should receive the message
        match result {
            Ok(Ok(InvalidationMessage::RoomPermission { room_id: rid })) => {
                assert_eq!(rid, "1");
                // Success! The broadcast was received.
            }
            Ok(Ok(other)) => {
                panic!("Expected RoomPermission message, got {other:?}");
            }
            Ok(Err(e)) => {
                panic!("Receiver error: {e:?}");
            }
            Err(timeout_error) => {
                panic!(
                    "Timeout waiting for broadcast ({timeout_error:?}) - this indicates \
                     invalidate_room_cache is not broadcasting locally. It should use \
                     invalidate_and_broadcast_room_permission."
                );
            }
        }
    });
}

#[test]
fn test_clear_cache_receives_broadcast_after_fix() {
    use crate::cache::{CacheInvalidationService, InvalidationMessage};

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        // Create a CacheInvalidationService without Redis
        let invalidation_service = Arc::new(CacheInvalidationService::new(
            // No Redis
            "test-node".to_string(),
            "test-stream".to_string(),
        ));

        // Subscribe to receive invalidation messages
        let mut receiver = invalidation_service.subscribe();

        let service = permission_service_with_invalidation(invalidation_service.clone());

        // Clear the cache - this should broadcast via invalidation_service
        service.clear_cache().await;

        // Try to receive the broadcast message
        let result =
            tokio::time::timeout(tokio::time::Duration::from_millis(100), receiver.recv()).await;

        // After the fix, this should receive the message
        match result {
            Ok(Ok(InvalidationMessage::All)) => {
                // Success! The broadcast was received.
            }
            Ok(Ok(other)) => {
                panic!("Expected All message, got {other:?}");
            }
            Ok(Err(e)) => {
                panic!("Receiver error: {e:?}");
            }
            Err(timeout_error) => {
                panic!(
                    "Timeout waiting for broadcast ({timeout_error:?}) - this indicates \
                     clear_cache is not broadcasting locally. It should use broadcast_all."
                );
            }
        }
    });
}
