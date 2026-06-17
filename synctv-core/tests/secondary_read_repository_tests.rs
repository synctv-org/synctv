use chrono::Utc;
use sqlx::PgPool;
use synctv_core::{
    models::{
        AuditAction, AuditTargetType, ChatMessage, ChatMessageType, ContentReportStatus,
        ContentReportTarget, CreateContentReport, PageParams, ReviewStatus, Room, RoomId,
        RoomStatus, SignupMethod, User, UserId, UserListQuery,
    },
    repository::{
        AuditLogQuery, AuditLogRepository, BanRecordListQuery, BanRecordRepository, ChatRepository,
        ContentReportListQuery, ContentReportListScope, ContentReportRepository, ReviewRepository,
        RoomRepository, UserRegistrationReviewListQuery, UserRepository,
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
    create_user(&read_pool, "read_pool_user").await;

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
async fn chat_history_page_uses_primary_snapshot_with_primary_event_cursor() {
    let (_primary_container, primary_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-chat-primary").await;
    let (_read_container, read_pool) =
        create_test_pool_with_db_and_label("synctv_test", "secondary-read-chat-read").await;

    let creator = create_user(&primary_pool, "primary_chat_owner").await;
    let room = create_room(&primary_pool, "Primary Chat History Room", creator.id).await;
    let mut message = ChatMessage::new(room.id, creator.id, "primary message".to_string());
    message.message_type = ChatMessageType::Text;
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
                    metadata: serde_json::json!({}),
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
                    metadata: serde_json::json!({}),
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
