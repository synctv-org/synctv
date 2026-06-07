use super::*;

#[test]
fn media_edit_requires_matching_creator() {
    let creator_id = UserId::expect_positive(10);
    let media = Media {
        id: MediaId::expect_positive(11),
        playlist_id: None,
        room_id: RoomId::expect_positive(12),
        creator_id: Some(creator_id),
        name: "Owned".to_string(),
        description: String::new(),
        position: 1.0,
        source_provider: "direct_url".to_string(),
        provider_instance_name: None,
        source_config: serde_json::json!({}),
        added_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        version: 1,
        cover_file_reference_id: None,
    };

    assert!(ensure_media_creator_can_edit(&media, &creator_id).is_ok());

    let other_user_id = UserId::expect_positive(13);
    assert!(matches!(
        ensure_media_creator_can_edit(&media, &other_user_id),
        Err(Error::Authorization(_))
    ));

    let mut unowned_media = media;
    unowned_media.creator_id = None;
    assert!(matches!(
        ensure_media_creator_can_edit(&unowned_media, &creator_id),
        Err(Error::Authorization(_))
    ));
}
