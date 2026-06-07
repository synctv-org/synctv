#[derive(Debug, serde::Deserialize)]
pub struct ChatMessagePath {
    pub room_id: String,
    pub message_id: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChatReactionPath {
    pub room_id: String,
    pub message_id: String,
    pub reaction_key: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct ChatImageObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatImageObjectQuery {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct VideoCoverObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VideoCoverObjectQuery {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct RoomCoverObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomCoverObjectQuery {
    pub token: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct PlaylistCoverObjectPath {
    pub encoded_object_key: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlaylistCoverObjectQuery {
    pub token: String,
}

#[derive(Debug, Default, serde::Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct KickRoomStreamBody {
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct RoomStreamPath {
    pub room_id: String,
    pub media_id: String,
}
