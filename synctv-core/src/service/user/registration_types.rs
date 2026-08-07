use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::Passkey;

use crate::{
    models::oauth2_client::OAuth2Provider,
    models::{FileMetadata, FileUploadManifestPart, SignupMethod, User, UserId},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegistrationMode {
    Password,
    Email,
    OAuth2,
    WebAuthn,
}

impl RegistrationMode {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "Password",
            Self::Email => "Email",
            Self::OAuth2 => "OAuth2",
            Self::WebAuthn => "WebAuthn",
        }
    }

    pub(super) const fn supports_review(self) -> bool {
        matches!(self, Self::Password | Self::Email | Self::WebAuthn)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegistrationPolicy {
    pub enabled: bool,
    pub need_review: bool,
}

#[derive(Debug, Clone)]
pub struct PendingAccountRegistration {
    pub review_request_id: UserId,
    pub username: String,
    pub email: Option<String>,
}

#[derive(Debug, Clone)]
pub enum AccountRegistrationOutcome {
    Registered {
        user: User,
        email: Option<String>,
        access_token: String,
        refresh_token: String,
    },
    PendingReview(PendingAccountRegistration),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateUserAvatarUploadSession {
    pub client_avatar_id: Option<String>,
    pub mime_type: String,
    pub size_bytes: i64,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub duration_seconds: Option<i32>,
    pub bitrate_bps: Option<i32>,
    pub parts: Vec<FileUploadManifestPart>,
    pub metadata: FileMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingRegistrationConflict {
    Username,
    Email,
    OAuth2Identity(UserId),
}

#[derive(Debug)]
pub(super) struct PendingRegistrationRequest {
    pub(super) username: String,
    pub(super) email: Option<String>,
    pub(super) opaque_record: Option<Vec<u8>>,
    pub(super) opaque_credential_identifier: Option<Vec<u8>>,
    pub(super) opaque_ciphersuite: Option<String>,
    pub(super) opaque_server_setup_version: Option<i32>,
    pub(super) oauth2_provider: Option<OAuth2Provider>,
    pub(super) oauth2_provider_instance_name: Option<String>,
    pub(super) oauth2_provider_issuer: Option<String>,
    pub(super) oauth2_provider_user_id: Option<String>,
    pub(super) oauth2_provider_username: Option<String>,
    pub(super) oauth2_avatar_url: Option<String>,
    pub(super) webauthn_credential_id: Option<Vec<u8>>,
    pub(super) webauthn_passkey: Option<Passkey>,
    pub(super) webauthn_credential_name: Option<String>,
    pub(super) signup_method: SignupMethod,
}

pub(super) struct PendingRegistrationRequestRow {
    pub(super) username: String,
    pub(super) email: Option<String>,
    pub(super) opaque_record: Option<Vec<u8>>,
    pub(super) opaque_credential_identifier: Option<Vec<u8>>,
    pub(super) opaque_ciphersuite: Option<String>,
    pub(super) opaque_server_setup_version: Option<i32>,
    pub(super) signup_method: SignupMethod,
    pub(super) oauth2_provider: Option<OAuth2Provider>,
    pub(super) oauth2_provider_instance_name: Option<String>,
    pub(super) oauth2_provider_issuer: Option<String>,
    pub(super) oauth2_provider_user_id: Option<String>,
    pub(super) oauth2_provider_username: Option<String>,
    pub(super) oauth2_avatar_url: Option<String>,
    pub(super) webauthn_credential_id: Option<Vec<u8>>,
    pub(super) webauthn_passkey: Option<PendingRegistrationPasskey>,
    pub(super) webauthn_credential_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct PendingRegistrationPasskey(Passkey);

impl PendingRegistrationPasskey {
    pub(crate) fn from_passkey(passkey: &Passkey) -> Self {
        Self(passkey.clone())
    }

    pub(crate) fn into_inner(self) -> Passkey {
        self.0
    }
}

impl sqlx::Type<sqlx::Postgres> for PendingRegistrationPasskey {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        <sqlx::types::Json<Self> as sqlx::Type<sqlx::Postgres>>::type_info()
    }

    fn compatible(ty: &sqlx::postgres::PgTypeInfo) -> bool {
        <sqlx::types::Json<Self> as sqlx::Type<sqlx::Postgres>>::compatible(ty)
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for PendingRegistrationPasskey {
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> std::result::Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        sqlx::types::Json(self).encode_by_ref(buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for PendingRegistrationPasskey {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> std::result::Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let sqlx::types::Json(value) =
            <sqlx::types::Json<Self> as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(value)
    }
}
