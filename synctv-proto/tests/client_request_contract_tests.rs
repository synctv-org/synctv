use synctv_proto::client::{AddMediaRequest, ListPlaylistRequest};

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
