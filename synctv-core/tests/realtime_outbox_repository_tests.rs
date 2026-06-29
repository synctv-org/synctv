use chrono::Utc;
use synctv_core::models::{MediaId, RealtimeEvent, RoomId, UserId};
use synctv_core::repository::realtime_outbox::{
    NewRealtimeOutboxEvent, RealtimeOutboxRepository, RealtimeOutboxStatus,
};
use synctv_core_testing::{create_test_pool, TestResultExt};

fn media_added_outbox_event(
    id: &str,
    enqueue_outbox: bool,
    room_id: RoomId,
    user_id: UserId,
    media_id: MediaId,
) -> NewRealtimeOutboxEvent {
    NewRealtimeOutboxEvent {
        id: id.to_string(),
        enqueue_outbox,
        aggregate_type: "media".to_string(),
        aggregate_id: media_id.to_string(),
        event_type: "media_added".to_string(),
        event_version: 1,
        aggregate_version: None,
        payload: RealtimeEvent::MediaAdded {
            event_id: id.to_string(),
            room_id,
            user_id,
            username: "batch-user".to_string(),
            media_id,
            media_title: format!("Media {media_id}"),
            timestamp: Utc::now(),
        },
    }
}

#[tokio::test]
async fn test_insert_many_writes_resource_events_and_only_enqueued_outbox_rows() {
    let (_container, pool) = create_test_pool().await;
    let repo = RealtimeOutboxRepository::new(pool.clone());
    let room_id = RoomId::expect_positive(10_000_301);
    let user_id = UserId::expect_positive(10_000_302);

    let events = vec![
        media_added_outbox_event(
            "evt-batch-enqueued",
            true,
            room_id,
            user_id,
            MediaId::expect_positive(10_000_303),
        ),
        media_added_outbox_event(
            "evt-batch-durable-only",
            false,
            room_id,
            user_id,
            MediaId::expect_positive(10_000_304),
        ),
    ];

    repo.insert_many(&events)
        .await
        .checked("batch outbox insert should succeed");

    let outbox_rows = sqlx::query!(
        r#"
        SELECT id, status
        FROM realtime_outbox
        ORDER BY id
        "#
    )
    .fetch_all(&pool)
    .await
    .checked("outbox rows should be queryable");
    assert_eq!(outbox_rows.len(), 1);
    assert_eq!(outbox_rows[0].id, "evt-batch-enqueued");
    assert_eq!(
        outbox_rows[0].status,
        RealtimeOutboxStatus::Pending.as_i16()
    );

    let resource_event_ids = sqlx::query_scalar!(
        r#"
        SELECT event_id
        FROM room_resource_events
        ORDER BY event_id
        "#
    )
    .fetch_all(&pool)
    .await
    .checked("room resource events should be queryable");
    assert_eq!(
        resource_event_ids,
        vec![
            "evt-batch-durable-only".to_string(),
            "evt-batch-enqueued".to_string(),
        ]
    );
}
