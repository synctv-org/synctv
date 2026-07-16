use serde::{Deserialize, Serialize};

use crate::{
    models::UserId,
    service::{TokenAuthContext, TokenCredentialBinding},
};

pub(super) const OPAQUE_LOGIN_SESSION_TTL_SECS: u64 = 300;
pub(super) const OPAQUE_LOGIN_SESSION_CAPACITY: u64 = 10_000;
pub(super) const OPAQUE_REGISTRATION_SESSION_TTL_SECS: u64 = 300;
pub(super) const OPAQUE_REGISTRATION_SESSION_CAPACITY: u64 = 10_000;
pub(super) const MFA_SESSION_TTL_SECS: u64 = 300;
pub(super) const MFA_SESSION_TTL_SECS_I64: i64 = 300;
pub(super) const MFA_SESSION_CAPACITY: u64 = 10_000;
pub(super) const SENSITIVE_VERIFICATION_SESSION_TTL_SECS: u64 = 300;
pub(super) const SENSITIVE_VERIFICATION_SESSION_TTL_SECS_I64: i64 = 300;
pub(super) const SENSITIVE_VERIFICATION_SESSION_CAPACITY: u64 = 10_000;
pub(super) const SENSITIVE_VERIFICATION_PASSWORD_BRUTE_FORCE_PREFIX: &str =
    "auth:sensitive:password";
pub(super) const TWO_FACTOR_REQUIRED_MESSAGE: &str =
    "Two-factor authentication is required before tokens can be issued";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaqueLoginSession {
    pub(crate) user_id: Option<UserId>,
    pub(crate) brute_force_key: String,
    pub(crate) user_existed: bool,
    pub(crate) server_login_state: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct OpaqueLoginStartChallenge {
    pub session_id: String,
    pub credential_response: bytes::Bytes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFactorMethod {
    Password,
    WebAuthn,
    Email,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfaSession {
    pub(crate) user_id: UserId,
    pub(crate) first_factor: AuthFactorMethod,
    pub(crate) credential_binding: TokenCredentialBinding,
    pub(crate) brute_force_key: String,
    pub(crate) expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MfaChallenge {
    pub session_id: String,
    pub available_methods: Vec<AuthFactorMethod>,
    pub masked_email: Option<String>,
    pub expires_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SensitiveVerificationSession {
    pub(crate) user_id: UserId,
    pub(crate) required_count: usize,
    pub(crate) completed_methods: Vec<AuthFactorMethod>,
    pub(crate) expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitiveVerificationChallenge {
    pub session_id: String,
    pub required_count: usize,
    pub required_methods: Vec<AuthFactorMethod>,
    pub completed_methods: Vec<AuthFactorMethod>,
    pub available_methods: Vec<AuthFactorMethod>,
    pub masked_email: Option<String>,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SensitiveVerificationOutcome {
    Pending(SensitiveVerificationChallenge),
    Complete { verification_id: String },
}

#[derive(Debug, Clone)]
pub enum AuthenticatedLogin {
    Complete {
        user: crate::models::User,
        email: Option<String>,
        access_token: String,
        refresh_token: String,
    },
    MfaRequired {
        user: crate::models::User,
        email: Option<String>,
        challenge: MfaChallenge,
    },
}

pub(super) struct TokenIssueContext<'a> {
    pub(super) auth_context: Option<TokenAuthContext>,
    pub(super) credential_binding: &'a TokenCredentialBinding,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OpaqueRegistrationPurpose {
    Account {
        username: String,
        email: Option<String>,
    },
    PasswordUpdate {
        user_id: UserId,
        expected_password_version: i32,
        verification: OpaquePasswordUpdateVerification,
    },
    PasswordReset {
        user_id: UserId,
        expected_password_version: i32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OpaquePasswordUpdateVerification {
    CurrentOpaquePassword { server_login_state: Vec<u8> },
    VerifiedExternal,
    PendingPasskey,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpaqueRegistrationSession {
    pub(crate) credential_identifier: Vec<u8>,
    pub(crate) purpose: OpaqueRegistrationPurpose,
}

#[derive(Debug, Clone)]
pub struct OpaqueRegistrationStartChallenge {
    pub session_id: String,
    pub credential_response: bytes::Bytes,
    pub registration_response: bytes::Bytes,
}
