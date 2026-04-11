pub mod json_bytes {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match serde_json::from_slice::<serde_json::Value>(bytes) {
            Ok(value) => value.serialize(serializer),
            Err(_) => bytes.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        if matches!(value, serde_json::Value::Array(ref entries) if entries.is_empty()) {
            return Ok(Vec::new());
        }

        serde_json::to_vec(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
pub enum ClientUpdateRoomSettingsRequestDef {
    Wrapped { settings: serde_json::Value },
    Raw(serde_json::Value),
}

impl From<ClientUpdateRoomSettingsRequestDef> for crate::client::UpdateRoomSettingsRequest {
    fn from(value: ClientUpdateRoomSettingsRequestDef) -> Self {
        let settings = match value {
            ClientUpdateRoomSettingsRequestDef::Wrapped { settings }
            | ClientUpdateRoomSettingsRequestDef::Raw(settings) => settings,
        };

        Self {
            settings: serde_json::to_vec(&settings).expect("serializing JSON value cannot fail"),
        }
    }
}

#[derive(serde::Deserialize)]
#[serde(untagged)]
pub enum AdminUpdateRoomSettingsRequestDef {
    Wrapped {
        #[serde(default)]
        room_id: String,
        settings: serde_json::Value,
    },
    Raw(serde_json::Value),
}

impl From<AdminUpdateRoomSettingsRequestDef> for crate::admin::UpdateRoomSettingsRequest {
    fn from(value: AdminUpdateRoomSettingsRequestDef) -> Self {
        match value {
            AdminUpdateRoomSettingsRequestDef::Wrapped { room_id, settings } => Self {
                room_id,
                settings: serde_json::to_vec(&settings)
                    .expect("serializing JSON value cannot fail"),
            },
            AdminUpdateRoomSettingsRequestDef::Raw(settings) => Self {
                room_id: String::new(),
                settings: serde_json::to_vec(&settings)
                    .expect("serializing JSON value cannot fail"),
            },
        }
    }
}

#[derive(serde::Deserialize)]
pub struct MovePlaylistRequestDef {
    #[serde(
        default,
        rename = "playlist_id",
        alias = "playlistId",
        alias = "playlist"
    )]
    playlist_id: String,
    #[serde(
        default,
        rename = "before_playlist_id",
        alias = "beforePlaylistId",
        alias = "before"
    )]
    before: Option<String>,
    #[serde(
        default,
        rename = "after_playlist_id",
        alias = "afterPlaylistId",
        alias = "after"
    )]
    after: Option<String>,
}

impl TryFrom<MovePlaylistRequestDef> for crate::client::MovePlaylistRequest {
    type Error = String;

    fn try_from(value: MovePlaylistRequestDef) -> Result<Self, Self::Error> {
        let anchor = match (value.before, value.after) {
            (Some(before_playlist_id), None) => Some(
                crate::client::move_playlist_request::Anchor::BeforePlaylistId(before_playlist_id),
            ),
            (None, Some(after_playlist_id)) => Some(
                crate::client::move_playlist_request::Anchor::AfterPlaylistId(after_playlist_id),
            ),
            (None, None) => None,
            (Some(_), Some(_)) => {
                return Err(
                    "before_playlist_id and after_playlist_id are mutually exclusive".to_string(),
                );
            }
        };

        Ok(Self {
            playlist_id: value.playlist_id,
            anchor,
        })
    }
}
