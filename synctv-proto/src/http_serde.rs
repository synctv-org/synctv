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

#[derive(serde::Deserialize)]
pub struct AlistLoginRequestDef {
    host: String,
    username: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    hashed_password: Option<String>,
    #[serde(default)]
    otp_code: String,
    #[serde(default)]
    otp_secret: String,
    #[serde(default)]
    instance_name: String,
}

impl TryFrom<AlistLoginRequestDef> for crate::providers::alist::LoginRequest {
    type Error = String;

    fn try_from(value: AlistLoginRequestDef) -> Result<Self, Self::Error> {
        let credential = Some(
            crate::providers::alist::login_request::Credential::try_from(AlistLoginRequestDef {
                host: value.host.clone(),
                username: value.username.clone(),
                password: value.password,
                hashed_password: value.hashed_password,
                otp_code: value.otp_code.clone(),
                otp_secret: value.otp_secret.clone(),
                instance_name: value.instance_name.clone(),
            })?,
        );

        Ok(Self {
            host: value.host,
            username: value.username,
            credential,
            otp_code: value.otp_code,
            otp_secret: value.otp_secret,
            instance_name: value.instance_name,
        })
    }
}

impl TryFrom<AlistLoginRequestDef> for crate::providers::alist::login_request::Credential {
    type Error = String;

    fn try_from(value: AlistLoginRequestDef) -> Result<Self, Self::Error> {
        match (value.password, value.hashed_password) {
            (Some(password), None) => Ok(Self::Password(password)),
            (None, Some(hashed_password)) => Ok(Self::HashedPassword(hashed_password)),
            (None, None) => {
                Err("exactly one of password or hashed_password must be provided".into())
            }
            (Some(_), Some(_)) => Err("password and hashed_password are mutually exclusive".into()),
        }
    }
}

#[derive(serde::Deserialize)]
pub struct EmbyLoginRequestDef {
    host: String,
    username: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    instance_name: String,
}

impl TryFrom<EmbyLoginRequestDef> for crate::providers::emby::LoginRequest {
    type Error = String;

    fn try_from(value: EmbyLoginRequestDef) -> Result<Self, Self::Error> {
        let credential = Some(crate::providers::emby::login_request::Credential::try_from(
            EmbyLoginRequestDef {
                host: value.host.clone(),
                username: value.username.clone(),
                password: value.password,
                api_key: value.api_key,
                instance_name: value.instance_name.clone(),
            },
        )?);

        Ok(Self {
            host: value.host,
            username: value.username,
            credential,
            instance_name: value.instance_name,
        })
    }
}

impl TryFrom<EmbyLoginRequestDef> for crate::providers::emby::login_request::Credential {
    type Error = String;

    fn try_from(value: EmbyLoginRequestDef) -> Result<Self, Self::Error> {
        match (value.password, value.api_key) {
            (Some(password), None) => Ok(Self::Password(password)),
            (None, Some(api_key)) => Ok(Self::ApiKey(api_key)),
            (None, None) => Err("exactly one of password or api_key must be provided".into()),
            (Some(_), Some(_)) => Err("password and api_key are mutually exclusive".into()),
        }
    }
}
