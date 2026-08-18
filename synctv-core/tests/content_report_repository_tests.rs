use std::collections::HashSet;

use chrono::Utc;
use synctv_core::{
    models::{
        ChatMessage, ContentReportStatus, ContentReportTarget, ContentReportTargetType,
        CreateContentReport, Room, RoomId, RoomMember, RoomRole, RoomStatus, User, UserId,
        UserRole, UserStatus,
    },
    repository::{
        ChatRepository, ContentReportListQuery, ContentReportListScope, ContentReportRepository,
        RoomMemberRepository, RoomRepository, UserRepository,
    },
};
use synctv_core_testing::{create_test_pool, ok};

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

fn make_room(name: &str, owner: UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        cover_file_reference_id: None,
        category: None,
        labels: Vec::new(),
        created_by: owner,
        status: RoomStatus::Active,
        is_banned: false,
        is_public: true,
        closed_at: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

fn report_query(room_id: RoomId) -> ContentReportListQuery {
    ContentReportListQuery {
        status: None,
        target_type: None,
        reporter_user_id: None,
        room_id: Some(room_id),
        target_room_id: None,
        target_user_id: None,
        target_member_room_id: None,
        target_member_user_id: None,
        target_chat_message_id: None,
        scope: ContentReportListScope::RoomContext,
        search: String::new(),
        limit: 100,
        offset: 0,
    }
}

#[tokio::test]
#[ignore = "Requires Docker"]
async fn room_context_report_list_includes_room_member_and_chat_message_reports() {
    let (_container, pool) = create_test_pool().await;
    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool.clone());
    let chat_repo = ChatRepository::new(pool.clone());
    let report_repo = ContentReportRepository::new(pool.clone());

    let owner = ok(
        user_repo.create(&make_user("report_owner")).await,
        "owner should be created",
    );
    let reporter = ok(
        user_repo.create(&make_user("reporter")).await,
        "reporter should be created",
    );
    let reported_member = ok(
        user_repo.create(&make_user("reported_member")).await,
        "reported member should be created",
    );
    let room = ok(
        room_repo
            .create(&make_room("reported room", owner.id))
            .await,
        "room should be created",
    );
    ok(
        member_repo
            .add(&RoomMember::new(
                room.id,
                reported_member.id,
                RoomRole::Member,
            ))
            .await,
        "reported room member should be created",
    );
    let chat_message = ok(
        chat_repo
            .create(&ChatMessage::new(
                room.id,
                reported_member.id,
                "reported chat message".to_string(),
            ))
            .await,
        "chat message should be created",
    );

    let room_report = ok(
        report_repo
            .create(
                CreateContentReport {
                    reporter_user_id: reporter.id,
                    target: ContentReportTarget::Room { room_id: room.id },
                    reason_code: "room".to_string(),
                    reason: String::new(),
                    metadata: None,
                },
                None,
            )
            .await,
        "room report should be created",
    );
    let member_report = ok(
        report_repo
            .create(
                CreateContentReport {
                    reporter_user_id: reporter.id,
                    target: ContentReportTarget::RoomMember {
                        room_id: room.id,
                        user_id: reported_member.id,
                    },
                    reason_code: "member".to_string(),
                    reason: String::new(),
                    metadata: None,
                },
                None,
            )
            .await,
        "member report should be created",
    );
    let chat_report = ok(
        report_repo
            .create(
                CreateContentReport {
                    reporter_user_id: reporter.id,
                    target: ContentReportTarget::ChatMessage {
                        room_id: room.id,
                        message_id: chat_message.id,
                    },
                    reason_code: "chat".to_string(),
                    reason: String::new(),
                    metadata: None,
                },
                Some(chat_message.created_at),
            )
            .await,
        "chat message report should be created",
    );

    let page = ok(
        report_repo.list_admin(&report_query(room.id)).await,
        "room-context report list should load",
    );
    let report_ids = page.rows.iter().map(|row| row.id).collect::<HashSet<_>>();
    assert_eq!(page.total, 3);
    assert!(report_ids.contains(&room_report.id));
    assert!(report_ids.contains(&member_report.id));
    assert!(report_ids.contains(&chat_report.id));

    let mut member_query = report_query(room.id);
    member_query.target_member_user_id = Some(reported_member.id);
    let member_page = ok(
        report_repo.list_admin(&member_query).await,
        "member-filtered report list should load",
    );
    assert_eq!(member_page.total, 1);
    assert_eq!(member_page.rows[0].id, member_report.id);
    assert_eq!(
        member_page.rows[0].target_type,
        ContentReportTargetType::RoomMember
    );
    assert_eq!(member_page.rows[0].status, ContentReportStatus::Open);
}
