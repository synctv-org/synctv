use synctv_proto::client::{
    AddMediaRequest, DeleteEntriesRequest, DeleteMediaRequest, DeletePlaylistRequest,
    ListPlaylistRequest, StartPlaybackRequest,
};

#[test]
fn test_add_media_request_allows_room_root_without_playlist_id() {
    let request = AddMediaRequest {
        playlist_id: None,
        provider: "direct_url".to_string(),
        provider_instance_name: String::new(),
        source_config: br#"{"url":"https://example.com/video.mp4"}"#.to_vec(),
        title: "Example".to_string(),
    };

    assert!(request.playlist_id.is_none());
}

#[test]
fn test_list_playlist_request_is_room_scoped_without_playlist_id_field() {
    let request = ListPlaylistRequest {
        page: 1,
        page_size: 50,
    };

    let json = serde_json::to_value(&request).expect("serialize list request");
    assert!(
        json.get("playlist_id").is_none(),
        "room-root list request must not encode a fake playlist_id"
    );
}

#[test]
fn test_delete_media_request_serializes_force_flag() {
    let request = DeleteMediaRequest {
        media_id: "media-1".to_string(),
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete media request");
    assert_eq!(json["media_id"], "media-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_delete_playlist_request_serializes_force_flag() {
    let request = DeletePlaylistRequest {
        playlist_id: "playlist-1".to_string(),
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete playlist request");
    assert_eq!(json["playlist_id"], "playlist-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_delete_entries_request_serializes_force_flag() {
    let request = DeleteEntriesRequest {
        playlist_ids: vec!["playlist-1".to_string()],
        media_ids: vec!["media-1".to_string()],
        force: true,
    };

    let json = serde_json::to_value(&request).expect("serialize delete entries request");
    assert_eq!(json["playlist_ids"][0], "playlist-1");
    assert_eq!(json["media_ids"][0], "media-1");
    assert_eq!(json["force"], true);
}

#[test]
fn test_start_playback_request_serializes_dynamic_playlist_target() {
    let request = StartPlaybackRequest {
        media_id: String::new(),
        playlist_id: "playlist-1".to_string(),
        relative_path: "/episode-1.mkv".to_string(),
    };

    let json = serde_json::to_value(&request).expect("serialize start playback request");
    assert_eq!(json["media_id"], "");
    assert_eq!(json["playlist_id"], "playlist-1");
    assert_eq!(json["relative_path"], "/episode-1.mkv");
}
