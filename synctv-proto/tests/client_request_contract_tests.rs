use synctv_proto::client::{
    AddMediaRequest, DeleteEntriesRequest, DeleteMediaRequest, DeletePlaylistRequest,
    ListPlaylistItemsRequest, StartPlaybackRequest,
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
fn test_list_playlist_items_request_allows_room_root_with_empty_playlist_id() {
    let request = ListPlaylistItemsRequest {
        playlist_id: String::new(),
        target: Vec::new(),
        page: 1,
        page_size: 50,
    };

    let json = serde_json::to_value(&request).expect("serialize list request");
    assert_eq!(json["playlist_id"], "");
    let target_bytes: Vec<u8> =
        serde_json::from_value(json["target"].clone()).expect("target bytes");
    assert!(target_bytes.is_empty());
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
        target: br#"{"item_id":"provider-item-1"}"#.to_vec(),
    };

    let json = serde_json::to_value(&request).expect("serialize start playback request");
    assert_eq!(json["media_id"], "");
    assert_eq!(json["playlist_id"], "playlist-1");
    assert_eq!(
        json["target"],
        serde_json::json!({"item_id":"provider-item-1"})
    );
}

#[test]
fn test_create_playlist_request_serializes_dynamic_fields_without_is_folder() {
    let request = synctv_proto::client::CreatePlaylistRequest {
        name: "Dyn".to_string(),
        parent_id: "playlist-root".to_string(),
        source_provider: "alist".to_string(),
        source_config: br#"{"path":"/tv"}"#.to_vec(),
        provider_instance_name: "alist-main".to_string(),
    };

    let json = serde_json::to_value(&request).expect("serialize create playlist request");
    assert!(json.get("is_folder").is_none());
    assert_eq!(json["source_provider"], "alist");
    assert_eq!(json["provider_instance_name"], "alist-main");
    assert_eq!(json["source_config"], serde_json::json!({"path":"/tv"}));
}
