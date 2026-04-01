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
            ClientUpdateRoomSettingsRequestDef::Wrapped { settings } => settings,
            ClientUpdateRoomSettingsRequestDef::Raw(settings) => settings,
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
