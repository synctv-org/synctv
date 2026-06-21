#![allow(clippy::missing_errors_doc)]

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
enum SourceProviderJsonValue {
    String(String),
    Number(i32),
}

fn source_provider_name(value: i32) -> Option<&'static str> {
    match crate::source_config::SourceProvider::try_from(value).ok()? {
        crate::source_config::SourceProvider::Unspecified => Some(""),
        crate::source_config::SourceProvider::DirectUrl => Some("direct_url"),
        crate::source_config::SourceProvider::Bilibili => Some("bilibili"),
        crate::source_config::SourceProvider::Alist => Some("alist"),
        crate::source_config::SourceProvider::Emby => Some("emby"),
        crate::source_config::SourceProvider::Rtmp => Some("rtmp"),
        crate::source_config::SourceProvider::LiveProxy => Some("live_proxy"),
    }
}

fn parse_source_provider_value<E>(value: SourceProviderJsonValue, field_name: &str) -> Result<i32, E>
where
    E: serde::de::Error,
{
    match value {
        SourceProviderJsonValue::Number(value) => {
            crate::source_config::SourceProvider::try_from(value).map_err(E::custom)?;
            Ok(value)
        }
        SourceProviderJsonValue::String(value) => {
            let provider = match value.trim().to_ascii_lowercase().as_str() {
                "" | "unspecified" => crate::source_config::SourceProvider::Unspecified,
                "direct_url" | "directurl" => crate::source_config::SourceProvider::DirectUrl,
                "bilibili" => crate::source_config::SourceProvider::Bilibili,
                "alist" => crate::source_config::SourceProvider::Alist,
                "emby" => crate::source_config::SourceProvider::Emby,
                "rtmp" => crate::source_config::SourceProvider::Rtmp,
                "live_proxy" | "liveproxy" => crate::source_config::SourceProvider::LiveProxy,
                other => {
                    return Err(E::custom(format!("unknown {field_name}: {other}")));
                }
            };
            Ok(provider as i32)
        }
    }
}

/// Generate a scalar + vec serde module pair for a `SourceProvider`-typed field.
/// Both share [`source_provider_name`] / [`parse_source_provider_value`]; only
/// the field name embedded in error messages differs per call.
macro_rules! source_provider_serde {
    ($scalar:ident, $vec:ident, $field:literal) => {
        pub mod $scalar {
            use serde::{ser::Error as _, Deserialize, Deserializer, Serializer};

            pub fn serialize<S>(value: &i32, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let name = super::source_provider_name(*value)
                    .ok_or_else(|| S::Error::custom(format!("unknown {}: {value}", $field)))?;
                serializer.serialize_str(name)
            }

            pub fn deserialize<'de, D>(deserializer: D) -> Result<i32, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = super::SourceProviderJsonValue::deserialize(deserializer)?;
                super::parse_source_provider_value(value, $field)
            }
        }

        pub mod $vec {
            use serde::{ser::Error as _, Deserialize, Deserializer, Serialize, Serializer};

            pub fn serialize<S>(values: &[i32], serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                let names = values
                    .iter()
                    .map(|value| {
                        super::source_provider_name(*value).ok_or_else(|| {
                            S::Error::custom(format!("unknown {}: {value}", $field))
                        })
                    })
                    .collect::<Result<Vec<_>, S::Error>>()?;
                names.serialize(serializer)
            }

            pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<i32>, D::Error>
            where
                D: Deserializer<'de>,
            {
                Vec::<super::SourceProviderJsonValue>::deserialize(deserializer)?
                    .into_iter()
                    .map(|value| super::parse_source_provider_value(value, $field))
                    .collect()
            }
        }
    };
}

source_provider_serde!(source_provider, source_provider_vec, "source_provider");
source_provider_serde!(provider_type, provider_type_vec, "provider_type");

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct BilibiliMediaSourceConfigDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    video: Option<crate::source_config::BilibiliVideoSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pgc: Option<crate::source_config::BilibiliPgcSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    live: Option<crate::source_config::BilibiliLiveSourceConfig>,
}

impl TryFrom<BilibiliMediaSourceConfigDef> for crate::source_config::BilibiliMediaSourceConfig {
    type Error = String;

    fn try_from(value: BilibiliMediaSourceConfigDef) -> Result<Self, Self::Error> {
        use crate::source_config::bilibili_media_source_config::Source;

        let mut sources = [
            value.video.map(Source::Video),
            value.pgc.map(Source::Pgc),
            value.live.map(Source::Live),
        ]
        .into_iter()
        .flatten();
        let source = sources
            .next()
            .ok_or_else(|| "bilibili source_config requires one source".to_string())?;
        if sources.next().is_some() {
            return Err("bilibili source_config accepts exactly one source".to_string());
        }
        Ok(Self {
            source: Some(source),
        })
    }
}

impl From<crate::source_config::BilibiliMediaSourceConfig> for BilibiliMediaSourceConfigDef {
    fn from(value: crate::source_config::BilibiliMediaSourceConfig) -> Self {
        use crate::source_config::bilibili_media_source_config::Source;

        let mut def = Self {
            video: None,
            pgc: None,
            live: None,
        };
        match value.source {
            Some(Source::Video(config)) => def.video = Some(config),
            Some(Source::Pgc(config)) => def.pgc = Some(config),
            Some(Source::Live(config)) => def.live = Some(config),
            None => {}
        }
        def
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct MediaSourceConfigDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    direct_url: Option<crate::source_config::DirectUrlMediaSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bilibili: Option<crate::source_config::BilibiliMediaSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alist: Option<crate::source_config::AlistMediaSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    emby: Option<crate::source_config::EmbyMediaSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    rtmp: Option<crate::source_config::RtmpMediaSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    live_proxy: Option<crate::source_config::LiveProxyMediaSourceConfig>,
}

impl TryFrom<MediaSourceConfigDef> for crate::source_config::MediaSourceConfig {
    type Error = String;

    fn try_from(value: MediaSourceConfigDef) -> Result<Self, Self::Error> {
        use crate::source_config::media_source_config::Provider;

        let mut providers = [
            value.direct_url.map(Provider::DirectUrl),
            value.bilibili.map(Provider::Bilibili),
            value.alist.map(Provider::Alist),
            value.emby.map(Provider::Emby),
            value.rtmp.map(Provider::Rtmp),
            value.live_proxy.map(Provider::LiveProxy),
        ]
        .into_iter()
        .flatten();
        let provider = providers
            .next()
            .ok_or_else(|| "media source_config requires one provider".to_string())?;
        if providers.next().is_some() {
            return Err("media source_config accepts exactly one provider".to_string());
        }
        Ok(Self {
            provider: Some(provider),
        })
    }
}

impl From<crate::source_config::MediaSourceConfig> for MediaSourceConfigDef {
    fn from(value: crate::source_config::MediaSourceConfig) -> Self {
        use crate::source_config::media_source_config::Provider;

        let mut def = Self {
            direct_url: None,
            bilibili: None,
            alist: None,
            emby: None,
            rtmp: None,
            live_proxy: None,
        };
        match value.provider {
            Some(Provider::DirectUrl(config)) => def.direct_url = Some(config),
            Some(Provider::Bilibili(config)) => def.bilibili = Some(config),
            Some(Provider::Alist(config)) => def.alist = Some(config),
            Some(Provider::Emby(config)) => def.emby = Some(config),
            Some(Provider::Rtmp(config)) => def.rtmp = Some(config),
            Some(Provider::LiveProxy(config)) => def.live_proxy = Some(config),
            None => {}
        }
        def
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct PlaylistSourceConfigDef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alist: Option<crate::source_config::AlistPlaylistSourceConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    emby: Option<crate::source_config::EmbyPlaylistSourceConfig>,
}

impl TryFrom<PlaylistSourceConfigDef> for crate::source_config::PlaylistSourceConfig {
    type Error = String;

    fn try_from(value: PlaylistSourceConfigDef) -> Result<Self, Self::Error> {
        use crate::source_config::playlist_source_config::Provider;

        let mut providers = [
            value.alist.map(Provider::Alist),
            value.emby.map(Provider::Emby),
        ]
        .into_iter()
        .flatten();
        let provider = providers
            .next()
            .ok_or_else(|| "playlist source_config requires one provider".to_string())?;
        if providers.next().is_some() {
            return Err("playlist source_config accepts exactly one provider".to_string());
        }
        Ok(Self {
            provider: Some(provider),
        })
    }
}

impl From<crate::source_config::PlaylistSourceConfig> for PlaylistSourceConfigDef {
    fn from(value: crate::source_config::PlaylistSourceConfig) -> Self {
        use crate::source_config::playlist_source_config::Provider;

        let mut def = Self {
            alist: None,
            emby: None,
        };
        match value.provider {
            Some(Provider::Alist(config)) => def.alist = Some(config),
            Some(Provider::Emby(config)) => def.emby = Some(config),
            None => {}
        }
        def
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

/// Resolves a credential oneof where exactly one of two mutually exclusive
/// string options must be present, mapping each to its proto variant.
fn exactly_one_credential<T>(
    first: Option<String>,
    second: Option<String>,
    first_variant: impl FnOnce(String) -> T,
    second_variant: impl FnOnce(String) -> T,
    missing_error: &str,
    conflict_error: &str,
) -> Result<T, String> {
    match (first, second) {
        (Some(first), None) => Ok(first_variant(first)),
        (None, Some(second)) => Ok(second_variant(second)),
        (None, None) => Err(missing_error.into()),
        (Some(_), Some(_)) => Err(conflict_error.into()),
    }
}

#[derive(serde::Deserialize)]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_client_StartOpaqueLoginRequest)
)]
#[serde(deny_unknown_fields)]
pub struct StartOpaqueLoginRequestDef {
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    #[serde(default)]
    pub username: Option<String>,
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    #[serde(default)]
    pub email: Option<String>,
    #[cfg_attr(feature = "openapi", schema(value_type = String, format = Binary))]
    #[serde(with = "base64_bytes")]
    pub credential_request: Vec<u8>,
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
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_client_LoginWithDirectPasswordRequest)
)]
#[serde(deny_unknown_fields)]
pub struct LoginWithDirectPasswordRequestDef {
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    #[serde(default)]
    pub username: Option<String>,
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    #[serde(default)]
    pub email: Option<String>,
    pub password: String,
}

impl TryFrom<LoginWithDirectPasswordRequestDef> for crate::client::LoginWithDirectPasswordRequest {
    type Error = serde_json::Error;

    fn try_from(value: LoginWithDirectPasswordRequestDef) -> Result<Self, Self::Error> {
        let identifier = login_identifier::<serde_json::Error, _>(
            value.username,
            value.email,
            crate::client::login_with_direct_password_request::Identifier::Username,
            crate::client::login_with_direct_password_request::Identifier::Email,
        )?;

        Ok(Self {
            identifier,
            password: value.password,
        })
    }
}

#[derive(serde::Deserialize)]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_client_StartPasskeyLoginRequest)
)]
#[serde(deny_unknown_fields)]
pub struct StartPasskeyLoginRequestDef {
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    #[serde(default)]
    pub username: Option<String>,
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    #[serde(default)]
    pub email: Option<String>,
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

#[cfg(test)]
mod auth_http_serde_tests {
    use super::{
        LoginWithDirectPasswordRequestDef, StartOpaqueLoginRequestDef, StartPasskeyLoginRequestDef,
    };

    #[test]
    fn direct_password_login_json_maps_to_proto_oneof() {
        let value: LoginWithDirectPasswordRequestDef = serde_json::from_str(
            r#"{"email":"user@example.com","password":"correct horse battery staple"}"#,
        )
        .expect("direct password login JSON should decode");
        let request: crate::client::LoginWithDirectPasswordRequest = value
            .try_into()
            .expect("direct password login JSON should convert to proto");

        assert_eq!(request.password, "correct horse battery staple");
        assert!(matches!(
            request.identifier,
            Some(crate::client::login_with_direct_password_request::Identifier::Email(email))
                if email == "user@example.com"
        ));
    }

    #[test]
    fn opaque_login_json_decodes_base64_credential_request() {
        let value: StartOpaqueLoginRequestDef =
            serde_json::from_str(r#"{"username":"alice","credential_request":"AQIDBA=="}"#)
                .expect("opaque login JSON should decode");
        let request: crate::client::StartOpaqueLoginRequest = value
            .try_into()
            .expect("opaque login JSON should convert to proto");

        assert_eq!(request.credential_request, vec![1, 2, 3, 4]);
        assert!(matches!(
            request.identifier,
            Some(crate::client::start_opaque_login_request::Identifier::Username(username))
                if username == "alice"
        ));
    }

    #[test]
    fn login_identifier_requires_one_identifier() {
        let direct: LoginWithDirectPasswordRequestDef = serde_json::from_str(
            r#"{"username":"alice","email":"user@example.com","password":"pw"}"#,
        )
        .expect("direct password login JSON should decode");
        assert!(
            TryInto::<crate::client::LoginWithDirectPasswordRequest>::try_into(direct).is_err()
        );

        let passkey: StartPasskeyLoginRequestDef =
            serde_json::from_str(r#"{"username":"alice","email":"user@example.com"}"#)
                .expect("passkey login JSON should decode");
        assert!(TryInto::<crate::client::StartPasskeyLoginRequest>::try_into(passkey).is_err());
    }
}

#[derive(serde::Deserialize)]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_client_UpdateRoomSettingsRequest)
)]
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
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_admin_UpdateRoomSettingsRequest)
)]
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
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_client_MovePlaylistRequest)
)]
#[serde(deny_unknown_fields)]
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

#[cfg(test)]
mod room_http_serde_tests {
    use super::{
        AdminUpdateRoomSettingsRequestDef, ClientUpdateRoomSettingsRequestDef,
        MovePlaylistRequestDef,
    };

    #[test]
    fn client_room_settings_accepts_raw_json_object() {
        let value: ClientUpdateRoomSettingsRequestDef =
            serde_json::from_str(r#"{"chat":{"enabled":true}}"#)
                .expect("raw room settings JSON should decode");
        let request = crate::client::UpdateRoomSettingsRequest::from(value);
        let settings: serde_json::Value =
            serde_json::from_slice(&request.settings).expect("settings should be JSON");

        assert_eq!(settings["chat"]["enabled"], true);
    }

    #[test]
    fn admin_room_settings_accepts_wrapped_json_object() {
        let value: AdminUpdateRoomSettingsRequestDef = serde_json::from_str(
            r#"{"room_id":"room_ignored_by_path","settings":{"chat":{"enabled":false}}}"#,
        )
        .expect("wrapped admin room settings JSON should decode");
        let request = crate::admin::UpdateRoomSettingsRequest::from(value);
        let settings: serde_json::Value =
            serde_json::from_slice(&request.settings).expect("settings should be JSON");

        assert_eq!(request.room_id, "room_ignored_by_path");
        assert_eq!(settings["chat"]["enabled"], false);
    }

    #[test]
    fn move_playlist_json_maps_anchor_to_proto_oneof() {
        let value: MovePlaylistRequestDef =
            serde_json::from_str(r#"{"after_playlist_id":"pl_after"}"#)
                .expect("move playlist JSON should decode");
        let request = crate::client::MovePlaylistRequest::try_from(value)
            .expect("move playlist JSON should convert to proto");

        assert!(matches!(
            request.anchor,
            Some(crate::client::move_playlist_request::Anchor::AfterPlaylistId(anchor))
                if anchor == "pl_after"
        ));
    }

    #[test]
    fn move_playlist_json_rejects_two_anchors() {
        let value: MovePlaylistRequestDef = serde_json::from_str(
            r#"{"before_playlist_id":"pl_before","after_playlist_id":"pl_after"}"#,
        )
        .expect("move playlist JSON should decode");

        assert!(crate::client::MovePlaylistRequest::try_from(value).is_err());
    }
}

#[derive(serde::Deserialize)]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_provider_alist_LoginRequest)
)]
#[serde(deny_unknown_fields)]
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
        let credential = Some(alist_credential(value.password, value.hashed_password)?);

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
        alist_credential(value.password, value.hashed_password)
    }
}

fn alist_credential(
    password: Option<String>,
    hashed_password: Option<String>,
) -> Result<crate::providers::alist::login_request::Credential, String> {
    exactly_one_credential(
        password,
        hashed_password,
        crate::providers::alist::login_request::Credential::Password,
        crate::providers::alist::login_request::Credential::HashedPassword,
        "exactly one of password or hashed_password must be provided",
        "password and hashed_password are mutually exclusive",
    )
}

#[derive(serde::Deserialize)]
#[cfg_attr(
    feature = "openapi",
    derive(utoipa::ToSchema),
    schema(as = synctv_provider_emby_LoginRequest)
)]
#[serde(deny_unknown_fields)]
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
        let credential = Some(emby_credential(value.password, value.api_key)?);

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
        emby_credential(value.password, value.api_key)
    }
}

fn emby_credential(
    password: Option<String>,
    api_key: Option<String>,
) -> Result<crate::providers::emby::login_request::Credential, String> {
    exactly_one_credential(
        password,
        api_key,
        crate::providers::emby::login_request::Credential::Password,
        crate::providers::emby::login_request::Credential::ApiKey,
        "exactly one of password or api_key must be provided",
        "password and api_key are mutually exclusive",
    )
}

#[cfg(test)]
mod provider_http_serde_tests {
    use super::{AlistLoginRequestDef, EmbyLoginRequestDef};

    #[test]
    fn alist_login_json_maps_plain_password_to_proto_oneof() {
        let value: AlistLoginRequestDef = serde_json::from_str(
            r#"{"host":"https://alist.example","username":"alice","password":"pw","otp_code":"123456"}"#,
        )
        .expect("Alist login JSON should decode");
        let request = crate::providers::alist::LoginRequest::try_from(value)
            .expect("Alist login JSON should convert to proto");

        assert_eq!(request.host, "https://alist.example");
        assert_eq!(request.username, "alice");
        assert_eq!(request.otp_code, "123456");
        assert!(matches!(
            request.credential,
            Some(crate::providers::alist::login_request::Credential::Password(password))
                if password == "pw"
        ));
    }

    #[test]
    fn alist_login_json_rejects_missing_or_duplicate_credential() {
        let missing: AlistLoginRequestDef =
            serde_json::from_str(r#"{"host":"https://alist.example","username":"alice"}"#)
                .expect("Alist login JSON should decode");
        assert!(crate::providers::alist::LoginRequest::try_from(missing).is_err());

        let duplicate: AlistLoginRequestDef = serde_json::from_str(
            r#"{"host":"https://alist.example","username":"alice","password":"pw","hashed_password":"hash"}"#,
        )
        .expect("Alist login JSON should decode");
        assert!(crate::providers::alist::LoginRequest::try_from(duplicate).is_err());
    }

    #[test]
    fn emby_login_json_maps_api_key_to_proto_oneof() {
        let value: EmbyLoginRequestDef = serde_json::from_str(
            r#"{"host":"https://emby.example","username":"alice","api_key":"key"}"#,
        )
        .expect("Emby login JSON should decode");
        let request = crate::providers::emby::LoginRequest::try_from(value)
            .expect("Emby login JSON should convert to proto");

        assert_eq!(request.host, "https://emby.example");
        assert_eq!(request.username, "alice");
        assert!(matches!(
            request.credential,
            Some(crate::providers::emby::login_request::Credential::ApiKey(api_key))
                if api_key == "key"
        ));
    }

    #[test]
    fn emby_login_json_rejects_missing_or_duplicate_credential() {
        let missing: EmbyLoginRequestDef =
            serde_json::from_str(r#"{"host":"https://emby.example","username":"alice"}"#)
                .expect("Emby login JSON should decode");
        assert!(crate::providers::emby::LoginRequest::try_from(missing).is_err());

        let duplicate: EmbyLoginRequestDef = serde_json::from_str(
            r#"{"host":"https://emby.example","username":"alice","password":"pw","api_key":"key"}"#,
        )
        .expect("Emby login JSON should decode");
        assert!(crate::providers::emby::LoginRequest::try_from(duplicate).is_err());
    }
}
