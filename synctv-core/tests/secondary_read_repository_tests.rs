use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    models::{
        room_settings::MaxMembers, AuditAction, AuditTargetType, ChatMessage, ChatMessageType,
        ContentReportStatus, ContentReportTarget, CreateContentReport, MyRoomListQuery, PageParams,
        ReviewStatus, Room, RoomId, RoomListQuery, RoomMember, RoomRole, RoomSettings, RoomStatus,
        SignupMethod, UpsertRoomCategory, UpsertRoomLabel, User, UserId, UserListQuery,
    },
    repository::{
        AuditLogQuery, AuditLogRepository, BanRecordListQuery, BanRecordRepository, ChatRepository,
        ContentReportListQuery, ContentReportListScope, ContentReportRepository, ReviewRepository,
        RoomMemberRepository, RoomPasswordRepository, RoomRepository, RoomSettingsRepository,
        RoomTaxonomyRepository, UserRegistrationReviewListQuery, UserRepository,
    },
};
use synctv_core_testing::{create_test_pool_with_db_and_label, ok, some};

fn user(username: &str) -> User {
    User::new(username.to_string(), SignupMethod::Email)
}

async fn create_user(pool: &PgPool, username: &str) -> User {
    ok(
        UserRepository::new(pool.clone())
            .create(&user(username))
            .await,
        "user should be created",
    )
}

fn room(name: &str, owner_id: UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: owner_id,
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

async fn create_room(pool: &PgPool, name: &str, owner_id: UserId) -> Room {
    ok(
        RoomRepository::new(pool.clone())
            .create(&room(name, owner_id))
            .await,
        "room should be created",
    )
}

async fn insert_pending_registration(pool: &PgPool, username: &str) -> UserId {
    let id: i64 = ok(
        sqlx::query_scalar!(
            r#"
            INSERT INTO user_registration_requests (
                username,
                email,
                opaque_record,
                opaque_credential_identifier,
                opaque_ciphersuite,
                opaque_server_setup_version,
                signup_method,
                status
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING id AS "id!"
            "#,
            username,
            &format!("{username}@example.com"),
            b"opaque-record".as_slice(),
            b"opaque-id".as_slice(),
            "ristretto255",
            1_i32,
            i16::from(SignupMethod::Email),
            i16::from(ReviewStatus::Pending)
        )
        .fetch_one(pool)
        .await,
        "registration review should be inserted",
    );
    ok(
        UserId::try_from(id),
        "registration review id should fit UserId",
    )
}

async fn insert_audit_log(pool: &PgPool, actor_username: &str) -> i64 {
    ok(
        sqlx::query_scalar!(
            r#"
            INSERT INTO audit_logs (
                actor_username,
                action,
                target_type,
                target_id,
                created_at
            )
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id AS "id!"
            "#,
            actor_username,
            i16::from(AuditAction::UserLogin),
            i16::from(AuditTargetType::User),
            actor_username,
            Utc::now()
        )
        .fetch_one(pool)
        .await,
        "audit log should be inserted",
    )
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn user_eventual_list_reads_from_read_pool_while_default_list_uses_primary() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-user-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-user-read").await;

    let primary_user = create_user(&primary_pool, "primary_only_user").await;
    let read_user = create_user(&read_pool, "read_pool_user").await;

    let repo = UserRepository::new_with_read_pool(primary_pool, read_pool);
    let (primary_users, primary_total) = ok(
        repo.list(&UserListQuery::default()).await,
        "primary user list should be loaded",
    );

    assert_eq!(primary_total, 1);
    assert_eq!(primary_users[0].username, "primary_only_user");
    let (eventual_users, eventual_total) = ok(
        repo.list_eventually_consistent(&UserListQuery::default())
            .await,
        "eventual user list should be loaded",
    );
    assert_eq!(eventual_total, 1);
    assert_eq!(eventual_users[0].username, "read_pool_user");
    let eventual_users_by_id = ok(
        repo.get_by_ids_eventually_consistent(&[read_user.id]).await,
        "eventual user batch should be loaded",
    );
    assert_eq!(eventual_users_by_id[0].username, "read_pool_user");
    let loaded = ok(
        repo.get_by_id(&primary_user.id).await,
        "primary user lookup should query primary",
    );
    assert_eq!(
        some(loaded, "primary user should be visible through get_by_id").username,
        "primary_only_user"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn homepage_room_reads_use_read_pool_while_primary_methods_stay_strongly_consistent() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "homepage-read-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "homepage-read-replica").await;

    let primary_owner = create_user(&primary_pool, "primary_home_owner").await;
    let read_owner = create_user(&read_pool, "read_home_owner").await;
    let primary_room = create_room(&primary_pool, "Primary Home Room", primary_owner.id).await;
    let read_room = create_room(&read_pool, "Read Replica Home Room", read_owner.id).await;
    assert_eq!(primary_owner.id, read_owner.id);
    assert_eq!(primary_room.id, read_room.id);

    ok(
        RoomMemberRepository::new(primary_pool.clone())
            .add(&RoomMember::new(
                primary_room.id,
                primary_owner.id,
                RoomRole::Creator,
            ))
            .await,
        "primary room member should be created",
    );
    ok(
        RoomMemberRepository::new(read_pool.clone())
            .add(&RoomMember::new(
                read_room.id,
                read_owner.id,
                RoomRole::Creator,
            ))
            .await,
        "read replica room member should be created",
    );
    ok(
        RoomRepository::new(read_pool.clone())
            .favorite_for_user(&read_owner.id, &read_room.id)
            .await,
        "read replica room should be favorited",
    );

    let primary_settings = RoomSettings {
        max_members: MaxMembers(11),
        ..RoomSettings::default()
    };
    let read_settings = RoomSettings {
        max_members: MaxMembers(37),
        ..RoomSettings::default()
    };
    ok(
        RoomSettingsRepository::new(primary_pool.clone())
            .set_settings(&primary_room.id, &primary_settings)
            .await,
        "primary room settings should be stored",
    );
    ok(
        RoomSettingsRepository::new(read_pool.clone())
            .set_settings(&read_room.id, &read_settings)
            .await,
        "read replica room settings should be stored",
    );

    ok(
        sqlx::query!(
            "INSERT INTO room_password_credentials (room_id, enabled, version) VALUES ($1, FALSE, 1)",
            primary_room.id as RoomId,
        )
        .execute(&primary_pool)
        .await,
        "primary room password state should be stored",
    );
    ok(
        sqlx::query!(
            "INSERT INTO room_password_credentials (
                room_id,
                opaque_record,
                opaque_credential_identifier,
                opaque_ciphersuite,
                opaque_server_setup_version,
                enabled,
                version
             ) VALUES ($1, $2, $3, $4, $5, TRUE, 1)",
            read_room.id as RoomId,
            b"read-home-opaque-record".as_slice(),
            b"read-home-credential-id".as_slice(),
            "ristretto255",
            1_i32,
        )
        .execute(&read_pool)
        .await,
        "read replica room password state should be stored",
    );

    let primary_taxonomy = RoomTaxonomyRepository::new(primary_pool.clone());
    let read_taxonomy = RoomTaxonomyRepository::new(read_pool.clone());
    ok(
        primary_taxonomy
            .upsert_category(&UpsertRoomCategory {
                key: "primary-home".to_string(),
                name: "Primary Home".to_string(),
                description: String::new(),
                sort_order: 0,
                is_enabled: true,
            })
            .await,
        "primary category should be stored",
    );
    let read_category = ok(
        read_taxonomy
            .upsert_category(&UpsertRoomCategory {
                key: "read-home".to_string(),
                name: "Read Home".to_string(),
                description: String::new(),
                sort_order: 0,
                is_enabled: true,
            })
            .await,
        "read replica category should be stored",
    );
    let read_label = ok(
        read_taxonomy
            .upsert_label(&UpsertRoomLabel {
                key: "read-featured".to_string(),
                name: "Read Featured".to_string(),
                description: String::new(),
                color: "#336699".to_string(),
                category_id: Some(read_category.id),
                sort_order: 0,
                is_enabled: true,
            })
            .await,
        "read replica label should be stored",
    );
    let mut read_connection = ok(
        read_pool.acquire().await,
        "read replica connection should be acquired",
    );
    ok(
        RoomTaxonomyRepository::assign_room_labels(
            read_room.id,
            &[read_label.id],
            Some(read_owner.id),
            &mut read_connection,
        )
        .await,
        "read replica room label should be assigned",
    );

    let room_repo = RoomRepository::new_with_read_pool(primary_pool.clone(), read_pool.clone());
    let query = RoomListQuery::default();
    let (primary_rooms, primary_total) = ok(
        room_repo.list(&query).await,
        "primary room list should load",
    );
    assert_eq!(primary_total, 1);
    assert_eq!(primary_rooms[0].name, "Primary Home Room");
    let (discovery_rooms, discovery_total) = ok(
        room_repo
            .list_window_excluding_eventually_consistent(&query, &[], 0, 10)
            .await,
        "discovery room window should load",
    );
    assert_eq!(discovery_total, 1);
    assert_eq!(discovery_rooms[0].name, "Read Replica Home Room");
    let (ranked_rooms, ranked_total) = ok(
        room_repo
            .list_ranked_window_eventually_consistent(&query, &[read_room.id], 0, 10)
            .await,
        "ranked discovery room window should load",
    );
    assert_eq!(ranked_total, 1);
    assert_eq!(ranked_rooms[0].name, "Read Replica Home Room");
    let active_rooms = ok(
        room_repo
            .list_active_unbanned_by_ids_eventually_consistent(&[read_room.id])
            .await,
        "discovery room lookup should load",
    );
    assert_eq!(active_rooms[0].name, "Read Replica Home Room");
    let primary_active_rooms = ok(
        room_repo
            .list_active_unbanned_by_ids(&[primary_room.id])
            .await,
        "primary discovery room lookup should load",
    );
    assert_eq!(primary_active_rooms[0].name, "Primary Home Room");
    let viewer_states = ok(
        room_repo
            .discovery_viewer_states_eventually_consistent(&read_owner.id, &[read_room.id])
            .await,
        "discovery viewer state should load",
    );
    let viewer_state = viewer_states
        .get(&read_room.id)
        .expect("read replica viewer state should exist");
    assert!(viewer_state.joined);
    assert!(viewer_state.favorited);
    let primary_viewer_states = ok(
        room_repo
            .discovery_viewer_states(&primary_owner.id, &[primary_room.id])
            .await,
        "primary discovery viewer state should load",
    );
    let primary_viewer_state = primary_viewer_states
        .get(&primary_room.id)
        .expect("primary viewer state should exist");
    assert!(primary_viewer_state.joined);
    assert!(!primary_viewer_state.favorited);
    let favorite_ids = ok(
        room_repo
            .favorite_room_ids_for_user_eventually_consistent(&read_owner.id, &[read_room.id])
            .await,
        "read replica favorites should load",
    );
    assert!(favorite_ids.contains(&read_room.id));

    let member_repo =
        RoomMemberRepository::new_with_read_pool(primary_pool.clone(), read_pool.clone());
    let member_counts = ok(
        member_repo
            .count_by_rooms_batch_eventually_consistent(&[&read_room.id])
            .await,
        "read replica member counts should load",
    );
    assert_eq!(member_counts.get(&read_room.id), Some(&1));
    let (joined_rooms, joined_total) = ok(
        member_repo
            .list_accessible_by_user_with_query_eventually_consistent(
                &read_owner.id,
                &MyRoomListQuery::default(),
            )
            .await,
        "read replica joined rooms should load",
    );
    assert_eq!(joined_total, 1);
    assert_eq!(joined_rooms[0].0.name, "Read Replica Home Room");

    let settings_repo =
        RoomSettingsRepository::new_with_read_pool(primary_pool.clone(), read_pool.clone());
    let loaded_primary_settings = ok(
        settings_repo.get_batch(&[primary_room.id]).await,
        "primary room settings should load",
    );
    assert_eq!(loaded_primary_settings[&primary_room.id].max_members.0, 11);
    let loaded_read_settings = ok(
        settings_repo
            .get_batch_eventually_consistent(&[read_room.id])
            .await,
        "read replica room settings should load",
    );
    assert_eq!(loaded_read_settings[&read_room.id].max_members.0, 37);

    let password_repo =
        RoomPasswordRepository::new_with_read_pool(primary_pool.clone(), read_pool.clone());
    let primary_password_ids = ok(
        password_repo.enabled_room_ids(&[primary_room.id]).await,
        "primary room password states should load",
    );
    assert!(primary_password_ids.is_empty());
    let read_password_ids = ok(
        password_repo
            .enabled_room_ids_eventually_consistent(&[read_room.id])
            .await,
        "read replica room password states should load",
    );
    assert!(read_password_ids.contains(&read_room.id));

    let taxonomy_repo = RoomTaxonomyRepository::new_with_read_pool(primary_pool, read_pool);
    let categories = ok(
        taxonomy_repo
            .list_categories_eventually_consistent(true)
            .await,
        "read replica categories should load",
    );
    assert!(categories
        .iter()
        .any(|category| category.key == "read-home"));
    let labels = ok(
        taxonomy_repo
            .list_labels_eventually_consistent(true, Some(read_category.id))
            .await,
        "read replica labels should load",
    );
    assert!(labels.iter().any(|label| label.key == "read-featured"));
    let labels_by_room = ok(
        taxonomy_repo
            .labels_for_rooms_eventually_consistent(&[read_room.id])
            .await,
        "read replica room labels should load",
    );
    assert_eq!(labels_by_room[&read_room.id][0].key, "read-featured");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn chat_history_page_uses_primary_snapshot_with_primary_event_cursor() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-chat-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-chat-read").await;

    let creator = create_user(&primary_pool, "primary_chat_owner").await;
    let room = create_room(&primary_pool, "Primary Chat History Room", creator.id).await;
    let mut message = ChatMessage::new(room.id, creator.id, "primary message".to_string());
    message.message_type = ChatMessageType::User;
    message.client_message_id = Some("primary-chat-message".to_string());

    let primary_repo = ChatRepository::new_with_read_pool(primary_pool, read_pool);
    let inserted = ok(
        primary_repo
            .insert_message_event_idempotent(
                &message,
                &[],
                &[],
                "primary-chat-message-hash",
                "primary-chat-event",
                Utc::now(),
            )
            .await,
        "primary chat message event should be inserted",
    );

    let page = ok(
        primary_repo
            .list_history_page_for_viewer(&room.id, None, 10, false, Some(&creator.id))
            .await,
        "history page should load",
    );

    assert_eq!(
        page.event_cursor.sequence, inserted.event.sequence,
        "event cursor should come from the primary event stream"
    );
    assert_eq!(
        page.messages
            .iter()
            .map(|message| message.message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["primary message"],
        "history page snapshot should come from the same primary source as the cursor"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn review_lists_read_from_read_pool_while_load_uses_primary() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-review-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-review-read").await;

    let primary_request_id = insert_pending_registration(&primary_pool, "primary_review").await;
    insert_pending_registration(&read_pool, "read_review").await;

    let repo = ReviewRepository::new_with_read_pool(primary_pool, read_pool);
    let page = ok(
        repo.list_user_registrations(&UserRegistrationReviewListQuery {
            status: ReviewStatus::Pending,
            search: None,
            limit: 10,
            offset: 0,
        })
        .await,
        "review list should load",
    );

    assert_eq!(page.total, 1);
    assert_eq!(page.rows[0].username, "read_review");
    let loaded = ok(
        repo.load_user_registration(primary_request_id).await,
        "registration review detail should query primary",
    );
    assert_eq!(
        some(
            loaded,
            "primary review should be visible through detail load"
        )
        .username,
        "primary_review"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn ban_record_list_reads_from_read_pool() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-ban-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-ban-read").await;

    create_user(&primary_pool, "primary_ban_user").await;
    let read_admin = create_user(&read_pool, "read_ban_admin").await;
    let read_target = create_user(&read_pool, "read_ban_target").await;
    ok(
        sqlx::query!(
            r#"
            INSERT INTO user_bans (user_id, banned_by, reason, starts_at)
            VALUES ($1, $2, $3, $4)
            "#,
            read_target.id.as_i64(),
            read_admin.id.as_i64(),
            "policy",
            Utc::now()
        )
        .execute(&read_pool)
        .await,
        "read-pool ban should be inserted",
    );

    let repo = BanRecordRepository::new_with_read_pool(primary_pool, read_pool);
    let page = ok(
        repo.list(&BanRecordListQuery {
            target_type: None,
            active: Some(true),
            user_id: None,
            room_id: None,
            limit: 10,
            offset: 0,
        })
        .await,
        "ban list should load",
    );

    assert_eq!(page.total, 1);
    assert_eq!(page.rows[0].username, "read_ban_target");
    assert_eq!(page.rows[0].banned_by_username, "read_ban_admin");
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn content_report_list_reads_from_read_pool_while_detail_uses_primary() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-report-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-report-read").await;

    let primary_reporter = create_user(&primary_pool, "primary_reporter").await;
    let primary_target = create_user(&primary_pool, "primary_target").await;
    let read_reporter = create_user(&read_pool, "read_reporter").await;
    let read_target = create_user(&read_pool, "read_target").await;

    let primary_repo = ContentReportRepository::new(primary_pool.clone());
    let primary_report = ok(
        primary_repo
            .create(
                CreateContentReport {
                    reporter_user_id: primary_reporter.id,
                    target: ContentReportTarget::User {
                        user_id: primary_target.id,
                    },
                    reason_code: "spam".to_string(),
                    reason: "primary report".to_string(),
                    metadata: None,
                },
                None,
            )
            .await,
        "primary report should be created",
    );
    ok(
        ContentReportRepository::new(read_pool.clone())
            .create(
                CreateContentReport {
                    reporter_user_id: read_reporter.id,
                    target: ContentReportTarget::User {
                        user_id: read_target.id,
                    },
                    reason_code: "spam".to_string(),
                    reason: "read report".to_string(),
                    metadata: None,
                },
                None,
            )
            .await,
        "read-pool report should be created",
    );

    let repo = ContentReportRepository::new_with_read_pool(primary_pool, read_pool);
    let page = ok(
        repo.list_admin(&ContentReportListQuery {
            status: Some(ContentReportStatus::Open),
            target_type: None,
            reporter_user_id: None,
            room_id: None,
            target_room_id: None,
            target_user_id: None,
            target_member_room_id: None,
            target_member_user_id: None,
            target_chat_message_id: None,
            scope: ContentReportListScope::AnyRelated,
            search: String::new(),
            limit: 10,
            offset: 0,
        })
        .await,
        "content report list should load",
    );

    assert_eq!(page.total, 1);
    assert_eq!(page.rows[0].reporter_username, "read_reporter");
    let loaded = ok(
        repo.get_admin(primary_report.id).await,
        "content report detail should query primary",
    );
    assert_eq!(
        some(
            loaded,
            "primary report should be visible through detail load"
        )
        .reason,
        "primary report"
    );
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn audit_log_list_reads_from_read_pool_while_detail_uses_primary() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-audit-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-audit-read").await;

    let primary_audit_id = insert_audit_log(&primary_pool, "primary_audit_actor").await;
    insert_audit_log(&read_pool, "read_audit_actor").await;

    let repo = AuditLogRepository::new_with_read_pool(primary_pool, read_pool);
    let (rows, total) = ok(
        repo.list(&AuditLogQuery {
            action: Some(AuditAction::UserLogin),
            from: Some(Utc::now() - chrono::Duration::hours(1)),
            page: PageParams::new(Some(1), Some(10)),
            ..Default::default()
        })
        .await,
        "audit list should load",
    );

    assert_eq!(total, 1);
    assert_eq!(rows[0].actor_username.as_deref(), Some("read_audit_actor"));
    let loaded = ok(
        repo.get_by_id(primary_audit_id).await,
        "audit detail should query primary",
    );
    assert_eq!(
        some(
            loaded,
            "primary audit should be visible through detail load"
        )
        .actor_username
        .as_deref(),
        Some("primary_audit_actor")
    );
}
