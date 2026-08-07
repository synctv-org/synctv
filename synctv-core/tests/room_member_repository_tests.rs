//! `RoomMemberRepository` integration tests
//!
//! Tests the core room member operations: `add_with_options`, role-check remove,
//! atomic permission grants/revokes, permission reset, batch counts, pagination,
//! and `diagnose_add_conflict` error branches.
//!
use chrono::Utc;
use synctv_core::{
    models::{
        AddMemberOptions, MemberStatus, MyRoomListQuery, MyRoomListSortBy, PageParams, Room,
        RoomId, RoomMember, RoomRole, RoomStatus, SortDirection, User, UserId, UserRole,
        UserStatus,
    },
    repository::{
        room_member::KickCooldownInsert, RoomMemberRepository, RoomRepository, UserRepository,
    },
    Error,
};
use synctv_core_testing::{create_test_pool, TestOptionExt, TestResultExt};

fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        role: UserRole::User,
        avatar_file_reference_id: None,
        status: UserStatus::Active,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        version: 0,
        deleted_at: None,
        is_banned: false,
        banned_at: None,
        banned_by: None,
        banned_reason: None,
    }
}

fn make_room(name: &str, owner: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: "test room".to_string(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: *owner,
        status: RoomStatus::Active,
        is_banned: false,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

fn make_member(room_id: RoomId, user_id: UserId, role: RoomRole) -> RoomMember {
    RoomMember::new(room_id, user_id, role)
}

async fn add_kick_cooldown(
    member_repo: &RoomMemberRepository,
    room_id: RoomId,
    user_id: UserId,
    kicked_by: Option<UserId>,
) {
    let now = Utc::now();
    member_repo
        .add_kick_cooldown_with_executor(
            KickCooldownInsert {
                room_id: &room_id,
                user_id: &user_id,
                kicked_by: kicked_by.as_ref(),
                starts_at: now,
                ends_at: now + chrono::Duration::hours(1),
                reason: Some("test kick"),
            },
            member_repo.pool(),
        )
        .await
        .checked("test operation should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_full_flow() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_awo"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room AWO", &owner.id))
        .await
        .checked("test operation should succeed");
    let joiner = user_repo
        .create(&make_user("joiner_awo"))
        .await
        .checked("test operation should succeed");

    let member = make_member(room.id, joiner.id, RoomRole::Member);
    let options = AddMemberOptions::new();

    let result = member_repo
        .add_with_options(&member, &options)
        .await
        .checked("test operation should succeed");
    assert_eq!(result.user_id, joiner.id);
    assert_eq!(result.role, RoomRole::Member);
    assert_eq!(result.status, MemberStatus::Active);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_my_room_queries_preserve_cover_reference() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_room_cover"))
        .await
        .checked("owner should be created");
    let viewer = user_repo
        .create(&make_user("viewer_room_cover"))
        .await
        .checked("viewer should be created");
    let mut room = room_repo
        .create(&make_room("Covered Room", &owner.id))
        .await
        .checked("room should be created");
    member_repo
        .add(&make_member(room.id, viewer.id, RoomRole::Member))
        .await
        .checked("viewer should join room");

    sqlx::query(
        r"INSERT INTO file_objects (
               storage_backend, object_key, mime_type, size_bytes,
               content_manifest_sha256, metadata, validated_at
           ) VALUES ($1, $2, 'image/jpeg', 1, $3, '{}'::jsonb, CURRENT_TIMESTAMP)",
    )
    .bind("database")
    .bind("tests/room-cover.jpg")
    .bind("0".repeat(64))
    .execute(&pool)
    .await
    .checked("cover object should be created");
    let cover_reference_id: i64 = sqlx::query_scalar(
        r"INSERT INTO file_references (
               storage_backend, object_key, reference_kind, reference_id, metadata
           ) VALUES ($1, $2, 'room_cover', $3, '{}'::jsonb)
           RETURNING id",
    )
    .bind("database")
    .bind("tests/room-cover.jpg")
    .bind(room.id.as_i64().to_string())
    .fetch_one(&pool)
    .await
    .checked("cover reference should be created");
    let old_version = room.version;
    room.cover_file_reference_id = Some(cover_reference_id);
    room_repo
        .update(&room, old_version)
        .await
        .checked("room cover reference should be assigned");

    let query = MyRoomListQuery {
        pagination: PageParams::new(Some(1), Some(10)),
        ..Default::default()
    };
    let (primary_rooms, _) = member_repo
        .list_by_user_with_query(&viewer.id, &query)
        .await
        .checked("primary my-room query should succeed");
    let (read_rooms, _) = member_repo
        .list_accessible_by_user_with_query_eventually_consistent(&viewer.id, &query)
        .await
        .checked("read-pool my-room query should succeed");

    assert_eq!(
        primary_rooms[0].0.cover_file_reference_id,
        Some(cover_reference_id)
    );
    assert_eq!(
        read_rooms[0].0.cover_file_reference_id,
        Some(cover_reference_id)
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_record_visit_deduplicates_and_drives_room_sorting() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_room_visits"))
        .await
        .checked("test operation should succeed");
    let visitor = user_repo
        .create(&make_user("visitor_room_visits"))
        .await
        .checked("test operation should succeed");
    let frequent_room = room_repo
        .create(&make_room("Frequent Room", &owner.id))
        .await
        .checked("test operation should succeed");
    let recent_room = room_repo
        .create(&make_room("Recent Room", &owner.id))
        .await
        .checked("test operation should succeed");

    for room in [&frequent_room, &recent_room] {
        member_repo
            .add(&make_member(room.id, visitor.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
    }

    let first_visit = Utc::now() - chrono::Duration::hours(1);
    assert!(member_repo
        .record_visit(
            &frequent_room.id,
            &visitor.id,
            first_visit - chrono::Duration::minutes(30),
            first_visit,
        )
        .await
        .checked("first visit should be recorded"));

    let duplicate_visit = first_visit + chrono::Duration::minutes(5);
    member_repo
        .record_visit(
            &frequent_room.id,
            &visitor.id,
            duplicate_visit - chrono::Duration::minutes(30),
            duplicate_visit,
        )
        .await
        .checked("duplicate visit should refresh recency");

    let reconnect_visit = first_visit + chrono::Duration::minutes(20);
    member_repo
        .record_visit(
            &frequent_room.id,
            &visitor.id,
            reconnect_visit - chrono::Duration::minutes(30),
            reconnect_visit,
        )
        .await
        .checked("reconnect should preserve the original counting window");

    let second_counted_visit = first_visit + chrono::Duration::minutes(40);
    member_repo
        .record_visit(
            &frequent_room.id,
            &visitor.id,
            second_counted_visit - chrono::Duration::minutes(30),
            second_counted_visit,
        )
        .await
        .checked("visit outside the counting window should increment frequency");

    let most_recent_visit = first_visit + chrono::Duration::minutes(45);
    member_repo
        .record_visit(
            &recent_room.id,
            &visitor.id,
            most_recent_visit - chrono::Duration::minutes(30),
            most_recent_visit,
        )
        .await
        .checked("recent room visit should be recorded");

    let visit_count: i64 = sqlx::query_scalar(
        "SELECT visit_count FROM room_members WHERE room_id = $1 AND user_id = $2",
    )
    .bind(frequent_room.id)
    .bind(visitor.id)
    .fetch_one(&pool)
    .await
    .checked("visit count should be readable");
    assert_eq!(visit_count, 2);

    room_repo
        .favorite_for_user(&visitor.id, &frequent_room.id)
        .await
        .checked("frequent room should be favorited");
    room_repo
        .favorite_for_user(&visitor.id, &recent_room.id)
        .await
        .checked("recent room should be favorited");
    let (favorite_results, favorite_total) = room_repo
        .list_favorites_for_user(&visitor.id, PageParams::new(Some(1), Some(10)), None)
        .await
        .checked("favorite room sorting should succeed");
    assert_eq!(favorite_total, 2);
    assert_eq!(favorite_results[0].id, frequent_room.id);

    let base_query = MyRoomListQuery {
        pagination: PageParams::new(Some(1), Some(10)),
        sort_direction: SortDirection::Desc,
        ..Default::default()
    };
    let (frequent_results, _) = member_repo
        .list_by_user_with_query(
            &visitor.id,
            &MyRoomListQuery {
                sort_by: MyRoomListSortBy::Frequent,
                ..base_query.clone()
            },
        )
        .await
        .checked("frequent room sorting should succeed");
    assert_eq!(frequent_results[0].0.id, frequent_room.id);

    let (recent_results, _) = member_repo
        .list_by_user_with_query(
            &visitor.id,
            &MyRoomListQuery {
                sort_by: MyRoomListSortBy::LastVisitedAt,
                ..base_query
            },
        )
        .await
        .checked("recent room sorting should succeed");
    assert_eq!(recent_results[0].0.id, recent_room.id);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_capacity_at_max_members() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_cap"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Cap", &owner.id))
        .await
        .checked("test operation should succeed");

    // Add one member to fill the room (max_members=1)
    let user1 = user_repo
        .create(&make_user("user1_cap"))
        .await
        .checked("test operation should succeed");
    let m1 = make_member(room.id, user1.id, RoomRole::Member);
    let options_fill = AddMemberOptions::new().with_max_members(1);
    member_repo
        .add_with_options(&m1, &options_fill)
        .await
        .checked("test operation should succeed");

    // Second member should be rejected
    let user2 = user_repo
        .create(&make_user("user2_cap"))
        .await
        .checked("test operation should succeed");
    let m2 = make_member(room.id, user2.id, RoomRole::Member);
    let options_reject = AddMemberOptions::new().with_max_members(1);
    let err = member_repo
        .add_with_options(&m2, &options_reject)
        .await
        .failed("operation should fail");
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_removed_members_do_not_consume_capacity() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_pending_capacity"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Pending Capacity", &owner.id))
        .await
        .checked("test operation should succeed");

    let removed_user = user_repo
        .create(&make_user("user_removed_capacity"))
        .await
        .checked("test operation should succeed");
    let departed_member = make_member(room.id, removed_user.id, RoomRole::Member);
    member_repo
        .add_with_options(&departed_member, &AddMemberOptions::new())
        .await
        .checked("test operation should succeed");
    member_repo
        .remove(&room.id, &removed_user.id)
        .await
        .checked("fixture member should be removed");

    let active_user = user_repo
        .create(&make_user("user_active_capacity"))
        .await
        .checked("test operation should succeed");
    let active_member = make_member(room.id, active_user.id, RoomRole::Member);

    member_repo
        .add_with_options(&active_member, &AddMemberOptions::new().with_max_members(1))
        .await
        .checked("pending members must not count against max_members");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_inactive_room_rejection() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_inactive"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Inactive", &owner.id))
        .await
        .checked("test operation should succeed");

    // Close the room
    room_repo
        .update_status(&room.id, RoomStatus::Closed)
        .await
        .checked("test operation should succeed");

    let joiner = user_repo
        .create(&make_user("joiner_inactive"))
        .await
        .checked("test operation should succeed");
    let member = make_member(room.id, joiner.id, RoomRole::Member);
    let options = AddMemberOptions::new();

    let err = member_repo
        .add_with_options(&member, &options)
        .await
        .failed("operation should fail");
    assert!(matches!(err, Error::InvalidInput(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_duplicate_membership_check() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_dup"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Dup", &owner.id))
        .await
        .checked("test operation should succeed");
    let joiner = user_repo
        .create(&make_user("joiner_dup"))
        .await
        .checked("test operation should succeed");

    let member = make_member(room.id, joiner.id, RoomRole::Member);
    let options = AddMemberOptions::new();

    // First join succeeds
    member_repo
        .add_with_options(&member, &options)
        .await
        .checked("test operation should succeed");

    // Second join with duplicate check fails
    let member2 = make_member(room.id, joiner.id, RoomRole::Member);
    let err = member_repo
        .add_with_options(&member2, &options)
        .await
        .failed("operation should fail");
    assert!(matches!(err, Error::AlreadyExists(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_options_max_members_zero_bypass() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_zero"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Zero", &owner.id))
        .await
        .checked("test operation should succeed");

    // max_members=0 means unlimited - even with check enabled, it bypasses
    let mut options = AddMemberOptions::new();
    options.check_max_members = true;
    options.max_members = 0;

    // Add many members - all should succeed
    for i in 0..5 {
        let user = user_repo
            .create(&make_user(&format!("user_zero_{i}")))
            .await
            .checked("test operation should succeed");
        let member = make_member(room.id, user.id, RoomRole::Member);
        member_repo
            .add_with_options(&member, &options)
            .await
            .checked("test operation should succeed");
    }

    let count = member_repo
        .count_by_room(&room.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(count, 5);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_with_role_check_member_cannot_kick_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_kick_role"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room KickRole", &owner.id))
        .await
        .checked("test operation should succeed");

    let admin_user = user_repo
        .create(&make_user("admin_kick_role"))
        .await
        .checked("test operation should succeed");
    let member_user = user_repo
        .create(&make_user("member_kick_role"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, admin_user.id, RoomRole::Admin))
        .await
        .checked("test operation should succeed");
    member_repo
        .add(&make_member(room.id, member_user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    // Member (role=3) trying to kick Admin (role=2) => should fail
    let result = member_repo
        .kick_with_role_check(&room.id, &member_user.id, &admin_user.id)
        .await
        .checked("test operation should succeed");
    assert!(!result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_with_role_check_creator_can_kick_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("creator_kick_admin"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room CreatorKickAdmin", &creator.id))
        .await
        .checked("test operation should succeed");

    // Add creator as Creator role in room_members
    member_repo
        .add(&make_member(room.id, creator.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    let admin_user = user_repo
        .create(&make_user("admin_kick_by_creator"))
        .await
        .checked("test operation should succeed");
    member_repo
        .add(&make_member(room.id, admin_user.id, RoomRole::Admin))
        .await
        .checked("test operation should succeed");

    // Creator (role=1) kicking Admin (role=2) => should succeed
    let kicked = member_repo
        .kick_with_role_check(&room.id, &creator.id, &admin_user.id)
        .await
        .checked("test operation should succeed");

    assert!(kicked);
    assert!(member_repo
        .get_any(&room.id, &admin_user.id)
        .await
        .checked("test operation should succeed")
        .is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_with_role_check_equal_rank_rejected() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_eqrank"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room EqRank", &owner.id))
        .await
        .checked("test operation should succeed");

    let admin1 = user_repo
        .create(&make_user("admin1_eqrank"))
        .await
        .checked("test operation should succeed");
    let admin2 = user_repo
        .create(&make_user("admin2_eqrank"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, admin1.id, RoomRole::Admin))
        .await
        .checked("test operation should succeed");
    member_repo
        .add(&make_member(room.id, admin2.id, RoomRole::Admin))
        .await
        .checked("test operation should succeed");

    // Admin (role=2) trying to kick another Admin (role=2) => equal rank, should fail
    let result = member_repo
        .kick_with_role_check(&room.id, &admin1.id, &admin2.id)
        .await
        .checked("test operation should succeed");
    assert!(!result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_with_role_check_self_kick_fails() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_selfkick"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room SelfKick", &owner.id))
        .await
        .checked("test operation should succeed");

    let admin_user = user_repo
        .create(&make_user("admin_selfkick"))
        .await
        .checked("test operation should succeed");
    member_repo
        .add(&make_member(room.id, admin_user.id, RoomRole::Admin))
        .await
        .checked("test operation should succeed");

    // Self-kick: actor.role == target.role (not strictly less), should fail
    let result = member_repo
        .kick_with_role_check(&room.id, &admin_user.id, &admin_user.id)
        .await
        .checked("test operation should succeed");
    assert!(!result);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_kick_with_role_check_creator_kicks_admin() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let creator = user_repo
        .create(&make_user("creator_kick"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room CreatorKick", &creator.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, creator.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    let admin_user = user_repo
        .create(&make_user("admin_kick"))
        .await
        .checked("test operation should succeed");
    member_repo
        .add(&make_member(room.id, admin_user.id, RoomRole::Admin))
        .await
        .checked("test operation should succeed");

    // Creator (role=1) kicking Admin (role=2) => should succeed
    let result = member_repo
        .kick_with_role_check(&room.id, &creator.id, &admin_user.id)
        .await
        .checked("test operation should succeed");
    assert!(result);

    // Verify admin is now gone
    let member = member_repo
        .get(&room.id, &admin_user.id)
        .await
        .checked("test operation should succeed");
    assert!(member.is_none());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_atomic_bitwise_or() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_grant"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Grant", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_grant"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    // Grant permission 0x01
    let m1 = member_repo
        .grant_permission_atomic(&room.id, &user.id, 0x01)
        .await
        .checked("test operation should succeed");
    assert_eq!(m1.added_permissions, 0x01);

    // Grant permission 0x02 (bitwise OR with existing)
    let m2 = member_repo
        .grant_permission_atomic(&room.id, &user.id, 0x02)
        .await
        .checked("test operation should succeed");
    assert_eq!(m2.added_permissions, 0x03); // 0x01 | 0x02 = 0x03
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoke_permission_atomic_bitwise_or() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_revoke"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Revoke", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_revoke"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    // Revoke permission 0x04
    let m1 = member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0x04)
        .await
        .checked("test operation should succeed");
    assert_eq!(m1.removed_permissions, 0x04);

    // Revoke permission 0x08 (bitwise OR with existing)
    let m2 = member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0x08)
        .await
        .checked("test operation should succeed");
    assert_eq!(m2.removed_permissions, 0x0C); // 0x04 | 0x08 = 0x0C
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_grant_permission_atomic_removed_member_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_removed_grant"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room RemovedGrant", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_removed_grant"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    // Remove the member
    assert!(member_repo
        .remove(&room.id, &user.id)
        .await
        .checked("test operation should succeed"));

    // Attempting to grant on removed member should return NotFound
    let err = member_repo
        .grant_permission_atomic(&room.id, &user.id, 0x01)
        .await
        .failed("operation should fail");
    assert!(matches!(err, Error::NotFound(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_revoke_permission_atomic_removed_member_returns_not_found() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_removed_revoke"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room RemovedRevoke", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_removed_revoke"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    // Remove the member
    member_repo
        .remove(&room.id, &user.id)
        .await
        .checked("test operation should succeed");

    // Attempting to revoke on removed member should return NotFound
    let err = member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0x01)
        .await
        .failed("operation should fail");
    assert!(matches!(err, Error::NotFound(_)));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_role_guarded_admin_permission_updates_fail_when_role_changed() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_role_guard"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Role Guard", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_role_guard"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Admin))
        .await
        .checked("test operation should succeed");

    let current = member_repo
        .get(&room.id, &user.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    member_repo
        .update_role(&room.id, &user.id, RoomRole::Member, current.version)
        .await
        .checked("test operation should succeed");

    let grant_err = member_repo
        .grant_admin_permission_atomic_for_role(&room.id, &user.id, 0x01, RoomRole::Admin)
        .await
        .failed("operation should fail");
    assert!(matches!(grant_err, Error::OptimisticLockConflict));

    let revoke_err = member_repo
        .revoke_admin_permission_atomic_for_role(&room.id, &user.id, 0x02, RoomRole::Admin)
        .await
        .failed("operation should fail");
    assert!(matches!(revoke_err, Error::OptimisticLockConflict));

    let refreshed = member_repo
        .get(&room.id, &user.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");
    assert_eq!(refreshed.role, RoomRole::Member);
    assert_eq!(refreshed.admin_added_permissions, 0);
    assert_eq!(refreshed.admin_removed_permissions, 0);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_reset_permissions_zeroes_all_four_columns() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_reset"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Reset", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_reset"))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    // Set various permissions
    member_repo
        .grant_permission_atomic(&room.id, &user.id, 0xFF)
        .await
        .checked("test operation should succeed");
    member_repo
        .revoke_permission_atomic(&room.id, &user.id, 0xAA)
        .await
        .checked("test operation should succeed");

    // Also set admin permissions via raw SQL (since atomic ops only touch member-level)
    sqlx::query!(
        "UPDATE room_members SET admin_added_permissions = 42, admin_removed_permissions = 84 WHERE room_id = $1 AND user_id = $2",
        room.id.as_i64(),
        user.id.as_i64()
    )
        .execute(&pool)
        .await
        .checked("test operation should succeed");

    // Get current version
    let current = member_repo
        .get(&room.id, &user.id)
        .await
        .checked("test operation should succeed")
        .checked("test operation should succeed");

    // Reset
    let reset = member_repo
        .reset_permissions(&room.id, &user.id, current.version)
        .await
        .checked("test operation should succeed");
    assert_eq!(reset.added_permissions, 0);
    assert_eq!(reset.removed_permissions, 0);
    assert_eq!(reset.admin_added_permissions, 0);
    assert_eq!(reset.admin_removed_permissions, 0);
    assert_eq!(reset.version, current.version + 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_rooms_batch_basic() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_batch"))
        .await
        .checked("test operation should succeed");
    let room1 = room_repo
        .create(&make_room("Room Batch1", &owner.id))
        .await
        .checked("test operation should succeed");
    let room2 = room_repo
        .create(&make_room("Room Batch2", &owner.id))
        .await
        .checked("test operation should succeed");
    let room3 = room_repo
        .create(&make_room("Room Batch3", &owner.id))
        .await
        .checked("test operation should succeed");

    // Add 2 members to room1
    for i in 0..2 {
        let u = user_repo
            .create(&make_user(&format!("batch_u1_{i}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room1.id, u.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
    }

    // Add 3 members to room2
    for i in 0..3 {
        let u = user_repo
            .create(&make_user(&format!("batch_u2_{i}")))
            .await
            .checked("test operation should succeed");
        member_repo
            .add(&make_member(room2.id, u.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
    }

    // room3 has 0 members

    let counts = member_repo
        .count_by_rooms_batch(&[&room1.id, &room2.id, &room3.id])
        .await
        .checked("test operation should succeed");

    assert_eq!(counts.get(&room1.id), Some(&2));
    assert_eq!(counts.get(&room2.id), Some(&3));
    // Zero-member room absent from map
    assert!(!counts.contains_key(&room3.id));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_count_by_rooms_batch_empty_input() {
    let (_container, pool) = create_test_pool().await;
    let member_repo = RoomMemberRepository::new(pool.clone());

    let counts = member_repo
        .count_by_rooms_batch(&[])
        .await
        .checked("test operation should succeed");
    assert!(counts.is_empty());
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_details_pagination() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_pagination"))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_pagination"))
        .await
        .checked("test operation should succeed");

    for i in 0..5 {
        let room = room_repo
            .create(&make_room(&format!("Room Pag {i}"), &owner.id))
            .await
            .checked("test operation should succeed");
        let member = make_member(room.id, user.id, RoomRole::Member);
        member_repo
            .add(&member)
            .await
            .checked("test operation should succeed");
        // Small delay to ensure distinct joined_at timestamps for stable ordering
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    }

    // Page 1 with page_size=2
    let page1 = PageParams::new(Some(1), Some(2));
    let (rooms_p1, total_p1) = member_repo
        .list_by_user_with_details(&user.id, page1)
        .await
        .checked("test operation should succeed");
    assert_eq!(total_p1, 5);
    assert_eq!(rooms_p1.len(), 2);

    // Page 2 with page_size=2
    let page2 = PageParams::new(Some(2), Some(2));
    let (rooms_p2, total_p2) = member_repo
        .list_by_user_with_details(&user.id, page2)
        .await
        .checked("test operation should succeed");
    assert_eq!(total_p2, 5);
    assert_eq!(rooms_p2.len(), 2);

    // Page 3 with page_size=2 (only 1 remaining)
    let page3 = PageParams::new(Some(3), Some(2));
    let (rooms_p3, total_p3) = member_repo
        .list_by_user_with_details(&user.id, page3)
        .await
        .checked("test operation should succeed");
    assert_eq!(total_p3, 5);
    assert_eq!(rooms_p3.len(), 1);

    // Out-of-range pages still need the real total for pagination controls.
    let page4 = PageParams::new(Some(4), Some(2));
    let (rooms_p4, total_p4) = member_repo
        .list_by_user_with_details(&user.id, page4)
        .await
        .checked("test operation should succeed");
    assert_eq!(total_p4, 5);
    assert!(rooms_p4.is_empty());

    let (room_ids_p4, room_id_total_p4) = member_repo
        .list_by_user(&user.id, page4)
        .await
        .checked("test operation should succeed");
    assert_eq!(room_id_total_p4, 5);
    assert!(room_ids_p4.is_empty());

    // Verify no overlapping room IDs between pages
    let ids_p1: Vec<_> = rooms_p1.iter().map(|(r, _, _, _)| r.id).collect();
    let ids_p2: Vec<_> = rooms_p2.iter().map(|(r, _, _, _)| r.id).collect();
    let ids_p3: Vec<_> = rooms_p3.iter().map(|(r, _, _, _)| r.id).collect();

    for id in &ids_p1 {
        assert!(!ids_p2.contains(id));
        assert!(!ids_p3.contains(id));
    }
    for id in &ids_p2 {
        assert!(!ids_p3.contains(id));
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_query_respects_filters_sort_and_pagination() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_related_query"))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_related_query"))
        .await
        .checked("test operation should succeed");

    let alpha_room = room_repo
        .create(&make_room("Alpha Room", &owner.id))
        .await
        .checked("test operation should succeed");
    let beta_room = room_repo
        .create(&make_room("Beta Room", &owner.id))
        .await
        .checked("test operation should succeed");
    let closed_room = room_repo
        .create(&make_room("Gamma Room", &owner.id))
        .await
        .checked("test operation should succeed");
    let banned_room = room_repo
        .create(&make_room("Delta Room", &owner.id))
        .await
        .checked("test operation should succeed");

    room_repo
        .update_status(&closed_room.id, RoomStatus::Closed)
        .await
        .checked("test operation should succeed");
    room_repo
        .update_ban_status(&banned_room.id, true)
        .await
        .checked("test operation should succeed");

    for room in [&alpha_room, &beta_room, &closed_room, &banned_room] {
        member_repo
            .add(&make_member(room.id, user.id, RoomRole::Member))
            .await
            .checked("test operation should succeed");
    }

    let query = MyRoomListQuery {
        pagination: PageParams::new(Some(1), Some(1)),
        search: Some("room".to_string()),
        status: Some(RoomStatus::Active),
        is_banned: Some(false),
        relation: synctv_core::models::MyRoomRelation::All,
        sort_by: MyRoomListSortBy::Name,
        sort_direction: SortDirection::Asc,
    };

    let (page1, total_page1) = member_repo
        .list_by_user_with_query(&user.id, &query)
        .await
        .checked("test operation should succeed");
    assert_eq!(total_page1, 2);
    assert_eq!(page1.len(), 1);
    assert_eq!(page1[0].0.name, "Alpha Room");

    let (page2, total_page2) = member_repo
        .list_by_user_with_query(
            &user.id,
            &MyRoomListQuery {
                pagination: PageParams::new(Some(2), Some(1)),
                ..query
            },
        )
        .await
        .checked("test operation should succeed");
    assert_eq!(total_page2, 2);
    assert_eq!(page2.len(), 1);
    assert_eq!(page2[0].0.name, "Beta Room");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_list_by_user_with_query_member_count_counts_active_only_rows() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_member_count_filters"))
        .await
        .checked("test operation should succeed");
    let viewer = user_repo
        .create(&make_user("viewer_member_count_filters"))
        .await
        .checked("test operation should succeed");
    let removed = user_repo
        .create(&make_user("removed_member_count_filters"))
        .await
        .checked("test operation should succeed");

    let room = room_repo
        .create(&make_room("Count Filter Room", &owner.id))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, owner.id, RoomRole::Creator))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, viewer.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    member_repo
        .add(&make_member(room.id, removed.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");
    member_repo
        .remove(&room.id, &removed.id)
        .await
        .checked("test operation should succeed");

    let (rows, total) = member_repo
        .list_by_user_with_query(
            &viewer.id,
            &MyRoomListQuery {
                pagination: PageParams::new(Some(1), Some(10)),
                relation: synctv_core::models::MyRoomRelation::All,
                ..MyRoomListQuery::default()
            },
        )
        .await
        .checked("test operation should succeed");

    assert_eq!(total, 1);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].0.id, room.id);
    assert_eq!(rows[0].3, 2, "only owner + active viewer should count");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_member_rejects_active_kick_cooldown() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_kicked"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Kicked", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_kicked"))
        .await
        .checked("test operation should succeed");

    // Add the user first
    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    member_repo
        .remove(&room.id, &user.id)
        .await
        .checked("test operation should succeed");
    add_kick_cooldown(&member_repo, room.id, user.id, Some(owner.id)).await;

    // Try to re-add -> should get Authorization error while cooldown is active.
    let member = make_member(room.id, user.id, RoomRole::Member);
    let err = member_repo
        .add(&member)
        .await
        .failed("operation should fail");
    assert!(matches!(err, Error::KickCooldownDenied));
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_failed_add_in_caller_transaction_does_not_advance_lifecycle_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_failed_add_lifecycle"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Failed Add Lifecycle", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_failed_add_lifecycle"))
        .await
        .checked("test operation should succeed");

    let member = member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");
    let before = member_repo
        .lifecycle_version(&room.id, &user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(before, member.version);

    let mut tx = pool.begin().await.checked("test operation should succeed");
    let duplicate = member_repo
        .add_with_executor(&make_member(room.id, user.id, RoomRole::Member), &mut tx)
        .await;
    assert!(matches!(duplicate, Err(Error::AlreadyExists(_))));
    tx.commit().await.checked("test operation should succeed");

    let after = member_repo
        .lifecycle_version(&room.id, &user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(
        after, before,
        "failed add inside a caller-owned transaction must not burn a lifecycle version"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_with_executor_diagnoses_uncommitted_duplicate_in_same_transaction() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_tx_duplicate_diag"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Tx Duplicate Diagnostic", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_tx_duplicate_diag"))
        .await
        .checked("test operation should succeed");

    let mut tx = pool.begin().await.checked("test operation should succeed");
    member_repo
        .add_with_executor(&make_member(room.id, user.id, RoomRole::Member), &mut tx)
        .await
        .checked("test operation should succeed");

    let duplicate = member_repo
        .add_with_executor(&make_member(room.id, user.id, RoomRole::Member), &mut tx)
        .await;

    assert!(
        matches!(duplicate, Err(Error::AlreadyExists(_))),
        "diagnostic query must see membership inserted earlier in the same transaction: {duplicate:?}"
    );

    tx.rollback().await.checked("test operation should succeed");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_add_after_remove_creates_fresh_membership() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_removed"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Room Removed", &owner.id))
        .await
        .checked("test operation should succeed");
    let user = user_repo
        .create(&make_user("user_removed"))
        .await
        .checked("test operation should succeed");

    // Add the user
    member_repo
        .add(&make_member(room.id, user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");
    member_repo
        .remove(&room.id, &user.id)
        .await
        .checked("test operation should succeed");
    let tombstone_version = member_repo
        .lifecycle_version(&room.id, &user.id)
        .await
        .checked("test operation should succeed");

    let member = make_member(room.id, user.id, RoomRole::Member);
    let result = member_repo.add(&member).await;

    assert!(result.is_ok());
    let rejoined = result.checked("test operation should succeed");
    assert_eq!(rejoined.status, MemberStatus::Active);
    assert_eq!(rejoined.added_permissions, 0);
    assert_eq!(rejoined.removed_permissions, 0);
    assert_eq!(rejoined.admin_added_permissions, 0);
    assert_eq!(rejoined.admin_removed_permissions, 0);
    assert!(
        rejoined.version > tombstone_version,
        "rejoined member row version must be strictly newer than the removal fence"
    );
    let active_lifecycle_version = member_repo
        .lifecycle_version(&room.id, &user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(
        rejoined.version, active_lifecycle_version,
        "rejoined member row must publish the lifecycle version allocated in the insert transaction"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_permissions_after_member_removed_should_fail() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_removed_permissions_test"))
        .await
        .checked("test operation should succeed");
    let member_user = user_repo
        .create(&make_user("member_removed_test"))
        .await
        .checked("test operation should succeed");

    let room = room_repo
        .create(&make_room("Permissions Test Room", &owner.id))
        .await
        .checked("test operation should succeed");

    // Add member with no permissions
    let new_member = make_member(room.id, member_user.id, RoomRole::Member);
    let _member = member_repo
        .add_with_options(&new_member, &AddMemberOptions::new())
        .await
        .checked("test operation should succeed");

    member_repo
        .remove(&room.id, &member_user.id)
        .await
        .checked("test operation should succeed");

    // SECURITY CHECK: Try to update permissions for removed member.
    let result = member_repo
        .update_permissions(
            &room.id,
            &member_user.id,
            0b0000_0001, // Add permission bit 0
            0,           // Remove nothing
            0,
        )
        .await;

    assert!(
        result.is_err(),
        "update_permissions should fail for removed member"
    );

    match result {
        Err(Error::OptimisticLockConflict) => { /* Expected */ }
        Err(e) => std::panic::panic_any(format!("expected OptimisticLockConflict, got: {e:?}")),
        Ok(_) => std::panic::panic_any("update_permissions should not succeed for removed member"),
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_update_permissions_for_active_member_should_succeed() {
    // Verify that update_permissions still works for active members (the valid case)

    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_active_perm"))
        .await
        .checked("test operation should succeed");
    let member_user = user_repo
        .create(&make_user("member_active"))
        .await
        .checked("test operation should succeed");

    let room = room_repo
        .create(&make_room("Active Permissions Test", &owner.id))
        .await
        .checked("test operation should succeed");

    // Add active member
    let new_member = make_member(room.id, member_user.id, RoomRole::Member);
    let member = member_repo
        .add_with_options(&new_member, &AddMemberOptions::new())
        .await
        .checked("test operation should succeed");

    // Update permissions for active member - this should succeed
    let updated = member_repo
        .update_permissions(
            &room.id,
            &member_user.id,
            0b0000_0001, // Add permission bit 0
            0,           // Remove nothing
            member.version,
        )
        .await;

    assert!(
        updated.is_ok(),
        "update_permissions should succeed for active member"
    );

    let updated_member = updated.checked("test operation should succeed");
    assert_eq!(updated_member.added_permissions, 0b0000_0001);
    assert_eq!(updated_member.version, member.version + 1);
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_bulk_remove_for_user_returns_post_delete_lifecycle_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_bulk_user_lifecycle"))
        .await
        .checked("test operation should succeed");
    let member_user = user_repo
        .create(&make_user("member_bulk_user_lifecycle"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Bulk User Lifecycle Room", &owner.id))
        .await
        .checked("test operation should succeed");
    let member = member_repo
        .add(&make_member(room.id, member_user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    let mut tx = pool.begin().await.checked("test operation should succeed");
    let removed = member_repo
        .remove_all_for_user_with_executor(&member_user.id, &mut tx)
        .await
        .checked("test operation should succeed");
    tx.commit().await.checked("test operation should succeed");

    assert_eq!(removed.len(), 1);
    let lifecycle_version = member_repo
        .lifecycle_version(&room.id, &member_user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(removed[0].version, lifecycle_version);
    assert!(
        removed[0].version > member.version,
        "bulk user removal must return the post-delete tombstone version, not the stale member row version"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn test_bulk_remove_for_rooms_returns_post_delete_lifecycle_version() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());

    let owner = user_repo
        .create(&make_user("owner_bulk_room_lifecycle"))
        .await
        .checked("test operation should succeed");
    let member_user = user_repo
        .create(&make_user("member_bulk_room_lifecycle"))
        .await
        .checked("test operation should succeed");
    let room = room_repo
        .create(&make_room("Bulk Room Lifecycle Room", &owner.id))
        .await
        .checked("test operation should succeed");
    let member = member_repo
        .add(&make_member(room.id, member_user.id, RoomRole::Member))
        .await
        .checked("test operation should succeed");

    let mut tx = pool.begin().await.checked("test operation should succeed");
    let removed = member_repo
        .remove_all_for_rooms_with_executor(&[room.id], &mut tx)
        .await
        .checked("test operation should succeed");
    tx.commit().await.checked("test operation should succeed");

    assert_eq!(removed.len(), 1);
    let lifecycle_version = member_repo
        .lifecycle_version(&room.id, &member_user.id)
        .await
        .checked("test operation should succeed");
    assert_eq!(removed[0].version, lifecycle_version);
    assert!(
        removed[0].version > member.version,
        "bulk room removal must return the post-delete tombstone version, not the stale member row version"
    );
}
