use super::*;

fn direct_url_media_source_config(url: impl Into<String>) -> crate::models::MediaSourceConfig {
    crate::models::MediaSourceConfig::DirectUrl(crate::models::DirectUrlMediaSourceConfig {
        is_live: None,
        duration_seconds: None,
        prefer_proxy: None,
        medias: vec![crate::models::DirectUrlMediaResourceConfig {
            name: String::new(),
            url: url.into(),
            headers: std::collections::HashMap::new(),
            format: String::new(),
        }],
        default_media_index: None,
        subtitles: Vec::new(),
        default_subtitle_index: None,
        danmakus: Vec::new(),
        default_danmaku_index: None,
    })
}

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
        source_provider: SourceProvider::DirectUrl,
        provider_instance_name: None,
        source_config: direct_url_media_source_config("https://example.com/video.mp4"),
        added_at: crate::SystemClock.now(),
        updated_at: crate::SystemClock.now(),
        version: 1,
        cover_file_reference_id: None,
        thumbnail_file_reference_id: None,
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
