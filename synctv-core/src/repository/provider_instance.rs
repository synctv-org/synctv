// Provider Instance Repository
// Database access layer for provider instance configuration management.

use crate::credential_encryption::CredentialEncryption;
use crate::models::{
    normalize_provider_instance_name, provider_type_code_from_name, provider_type_name_from_code,
    ProviderCredential, ProviderInstance, ProviderInstanceListQuery, ProviderInstanceListSortBy,
    SourceProvider, UserId, UserProviderCredential,
};
use crate::repository::pools::RepoPools;
use crate::Result;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

#[derive(Debug, sqlx::FromRow)]
struct ProviderInstanceRow {
    name: String,
    endpoint: String,
    comment: Option<String>,
    jwt_secret: Option<String>,
    custom_ca: Option<String>,
    timeout: String,
    tls: bool,
    insecure_tls: bool,
    providers: Vec<i16>,
    enabled: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

impl TryFrom<ProviderInstanceRow> for ProviderInstance {
    type Error = crate::Error;

    fn try_from(row: ProviderInstanceRow) -> Result<Self> {
        let providers = row
            .providers
            .into_iter()
            .map(SourceProvider::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(crate::Error::InvalidInput)?;

        Ok(Self {
            name: row.name,
            endpoint: row.endpoint,
            comment: row.comment,
            jwt_secret: row.jwt_secret,
            custom_ca: row.custom_ca,
            timeout: row.timeout,
            tls: row.tls,
            insecure_tls: row.insecure_tls,
            providers,
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }
}

#[derive(Debug, sqlx::FromRow)]
struct UserProviderCredentialRow {
    id: i64,
    user_id: UserId,
    provider: i16,
    server_id: String,
    provider_instance_name: Option<String>,
    credential_data: StoredProviderCredential,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    created_at: chrono::DateTime<chrono::Utc>,
    updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
struct EncryptedCredentialValue(String);

#[derive(Clone, Copy)]
struct CredentialEncryptionContext<'a> {
    encryption: Option<&'a CredentialEncryption>,
    aad: &'a [u8],
}

impl EncryptedCredentialValue {
    fn encrypt_json(
        context: CredentialEncryptionContext<'_>,
        value: &serde_json::Value,
    ) -> Result<Self> {
        let encrypted = match context.encryption {
            Some(enc) => enc.encrypt_with_context(value, context.aad),
            None => Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider credentials"
                    .to_string(),
            )),
        }?;
        Ok(Self(encrypted))
    }

    #[cfg(test)]
    fn from_string_for_test(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    fn encrypt_string(context: CredentialEncryptionContext<'_>, value: &str) -> Result<Self> {
        Self::encrypt_json(context, &serde_json::Value::String(value.to_string()))
    }

    fn decrypt_json(&self, context: CredentialEncryptionContext<'_>) -> Result<serde_json::Value> {
        match context.encryption {
            Some(enc) => enc.decrypt_with_context(&self.0, context.aad),
            None => Err(crate::Error::Internal(
                "Credential encryption must be configured before reading provider credentials"
                    .to_string(),
            )),
        }
    }

    fn decrypt_string(&self, context: CredentialEncryptionContext<'_>) -> Result<String> {
        let value = self.decrypt_json(context)?;
        match value {
            serde_json::Value::String(value) => Ok(value),
            other => Err(crate::Error::Internal(format!(
                "Decrypted credential field must be a string, got {other}."
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum StoredProviderCredential {
    #[serde(rename = "bilibili")]
    Bilibili { cookies: EncryptedCredentialValue },
    #[serde(rename = "alist")]
    Alist {
        host: String,
        username: String,
        password: EncryptedCredentialValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        otp_secret: Option<EncryptedCredentialValue>,
    },
    #[serde(rename = "emby")]
    Emby {
        host: String,
        api_key: EncryptedCredentialValue,
        emby_user_id: String,
    },
    #[serde(rename = "cloudreve")]
    Cloudreve {
        host: String,
        email: String,
        password: EncryptedCredentialValue,
    },
    #[serde(rename = "twitch")]
    Twitch {
        login: String,
        twitch_user_id: String,
        client_id: String,
        #[serde(default)]
        scopes: Vec<String>,
        auth_token: EncryptedCredentialValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<EncryptedCredentialValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_integrity: Option<EncryptedCredentialValue>,
    },
    #[serde(rename = "youtube")]
    Youtube {
        label: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        visitor_data: Option<EncryptedCredentialValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        po_token: Option<EncryptedCredentialValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cookie: Option<EncryptedCredentialValue>,
    },
    #[serde(rename = "douyin")]
    Douyin {
        label: String,
        cookie: EncryptedCredentialValue,
    },
    #[serde(rename = "tiktok")]
    TikTok {
        label: String,
        cookie: EncryptedCredentialValue,
    },
    #[serde(rename = "fnos")]
    Fnos {
        endpoint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        webdav_endpoint: Option<String>,
        username: String,
        password: EncryptedCredentialValue,
        token: EncryptedCredentialValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        long_token: Option<EncryptedCredentialValue>,
        secret: EncryptedCredentialValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_endpoint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        media_token: Option<EncryptedCredentialValue>,
    },
    #[serde(rename = "qnap")]
    Qnap {
        endpoint: String,
        username: String,
        password: EncryptedCredentialValue,
        sid: EncryptedCredentialValue,
        server_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        version: Option<String>,
        support_rtt: bool,
    },
    #[serde(rename = "synology")]
    Synology {
        endpoint: String,
        username: String,
        password: EncryptedCredentialValue,
        file_sid: EncryptedCredentialValue,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        video_sid: Option<EncryptedCredentialValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_id: Option<EncryptedCredentialValue>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        synotoken: Option<EncryptedCredentialValue>,
        apis: std::collections::HashMap<String, crate::models::SynologyApiBinding>,
    },
    #[serde(rename = "nextcloud")]
    Nextcloud {
        endpoint: String,
        username: String,
        user_id: String,
        app_password: EncryptedCredentialValue,
        version: String,
        edition: String,
        capabilities: serde_json::Value,
    },
    #[serde(rename = "seafile")]
    Seafile {
        endpoint: String,
        username: String,
        token: EncryptedCredentialValue,
        version: String,
        features: Vec<String>,
        library_passwords: EncryptedCredentialValue,
    },
    #[serde(rename = "truenas")]
    TrueNas {
        endpoint: String,
        api_key: EncryptedCredentialValue,
        hostname: String,
        version: String,
        system_product: String,
    },
}

impl StoredProviderCredential {
    fn encrypt_from_domain(
        encryption: Option<&CredentialEncryption>,
        data: &ProviderCredential,
        aad: &[u8],
    ) -> Result<Self> {
        let encryption = CredentialEncryptionContext { encryption, aad };
        match data {
            ProviderCredential::Bilibili { cookies } => Ok(Self::Bilibili {
                cookies: EncryptedCredentialValue::encrypt_json(
                    encryption,
                    &serde_json::to_value(cookies)?,
                )?,
            }),
            ProviderCredential::Alist {
                host,
                username,
                password,
                otp_secret,
            } => Ok(Self::Alist {
                host: host.clone(),
                username: username.clone(),
                password: EncryptedCredentialValue::encrypt_string(encryption, password)?,
                otp_secret: otp_secret
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
            }),
            ProviderCredential::Emby {
                host,
                api_key,
                emby_user_id,
            } => Ok(Self::Emby {
                host: host.clone(),
                api_key: EncryptedCredentialValue::encrypt_string(encryption, api_key)?,
                emby_user_id: emby_user_id.clone(),
            }),
            ProviderCredential::Cloudreve {
                host,
                email,
                password,
            } => Ok(Self::Cloudreve {
                host: host.clone(),
                email: email.clone(),
                password: EncryptedCredentialValue::encrypt_string(encryption, password)?,
            }),
            ProviderCredential::Twitch {
                login,
                twitch_user_id,
                client_id,
                scopes,
                auth_token,
                device_id,
                client_integrity,
            } => Ok(Self::Twitch {
                login: login.clone(),
                twitch_user_id: twitch_user_id.clone(),
                client_id: client_id.clone(),
                scopes: scopes.clone(),
                auth_token: EncryptedCredentialValue::encrypt_string(encryption, auth_token)?,
                device_id: device_id
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
                client_integrity: client_integrity
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
            }),
            ProviderCredential::Youtube {
                label,
                visitor_data,
                po_token,
                cookie,
            } => Ok(Self::Youtube {
                label: label.clone(),
                visitor_data: visitor_data
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
                po_token: po_token
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
                cookie: cookie
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
            }),
            ProviderCredential::Douyin { label, cookie } => Ok(Self::Douyin {
                label: label.clone(),
                cookie: EncryptedCredentialValue::encrypt_string(encryption, cookie)?,
            }),
            ProviderCredential::TikTok { label, cookie } => Ok(Self::TikTok {
                label: label.clone(),
                cookie: EncryptedCredentialValue::encrypt_string(encryption, cookie)?,
            }),
            ProviderCredential::Fnos {
                endpoint,
                webdav_endpoint,
                username,
                password,
                token,
                long_token,
                secret,
                media_endpoint,
                media_token,
            } => Ok(Self::Fnos {
                endpoint: endpoint.clone(),
                webdav_endpoint: webdav_endpoint.clone(),
                username: username.clone(),
                password: EncryptedCredentialValue::encrypt_string(encryption, password)?,
                token: EncryptedCredentialValue::encrypt_string(encryption, token)?,
                long_token: long_token
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
                secret: EncryptedCredentialValue::encrypt_string(encryption, secret)?,
                media_endpoint: media_endpoint.clone(),
                media_token: media_token
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
            }),
            ProviderCredential::Qnap {
                endpoint,
                username,
                password,
                sid,
                server_name,
                version,
                support_rtt,
            } => Ok(Self::Qnap {
                endpoint: endpoint.clone(),
                username: username.clone(),
                password: EncryptedCredentialValue::encrypt_string(encryption, password)?,
                sid: EncryptedCredentialValue::encrypt_string(encryption, sid)?,
                server_name: server_name.clone(),
                version: version.clone(),
                support_rtt: *support_rtt,
            }),
            ProviderCredential::Synology {
                endpoint,
                username,
                password,
                file_sid,
                video_sid,
                device_id,
                synotoken,
                apis,
            } => Ok(Self::Synology {
                endpoint: endpoint.clone(),
                username: username.clone(),
                password: EncryptedCredentialValue::encrypt_string(encryption, password)?,
                file_sid: EncryptedCredentialValue::encrypt_string(encryption, file_sid)?,
                video_sid: video_sid
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
                device_id: device_id
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
                synotoken: synotoken
                    .as_deref()
                    .map(|value| EncryptedCredentialValue::encrypt_string(encryption, value))
                    .transpose()?,
                apis: apis.clone(),
            }),
            ProviderCredential::Nextcloud {
                endpoint,
                username,
                user_id,
                app_password,
                version,
                edition,
                capabilities,
            } => Ok(Self::Nextcloud {
                endpoint: endpoint.clone(),
                username: username.clone(),
                user_id: user_id.clone(),
                app_password: EncryptedCredentialValue::encrypt_string(encryption, app_password)?,
                version: version.clone(),
                edition: edition.clone(),
                capabilities: capabilities.clone(),
            }),
            ProviderCredential::Seafile {
                endpoint,
                username,
                token,
                version,
                features,
                library_passwords,
            } => Ok(Self::Seafile {
                endpoint: endpoint.clone(),
                username: username.clone(),
                token: EncryptedCredentialValue::encrypt_string(encryption, token)?,
                version: version.clone(),
                features: features.clone(),
                library_passwords: EncryptedCredentialValue::encrypt_json(
                    encryption,
                    &serde_json::to_value(library_passwords)?,
                )?,
            }),
            ProviderCredential::TrueNas {
                endpoint,
                api_key,
                hostname,
                version,
                system_product,
            } => Ok(Self::TrueNas {
                endpoint: endpoint.clone(),
                api_key: EncryptedCredentialValue::encrypt_string(encryption, api_key)?,
                hostname: hostname.clone(),
                version: version.clone(),
                system_product: system_product.clone(),
            }),
        }
    }

    fn decrypt_to_domain(
        &self,
        encryption: Option<&CredentialEncryption>,
        aad: &[u8],
    ) -> Result<ProviderCredential> {
        let encryption = CredentialEncryptionContext { encryption, aad };
        match self {
            Self::Bilibili { cookies } => Ok(ProviderCredential::Bilibili {
                cookies: serde_json::from_value(cookies.decrypt_json(encryption)?)?,
            }),
            Self::Alist {
                host,
                username,
                password,
                otp_secret,
            } => Ok(ProviderCredential::Alist {
                host: host.clone(),
                username: username.clone(),
                password: password.decrypt_string(encryption)?,
                otp_secret: otp_secret
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
            }),
            Self::Emby {
                host,
                api_key,
                emby_user_id,
            } => Ok(ProviderCredential::Emby {
                host: host.clone(),
                api_key: api_key.decrypt_string(encryption)?,
                emby_user_id: emby_user_id.clone(),
            }),
            Self::Cloudreve {
                host,
                email,
                password,
            } => Ok(ProviderCredential::Cloudreve {
                host: host.clone(),
                email: email.clone(),
                password: password.decrypt_string(encryption)?,
            }),
            Self::Twitch {
                login,
                twitch_user_id,
                client_id,
                scopes,
                auth_token,
                device_id,
                client_integrity,
            } => Ok(ProviderCredential::Twitch {
                login: login.clone(),
                twitch_user_id: twitch_user_id.clone(),
                client_id: client_id.clone(),
                scopes: scopes.clone(),
                auth_token: auth_token.decrypt_string(encryption)?,
                device_id: device_id
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
                client_integrity: client_integrity
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
            }),
            Self::Youtube {
                label,
                visitor_data,
                po_token,
                cookie,
            } => Ok(ProviderCredential::Youtube {
                label: label.clone(),
                visitor_data: visitor_data
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
                po_token: po_token
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
                cookie: cookie
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
            }),
            Self::Douyin { label, cookie } => Ok(ProviderCredential::Douyin {
                label: label.clone(),
                cookie: cookie.decrypt_string(encryption)?,
            }),
            Self::TikTok { label, cookie } => Ok(ProviderCredential::TikTok {
                label: label.clone(),
                cookie: cookie.decrypt_string(encryption)?,
            }),
            Self::Fnos {
                endpoint,
                webdav_endpoint,
                username,
                password,
                token,
                long_token,
                secret,
                media_endpoint,
                media_token,
            } => Ok(ProviderCredential::Fnos {
                endpoint: endpoint.clone(),
                webdav_endpoint: webdav_endpoint.clone(),
                username: username.clone(),
                password: password.decrypt_string(encryption)?,
                token: token.decrypt_string(encryption)?,
                long_token: long_token
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
                secret: secret.decrypt_string(encryption)?,
                media_endpoint: media_endpoint.clone(),
                media_token: media_token
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
            }),
            Self::Qnap {
                endpoint,
                username,
                password,
                sid,
                server_name,
                version,
                support_rtt,
            } => Ok(ProviderCredential::Qnap {
                endpoint: endpoint.clone(),
                username: username.clone(),
                password: password.decrypt_string(encryption)?,
                sid: sid.decrypt_string(encryption)?,
                server_name: server_name.clone(),
                version: version.clone(),
                support_rtt: *support_rtt,
            }),
            Self::Synology {
                endpoint,
                username,
                password,
                file_sid,
                video_sid,
                device_id,
                synotoken,
                apis,
            } => Ok(ProviderCredential::Synology {
                endpoint: endpoint.clone(),
                username: username.clone(),
                password: password.decrypt_string(encryption)?,
                file_sid: file_sid.decrypt_string(encryption)?,
                video_sid: video_sid
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
                device_id: device_id
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
                synotoken: synotoken
                    .as_ref()
                    .map(|value| value.decrypt_string(encryption))
                    .transpose()?,
                apis: apis.clone(),
            }),
            Self::Nextcloud {
                endpoint,
                username,
                user_id,
                app_password,
                version,
                edition,
                capabilities,
            } => Ok(ProviderCredential::Nextcloud {
                endpoint: endpoint.clone(),
                username: username.clone(),
                user_id: user_id.clone(),
                app_password: app_password.decrypt_string(encryption)?,
                version: version.clone(),
                edition: edition.clone(),
                capabilities: capabilities.clone(),
            }),
            Self::Seafile {
                endpoint,
                username,
                token,
                version,
                features,
                library_passwords,
            } => Ok(ProviderCredential::Seafile {
                endpoint: endpoint.clone(),
                username: username.clone(),
                token: token.decrypt_string(encryption)?,
                version: version.clone(),
                features: features.clone(),
                library_passwords: serde_json::from_value(
                    library_passwords.decrypt_json(encryption)?,
                )?,
            }),
            Self::TrueNas {
                endpoint,
                api_key,
                hostname,
                version,
                system_product,
            } => Ok(ProviderCredential::TrueNas {
                endpoint: endpoint.clone(),
                api_key: api_key.decrypt_string(encryption)?,
                hostname: hostname.clone(),
                version: version.clone(),
                system_product: system_product.clone(),
            }),
        }
    }
}

impl sqlx::Type<sqlx::Postgres> for StoredProviderCredential {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <sqlx::types::Json<Self> as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <sqlx::types::Json<Self> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for StoredProviderCredential {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> std::result::Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for StoredProviderCredential {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(value) =
            <sqlx::types::Json<Self> as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(value)
    }
}

fn provider_type_code(provider: &str) -> Result<i16> {
    provider_type_code_from_name(provider).map_err(crate::Error::InvalidInput)
}

fn provider_type_codes(providers: &[SourceProvider]) -> Vec<i16> {
    providers
        .iter()
        .copied()
        .map(SourceProvider::as_i16)
        .collect()
}

/// Provider Instance Repository
///
/// Encrypts sensitive fields (`jwt_secret`, `custom_ca`) using `CredentialEncryption`
/// before storage and decrypts after read. Encryption is mandatory.
#[derive(Clone)]
pub struct ProviderInstanceRepository {
    pools: RepoPools,
    encryption: Option<CredentialEncryption>,
}

impl std::fmt::Debug for ProviderInstanceRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderInstanceRepository")
            .field("pools", &"RepoPools")
            .field("encryption", &self.encryption.is_some())
            .finish()
    }
}

impl ProviderInstanceRepository {
    const INSTANCE_SELECT_COLUMNS: &'static str = "name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled, created_at, updated_at";

    fn is_provider_instance_reference_violation(db_err: &dyn sqlx::error::DatabaseError) -> bool {
        db_err.code().as_deref() == Some("23503")
            || db_err
                .constraint()
                .is_some_and(|constraint| constraint.contains("provider_instance"))
            || db_err.message().contains("foreign key constraint")
                && db_err.message().contains("media_provider_instances")
    }

    fn push_list_order_by(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        query: &ProviderInstanceListQuery,
    ) {
        use crate::models::SortDirection;

        let order_by = match (query.sort_by, query.sort_direction) {
            (ProviderInstanceListSortBy::Name, SortDirection::Asc) => {
                " ORDER BY name ASC, created_at ASC"
            }
            (ProviderInstanceListSortBy::Name, SortDirection::Desc) => {
                " ORDER BY name DESC, created_at DESC"
            }
            (ProviderInstanceListSortBy::Endpoint, SortDirection::Asc) => {
                " ORDER BY endpoint ASC, created_at ASC"
            }
            (ProviderInstanceListSortBy::Endpoint, SortDirection::Desc) => {
                " ORDER BY endpoint DESC, created_at DESC"
            }
            (ProviderInstanceListSortBy::UpdatedAt, SortDirection::Asc) => {
                " ORDER BY updated_at ASC, name ASC"
            }
            (ProviderInstanceListSortBy::UpdatedAt, SortDirection::Desc) => {
                " ORDER BY updated_at DESC, name DESC"
            }
            (ProviderInstanceListSortBy::CreatedAt, SortDirection::Asc) => {
                " ORDER BY created_at ASC, name ASC"
            }
            (ProviderInstanceListSortBy::CreatedAt, SortDirection::Desc) => {
                " ORDER BY created_at DESC, name DESC"
            }
        };
        builder.push(order_by);
    }

    fn push_list_filters(
        builder: &mut sqlx::QueryBuilder<sqlx::Postgres>,
        query: &ProviderInstanceListQuery,
    ) -> Result<()> {
        builder.push(" WHERE TRUE");

        if let Some(provider_type) = &query.provider_type {
            builder.push(" AND ");
            builder.push_bind(provider_type.as_i16());
            builder.push(" = ANY(providers)");
        }
        if let Some(enabled) = query.enabled {
            builder.push(" AND enabled = ");
            builder.push_bind(enabled);
        }
        if let Some(tls) = query.tls {
            builder.push(" AND tls = ");
            builder.push_bind(tls);
        }
        if let Some(search) = query
            .search
            .as_deref()
            .and_then(super::query_builder::normalize_search_text)
        {
            builder.push(" AND (");
            let mut has_search_condition = false;
            if let Some(pattern) = super::query_builder::ilike_contains_pattern(&search) {
                builder.push("name ILIKE ");
                builder.push_bind(pattern.clone());
                builder.push(" ESCAPE '\\' OR endpoint ILIKE ");
                builder.push_bind(pattern.clone());
                builder.push(" ESCAPE '\\' OR COALESCE(comment, '') ILIKE ");
                builder.push_bind(pattern);
                builder.push(" ESCAPE '\\'");
                has_search_condition = true;
            }
            if let Ok(provider_code) = provider_type_code(&search) {
                if has_search_condition {
                    builder.push(" OR ");
                }
                builder.push_bind(provider_code);
                builder.push(" = ANY(providers)");
                has_search_condition = true;
            }
            if !has_search_condition {
                builder.push("FALSE");
            }
            builder.push(")");
        }
        Ok(())
    }

    fn sensitive_fields_present(instance: &ProviderInstance) -> bool {
        instance
            .jwt_secret
            .as_ref()
            .is_some_and(|value| !value.trim().is_empty())
            || instance
                .custom_ca
                .as_ref()
                .is_some_and(|value| !value.trim().is_empty())
    }

    fn ensure_encryption_for_sensitive_fields_with(
        encryption: Option<&CredentialEncryption>,
        instance: &ProviderInstance,
    ) -> Result<()> {
        if encryption.is_none() && Self::sensitive_fields_present(instance) {
            return Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider instance secrets"
                    .to_string(),
            ));
        }
        Ok(())
    }

    fn decrypt_field_with(
        encryption: Option<&CredentialEncryption>,
        name: &str,
        field: &str,
        stored: Option<&str>,
    ) -> Result<Option<String>> {
        match (encryption, stored) {
            (Some(enc), Some(value)) if value.starts_with("enc:") => {
                let context = format!("provider-instance:v1\0{name}\0{field}");
                let decrypted = enc.decrypt_with_context(value, context.as_bytes())?;
                match decrypted {
                    serde_json::Value::String(s) => Ok(Some(s)),
                    other => Ok(Some(other.to_string())),
                }
            }
            (Some(_), Some(value)) if !value.is_empty() => Err(crate::Error::Internal(
                "Provider instance contains plaintext sensitive data while credential encryption is enabled"
                    .to_string(),
            )),
            (None, Some(value)) if !value.trim().is_empty() => Err(crate::Error::Internal(
                "Credential encryption must be configured before reading provider instance secrets"
                    .to_string(),
            )),
            _ => Ok(None),
        }
    }

    /// Create a new repository without encryption
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pools: RepoPools::new(pool),
            encryption: None,
        }
    }

    /// Create a new repository with a dedicated pool for eventually-consistent reads.
    #[must_use]
    pub const fn new_with_read_pool(pool: PgPool, read_pool: PgPool) -> Self {
        Self {
            pools: RepoPools::with_read(pool, read_pool),
            encryption: None,
        }
    }

    /// Create a new repository with credential encryption enabled
    #[must_use]
    pub const fn new_with_encryption(pool: PgPool, encryption: CredentialEncryption) -> Self {
        Self {
            pools: RepoPools::new(pool),
            encryption: Some(encryption),
        }
    }

    /// Create a new encrypted repository with a dedicated pool for eventually-consistent reads.
    #[must_use]
    pub const fn new_with_encryption_and_read_pool(
        pool: PgPool,
        read_pool: PgPool,
        encryption: CredentialEncryption,
    ) -> Self {
        Self {
            pools: RepoPools::with_read(pool, read_pool),
            encryption: Some(encryption),
        }
    }

    fn eventually_consistent_pool(&self) -> &PgPool {
        self.pools.read()
    }

    fn primary_pool(&self) -> &PgPool {
        self.pools.primary()
    }

    fn encrypt_field(
        &self,
        name: &str,
        field: &str,
        plaintext: Option<&str>,
    ) -> Result<Option<String>> {
        match (&self.encryption, plaintext) {
            (Some(enc), Some(value)) if !value.is_empty() => {
                let json_value = serde_json::Value::String(value.to_owned());
                let context = format!("provider-instance:v1\0{name}\0{field}");
                let encrypted = enc.encrypt_with_context(&json_value, context.as_bytes())?;
                Ok(Some(encrypted))
            }
            (None, Some(value)) if !value.trim().is_empty() => Err(crate::Error::Internal(
                "Credential encryption must be configured before storing provider instance secrets"
                    .to_string(),
            )),
            _ => Ok(None),
        }
    }

    fn ensure_encryption_for_sensitive_fields(&self, instance: &ProviderInstance) -> Result<()> {
        Self::ensure_encryption_for_sensitive_fields_with(self.encryption.as_ref(), instance)
    }

    fn decrypt_field(
        &self,
        name: &str,
        field: &str,
        stored: Option<&str>,
    ) -> Result<Option<String>> {
        Self::decrypt_field_with(self.encryption.as_ref(), name, field, stored)
    }

    /// Decrypt sensitive fields on a `ProviderInstance` after reading from DB.
    fn decrypt_instance(&self, mut instance: ProviderInstance) -> Result<ProviderInstance> {
        instance.jwt_secret =
            self.decrypt_field(&instance.name, "jwt_secret", instance.jwt_secret.as_deref())?;
        instance.custom_ca =
            self.decrypt_field(&instance.name, "custom_ca", instance.custom_ca.as_deref())?;
        Ok(instance)
    }

    fn decrypt_instance_row(&self, row: ProviderInstanceRow) -> Result<ProviderInstance> {
        self.decrypt_instance(row.try_into()?)
    }

    /// Decrypt sensitive fields on a list of `ProviderInstance`.
    fn decrypt_instance_rows(
        &self,
        rows: Vec<ProviderInstanceRow>,
    ) -> Result<Vec<ProviderInstance>> {
        rows.into_iter()
            .map(|row| self.decrypt_instance_row(row))
            .collect()
    }

    /// Get all provider instances (sensitive fields decrypted)
    pub async fn get_all(&self) -> Result<Vec<ProviderInstance>> {
        let rows = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            ORDER BY created_at DESC
            ",
        )
        .fetch_all(self.eventually_consistent_pool())
        .await?;
        self.decrypt_instance_rows(rows)
    }

    /// Get all enabled provider instances (sensitive fields decrypted)
    pub async fn get_all_enabled(&self) -> Result<Vec<ProviderInstance>> {
        self.get_all_enabled_from_primary().await
    }

    /// Get all enabled provider instances from the primary database.
    ///
    /// These rows contain connection-building inputs and secrets. Reading them
    /// from a lagging replica can cache stale enabled credentials.
    pub async fn get_all_enabled_from_primary(&self) -> Result<Vec<ProviderInstance>> {
        let rows = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            WHERE enabled = true
            ORDER BY created_at DESC
            ",
        )
        .fetch_all(self.primary_pool())
        .await?;
        self.decrypt_instance_rows(rows)
    }

    /// Get provider instance by name (sensitive fields decrypted)
    pub async fn get_by_name(&self, name: &str) -> Result<Option<ProviderInstance>> {
        let row = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            WHERE name = $1
            ",
            name,
        )
        .fetch_optional(self.primary_pool())
        .await?;
        match row {
            Some(row) => Ok(Some(self.decrypt_instance_row(row)?)),
            None => Ok(None),
        }
    }

    pub async fn list(&self, query: &ProviderInstanceListQuery) -> Result<Vec<ProviderInstance>> {
        self.list_with_total(query)
            .await
            .map(|(instances, _)| instances)
    }

    pub async fn list_with_total(
        &self,
        query: &ProviderInstanceListQuery,
    ) -> Result<(Vec<ProviderInstance>, i64)> {
        let limit = query.pagination.limit_i64()?;
        let offset = query.pagination.offset_i64()?;

        let mut count_builder = sqlx::QueryBuilder::<sqlx::Postgres>::new(
            "SELECT COUNT(*) FROM media_provider_instances",
        );
        Self::push_list_filters(&mut count_builder, query)?;
        let total = count_builder
            .build_query_scalar::<i64>()
            .fetch_one(self.eventually_consistent_pool())
            .await?;

        let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT ");
        builder.push(Self::INSTANCE_SELECT_COLUMNS);
        builder.push(" FROM media_provider_instances");
        Self::push_list_filters(&mut builder, query)?;
        Self::push_list_order_by(&mut builder, query);
        builder.push(" LIMIT ");
        builder.push_bind(limit);
        builder.push(" OFFSET ");
        builder.push_bind(offset);

        let rows = builder
            .build_query_as::<ProviderInstanceRow>()
            .fetch_all(self.eventually_consistent_pool())
            .await?;
        Ok((self.decrypt_instance_rows(rows)?, total))
    }

    /// Get instances that support a specific provider type (sensitive fields decrypted)
    pub async fn find_by_provider(&self, provider: &str) -> Result<Vec<ProviderInstance>> {
        self.find_by_provider_from_primary(provider).await
    }

    /// Get enabled instances that support a provider type from the primary database.
    pub async fn find_by_provider_from_primary(
        &self,
        provider: &str,
    ) -> Result<Vec<ProviderInstance>> {
        let rows = sqlx::query_as!(
            ProviderInstanceRow,
            r"
            SELECT name, endpoint, comment, jwt_secret, custom_ca, timeout, tls,
                   insecure_tls, providers, enabled, created_at, updated_at
            FROM media_provider_instances
            WHERE $1 = ANY(providers) AND enabled = true
            ",
            provider_type_code(provider)?,
        )
        .fetch_all(self.primary_pool())
        .await?;
        self.decrypt_instance_rows(rows)
    }

    /// Create a new provider instance (encrypts sensitive fields before storage)
    pub async fn create(&self, instance: &ProviderInstance) -> Result<()> {
        self.ensure_encryption_for_sensitive_fields(instance)?;
        let encrypted_jwt_secret =
            self.encrypt_field(&instance.name, "jwt_secret", instance.jwt_secret.as_deref())?;
        let encrypted_custom_ca =
            self.encrypt_field(&instance.name, "custom_ca", instance.custom_ca.as_deref())?;
        let provider_codes = provider_type_codes(&instance.providers);
        let result = sqlx::query!(
            r"
            INSERT INTO media_provider_instances
            (name, endpoint, comment, jwt_secret, custom_ca, timeout, tls, insecure_tls, providers, enabled)
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            ",
            instance.name.as_str(),
            instance.endpoint.as_str(),
            instance.comment.as_deref(),
            encrypted_jwt_secret.as_deref(),
            encrypted_custom_ca.as_deref(),
            instance.timeout.as_str(),
            instance.tls,
            instance.insecure_tls,
            &provider_codes,
            instance.enabled,
        )
        .execute(self.primary_pool())
        .await;

        match result {
            Ok(_) => Ok(()),
            Err(sqlx::Error::Database(db_err)) if db_err.code().as_deref() == Some("23505") => {
                Err(crate::Error::AlreadyExists(format!(
                    "Provider instance '{}' already exists",
                    instance.name
                )))
            }
            Err(err) => Err(err.into()),
        }
    }

    /// Update an existing provider instance (encrypts sensitive fields before storage)
    pub async fn update(&self, instance: &ProviderInstance) -> Result<()> {
        self.ensure_encryption_for_sensitive_fields(instance)?;
        let encrypted_jwt_secret =
            self.encrypt_field(&instance.name, "jwt_secret", instance.jwt_secret.as_deref())?;
        let encrypted_custom_ca =
            self.encrypt_field(&instance.name, "custom_ca", instance.custom_ca.as_deref())?;
        let provider_codes = provider_type_codes(&instance.providers);

        let result = sqlx::query!(
            r"
            UPDATE media_provider_instances
            SET endpoint = $2, comment = $3, jwt_secret = $4, custom_ca = $5,
                timeout = $6, tls = $7, insecure_tls = $8, providers = $9, enabled = $10,
                updated_at = NOW()
            WHERE name = $1
            ",
            instance.name.as_str(),
            instance.endpoint.as_str(),
            instance.comment.as_deref(),
            encrypted_jwt_secret.as_deref(),
            encrypted_custom_ca.as_deref(),
            instance.timeout.as_str(),
            instance.tls,
            instance.insecure_tls,
            &provider_codes,
            instance.enabled,
        )
        .execute(self.primary_pool())
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{}' not found",
                instance.name
            )));
        }

        Ok(())
    }

    /// Delete a provider instance
    pub async fn delete(&self, name: &str) -> Result<()> {
        let result = sqlx::query!("DELETE FROM media_provider_instances WHERE name = $1", name,)
            .execute(self.primary_pool())
            .await;

        let result = match result {
            Ok(result) => result,
            Err(sqlx::Error::Database(db_err))
                if Self::is_provider_instance_reference_violation(db_err.as_ref()) =>
            {
                return Err(crate::Error::InvalidInput(format!(
                    "Provider instance '{name}' is still referenced by media or playlists"
                )));
            }
            Err(err) => return Err(err.into()),
        };

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }

    /// Enable a provider instance
    pub async fn enable(&self, name: &str) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE media_provider_instances SET enabled = true, updated_at = NOW() WHERE name = $1",
            name,
        )
            .execute(self.primary_pool())
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }

    /// Disable a provider instance
    pub async fn disable(&self, name: &str) -> Result<()> {
        let result = sqlx::query!(
            "UPDATE media_provider_instances SET enabled = false, updated_at = NOW() WHERE name = $1",
            name,
        )
            .execute(self.primary_pool())
            .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "Provider instance '{name}' not found"
            )));
        }

        Ok(())
    }
}

/// User Provider Credential Repository
///
/// Credentials are encrypted at rest using AES-256-GCM. Encryption is mandatory.
pub struct UserProviderCredentialRepository {
    pool: PgPool,
    encryption: Option<CredentialEncryption>,
}

impl std::fmt::Debug for UserProviderCredentialRepository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserProviderCredentialRepository")
            .field("pool", &"PgPool")
            .field("encryption", &self.encryption.is_some())
            .finish()
    }
}

impl UserProviderCredentialRepository {
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn encrypt_credential_with(
        encryption: Option<&CredentialEncryption>,
        user_id: UserId,
        provider: &str,
        server_id: &str,
        data: &ProviderCredential,
    ) -> Result<StoredProviderCredential> {
        let context = Self::credential_context(user_id, provider, server_id)?;
        StoredProviderCredential::encrypt_from_domain(encryption, data, &context)
    }

    fn decrypt_credential_with(
        encryption: Option<&CredentialEncryption>,
        user_id: UserId,
        provider: &str,
        server_id: &str,
        data: &StoredProviderCredential,
    ) -> Result<ProviderCredential> {
        let context = Self::credential_context(user_id, provider, server_id)?;
        data.decrypt_to_domain(encryption, &context)
    }

    fn credential_context(user_id: UserId, provider: &str, server_id: &str) -> Result<Vec<u8>> {
        let provider_code = provider_type_code(provider)?;
        Ok(
            format!("user-provider-credential:v1\0{user_id}\0{provider_code}\0{server_id}")
                .into_bytes(),
        )
    }

    /// Create a new repository without encryption
    #[must_use]
    pub const fn new(pool: PgPool) -> Self {
        Self {
            pool,
            encryption: None,
        }
    }

    /// Create a new repository with credential encryption enabled
    #[must_use]
    pub const fn new_with_encryption(pool: PgPool, encryption: CredentialEncryption) -> Self {
        Self {
            pool,
            encryption: Some(encryption),
        }
    }

    fn encrypt_credential(
        &self,
        credential: &UserProviderCredential,
    ) -> Result<StoredProviderCredential> {
        Self::encrypt_credential_with(
            self.encryption.as_ref(),
            credential.user_id,
            &credential.provider,
            &credential.server_id,
            &credential.credential_data,
        )
    }

    fn decrypt_credential(
        &self,
        user_id: UserId,
        provider: &str,
        server_id: &str,
        data: &StoredProviderCredential,
    ) -> Result<ProviderCredential> {
        Self::decrypt_credential_with(self.encryption.as_ref(), user_id, provider, server_id, data)
    }

    fn decrypt_credential_row(
        &self,
        row: UserProviderCredentialRow,
    ) -> Result<UserProviderCredential> {
        let provider =
            provider_type_name_from_code(row.provider).map_err(crate::Error::InvalidInput)?;
        let credential_data =
            self.decrypt_credential(row.user_id, &provider, &row.server_id, &row.credential_data)?;
        Ok(UserProviderCredential {
            id: row.id,
            user_id: row.user_id,
            provider,
            server_id: row.server_id,
            provider_instance_name: row.provider_instance_name,
            credential_data,
            expires_at: row.expires_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
    }

    /// Decrypt credentials on a list of `UserProviderCredentialRow`.
    fn decrypt_credential_rows(
        &self,
        rows: Vec<UserProviderCredentialRow>,
    ) -> Result<Vec<UserProviderCredential>> {
        rows.into_iter()
            .map(|row| self.decrypt_credential_row(row))
            .collect()
    }

    fn decrypt_readable_credential_rows(
        &self,
        rows: Vec<UserProviderCredentialRow>,
    ) -> Vec<UserProviderCredential> {
        rows.into_iter()
            .filter_map(|row| {
                let credential_id = row.id;
                let user_id = row.user_id;
                let provider = row.provider;
                let server_id = row.server_id.clone();

                match self.decrypt_credential_row(row) {
                    Ok(credential) => Some(credential),
                    Err(error) => {
                        tracing::warn!(
                            credential_id,
                            user_id = %user_id,
                            provider,
                            server_id = %server_id,
                            error = %error,
                            "Skipping unreadable user provider credential"
                        );
                        None
                    }
                }
            })
            .collect()
    }

    /// Get all credentials for a user (decrypted)
    pub async fn get_by_user(&self, user_id: UserId) -> Result<Vec<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1
            ORDER BY created_at DESC
            "#,
            user_id as UserId,
        )
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credential_rows(rows)
    }

    /// Get credential by ID (decrypted)
    pub async fn get_by_id(&self, id: i64) -> Result<Option<UserProviderCredential>> {
        let row = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE id = $1
            "#,
            id,
        )
        .fetch_optional(&self.pool)
        .await?;

        match row {
            Some(row) => Ok(Some(self.decrypt_credential_row(row)?)),
            None => Ok(None),
        }
    }

    /// Get user credential for a specific provider and server (decrypted)
    pub async fn get_by_provider_and_server(
        &self,
        user_id: UserId,
        provider: &str,
        server_id: &str,
    ) -> Result<Option<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2 AND server_id = $3
            ORDER BY created_at DESC
            "#,
            user_id as UserId,
            provider_type_code(provider)?,
            server_id,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(self
            .decrypt_readable_credential_rows(rows)
            .into_iter()
            .next())
    }

    /// Get all credentials for a specific provider type (decrypted)
    pub async fn get_by_provider(
        &self,
        user_id: UserId,
        provider: &str,
    ) -> Result<Vec<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2
            "#,
            user_id as UserId,
            provider_type_code(provider)?,
        )
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credential_rows(rows)
    }

    /// Get readable credentials for a specific provider type.
    ///
    /// This is intended for bind-list UI paths where one corrupt historical
    /// credential should not hide the user's current valid binding.
    pub async fn get_readable_by_provider(
        &self,
        user_id: UserId,
        provider: &str,
    ) -> Result<Vec<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE user_id = $1 AND provider = $2
            ORDER BY created_at DESC
            "#,
            user_id as UserId,
            provider_type_code(provider)?,
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(self.decrypt_readable_credential_rows(rows))
    }

    /// Create a new user credential (encrypts before storage)
    pub async fn create(
        &self,
        credential: &UserProviderCredential,
    ) -> Result<UserProviderCredential> {
        let stored_data = self.encrypt_credential(credential)?;
        let stored_json = sqlx::types::Json(&stored_data);
        let provider_code = provider_type_code(&credential.provider)?;

        let created = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            INSERT INTO user_media_provider_credentials
            (user_id, provider, server_id, provider_instance_name, credential_data, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, user_id as "user_id: UserId", provider, server_id,
                      provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                      expires_at, created_at, updated_at
            "#,
            credential.user_id as UserId,
            provider_code,
            credential.server_id.as_str(),
            normalize_provider_instance_name(credential.provider_instance_name.as_deref()),
            stored_json as _,
            credential.expires_at,
        )
        .fetch_one(&self.pool)
        .await?;

        self.decrypt_credential_row(created)
    }

    /// Insert or replace the credential for a `(user_id, provider, server_id)` binding.
    ///
    /// This is intentionally a repository-level primitive so provider login flows do not
    /// implement non-atomic delete-then-create upserts.
    pub async fn upsert_by_user_provider_server(
        &self,
        credential: &UserProviderCredential,
    ) -> Result<UserProviderCredential> {
        let stored_data = self.encrypt_credential(credential)?;
        let stored_json = sqlx::types::Json(&stored_data);
        let provider_code = provider_type_code(&credential.provider)?;

        let upserted = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            INSERT INTO user_media_provider_credentials
            (user_id, provider, server_id, provider_instance_name, credential_data, expires_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (user_id, provider, server_id)
            DO UPDATE SET
                provider_instance_name = EXCLUDED.provider_instance_name,
                credential_data = EXCLUDED.credential_data,
                expires_at = EXCLUDED.expires_at,
                updated_at = NOW()
            RETURNING id, user_id as "user_id: UserId", provider, server_id,
                      provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                      expires_at, created_at, updated_at
            "#,
            credential.user_id as UserId,
            provider_code,
            credential.server_id.as_str(),
            normalize_provider_instance_name(credential.provider_instance_name.as_deref()),
            stored_json as _,
            credential.expires_at,
        )
        .fetch_one(&self.pool)
        .await?;

        self.decrypt_credential_row(upserted)
    }

    /// Update an existing user credential (encrypts before storage)
    pub async fn update(&self, credential: &UserProviderCredential) -> Result<()> {
        let provider_code = provider_type_code(&credential.provider)?;
        let stored_data = self.encrypt_credential(credential)?;
        let stored_json = sqlx::types::Json(&stored_data);

        let result = sqlx::query!(
            r"
            UPDATE user_media_provider_credentials
            SET provider_instance_name = $2, credential_data = $3, expires_at = $4, updated_at = NOW()
            WHERE id = $1
              AND user_id = $5
              AND provider = $6
              AND server_id = $7
            ",
            credential.id,
            normalize_provider_instance_name(credential.provider_instance_name.as_deref()),
            stored_json as _,
            credential.expires_at,
            credential.user_id as UserId,
            provider_code,
            credential.server_id,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "User provider credential '{}' not found for the supplied binding",
                credential.id
            )));
        }

        Ok(())
    }

    /// Delete a user credential
    pub async fn delete(&self, id: i64) -> Result<()> {
        let result = sqlx::query!(
            "DELETE FROM user_media_provider_credentials WHERE id = $1",
            id,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "User provider credential '{id}' not found"
            )));
        }

        Ok(())
    }

    /// Delete all credentials for a user and provider
    pub async fn delete_by_user_and_provider(&self, user_id: UserId, provider: &str) -> Result<()> {
        let result = sqlx::query!(
            "DELETE FROM user_media_provider_credentials WHERE user_id = $1 AND provider = $2",
            user_id as UserId,
            provider_type_code(provider)?,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!(
                "No credentials found for user '{user_id}' and provider '{provider}'"
            )));
        }

        Ok(())
    }

    /// Get all expired credentials (for cleanup jobs, decrypted)
    pub async fn get_expired(&self) -> Result<Vec<UserProviderCredential>> {
        let rows = sqlx::query_as!(
            UserProviderCredentialRow,
            r#"
            SELECT id, user_id as "user_id: UserId", provider, server_id,
                   provider_instance_name, credential_data as "credential_data!: StoredProviderCredential",
                   expires_at, created_at, updated_at
            FROM user_media_provider_credentials
            WHERE expires_at IS NOT NULL AND expires_at <= NOW()
            "#,
        )
        .fetch_all(&self.pool)
        .await?;

        self.decrypt_credential_rows(rows)
    }

    /// Delete all expired credentials
    pub async fn delete_expired(&self) -> Result<u64> {
        let result = sqlx::query!(
            "DELETE FROM user_media_provider_credentials WHERE expires_at IS NOT NULL AND expires_at <= NOW()"
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }
}

#[cfg(test)]
#[path = "provider_instance_tests.rs"]
mod tests;
