use serde::{Deserialize, Serialize};
use webauthn_rs::prelude::Passkey;

use crate::{
    models::oauth2_client::OAuth2Provider,
    models::{SignupMethod, User, UserId},
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
    pub checksum_sha256: Option<String>,
    pub metadata: serde_json::Value,
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
    pub(super) oauth2_email_trusted: bool,
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
    pub(super) oauth2_provider: Option<crate::models::OAuth2ProviderTypeName>,
    pub(super) oauth2_provider_instance_name: Option<String>,
    pub(super) oauth2_provider_issuer: Option<String>,
    pub(super) oauth2_provider_user_id: Option<String>,
    pub(super) oauth2_provider_username: Option<String>,
    pub(super) oauth2_avatar_url: Option<String>,
    pub(super) oauth2_email_trusted: Option<bool>,
    pub(super) webauthn_credential_id: Option<Vec<u8>>,
    pub(super) webauthn_passkey: Option<serde_json::Value>,
    pub(super) webauthn_credential_name: Option<String>,
}
