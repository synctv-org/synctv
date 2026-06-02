pub mod base64_bytes {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        STANDARD.decode(encoded).map_err(D::Error::custom)
    }
}

pub mod json_bytes {
    use serde::{ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        if bytes.is_empty() {
            return serializer.serialize_none();
        }

        let value = serde_json::from_slice::<serde_json::Value>(bytes).map_err(|error| {
            S::Error::custom(format!("JSON bytes field contains invalid JSON: {error}"))
        })?;
        value.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;

        if value.is_null() {
            return Ok(Vec::new());
        }

        if matches!(value, serde_json::Value::Array(ref entries) if entries.is_empty()) {
            return Ok(Vec::new());
        }

        serde_json::to_vec(&value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum Int64JsonValue {
    String(String),
    Number(i64),
}

#[derive(Debug, serde::Deserialize)]
#[serde(untagged)]
enum Uint64JsonValue {
    String(String),
    Number(u64),
}

fn parse_i64_value<E>(value: Int64JsonValue) -> Result<i64, E>
where
    E: serde::de::Error,
{
    match value {
        Int64JsonValue::String(value) => value.parse::<i64>().map_err(E::custom),
        Int64JsonValue::Number(value) => Ok(value),
    }
}

fn parse_u64_value<E>(value: Uint64JsonValue) -> Result<u64, E>
where
    E: serde::de::Error,
{
    match value {
        Uint64JsonValue::String(value) => value.parse::<u64>().map_err(E::custom),
        Uint64JsonValue::Number(value) => Ok(value),
    }
}

pub mod int64_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &i64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<i64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = super::Int64JsonValue::deserialize(deserializer)?;
        super::parse_i64_value(value)
    }
}

pub mod uint64_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &u64, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&value.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<u64, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = super::Uint64JsonValue::deserialize(deserializer)?;
        super::parse_u64_value(value)
    }
}

pub mod int64_string_option {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<i64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Some(value) = Option::<super::Int64JsonValue>::deserialize(deserializer)? else {
            return Ok(None);
        };
        super::parse_i64_value(value).map(Some)
    }
}

pub mod uint64_string_option {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Some(value) => serializer.serialize_some(&value.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let Some(value) = Option::<super::Uint64JsonValue>::deserialize(deserializer)? else {
            return Ok(None);
        };
        super::parse_u64_value(value).map(Some)
    }
}

pub mod int64_string_vec {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(values: &[i64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let values = values.iter().map(i64::to_string).collect::<Vec<_>>();
        values.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<i64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<super::Int64JsonValue>::deserialize(deserializer)?
            .into_iter()
            .map(super::parse_i64_value)
            .collect()
    }
}

pub mod uint64_string_vec {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(values: &[u64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let values = values.iter().map(u64::to_string).collect::<Vec<_>>();
        values.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u64>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<super::Uint64JsonValue>::deserialize(deserializer)?
            .into_iter()
            .map(super::parse_u64_value)
            .collect()
    }
}

fn login_identifier<E, T>(
    username: Option<String>,
    email: Option<String>,
    username_variant: impl FnOnce(String) -> T,
    email_variant: impl FnOnce(String) -> T,
) -> Result<Option<T>, E>
where
    E: serde::de::Error,
{
    match (username, email) {
        (Some(username), None) => Ok(Some(username_variant(username))),
        (None, Some(email)) => Ok(Some(email_variant(email))),
        (None, None) => Ok(None),
        (Some(_), Some(_)) => Err(E::custom("username and email are mutually exclusive")),
    }
}

#[derive(serde::Deserialize)]
pub struct LoginRequestDef {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    password: String,
}

impl TryFrom<LoginRequestDef> for crate::client::LoginRequest {
    type Error = serde_json::Error;

    fn try_from(value: LoginRequestDef) -> Result<Self, Self::Error> {
        let identifier = login_identifier::<serde_json::Error, _>(
            value.username,
            value.email,
            crate::client::login_request::Identifier::Username,
            crate::client::login_request::Identifier::Email,
        )?;

        Ok(Self {
            password: value.password,
            identifier,
        })
    }
}

#[derive(serde::Deserialize)]
pub struct StartOpaqueLoginRequestDef {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(with = "base64_bytes")]
    credential_request: Vec<u8>,
}

impl TryFrom<StartOpaqueLoginRequestDef> for crate::client::StartOpaqueLoginRequest {
    type Error = serde_json::Error;

    fn try_from(value: StartOpaqueLoginRequestDef) -> Result<Self, Self::Error> {
        let identifier = login_identifier::<serde_json::Error, _>(
            value.username,
            value.email,
            crate::client::start_opaque_login_request::Identifier::Username,
            crate::client::start_opaque_login_request::Identifier::Email,
        )?;

        Ok(Self {
            credential_request: value.credential_request,
            identifier,
        })
    }
}

#[derive(serde::Deserialize)]
pub struct StartPasskeyLoginRequestDef {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    email: Option<String>,
}

impl TryFrom<StartPasskeyLoginRequestDef> for crate::client::StartPasskeyLoginRequest {
    type Error = serde_json::Error;

    fn try_from(value: StartPasskeyLoginRequestDef) -> Result<Self, Self::Error> {
        let identifier = login_identifier::<serde_json::Error, _>(
            value.username,
            value.email,
            crate::client::start_passkey_login_request::Identifier::Username,
            crate::client::start_passkey_login_request::Identifier::Email,
        )?;

        Ok(Self { identifier })
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
    #[serde(default, rename = "playlist_id")]
    playlist_id: String,
    #[serde(default, rename = "before_playlist_id")]
    before: Option<String>,
    #[serde(default, rename = "after_playlist_id")]
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
