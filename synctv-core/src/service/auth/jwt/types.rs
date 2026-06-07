use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::{
    models::{RoomId, UserId},
    Error, Result,
};

/// JWT token type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenType {
    Access,
    Refresh,
    Guest,
}

impl TokenType {
    #[must_use]
    pub fn from_claim_typ(value: &str) -> Option<Self> {
        match value {
            "access" => Some(Self::Access),
            "refresh" => Some(Self::Refresh),
            "guest" => Some(Self::Guest),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UserTokenSigningKind {
    Access,
    Refresh,
}

impl UserTokenSigningKind {
    pub(super) const fn claim_typ(self) -> &'static str {
        match self {
            Self::Access => "access",
            Self::Refresh => "refresh",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAuthContext {
    LocalTwoFactor,
    OAuth2,
}

impl TokenAuthContext {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalTwoFactor => "local_2fa",
            Self::OAuth2 => "oauth2",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenCredentialBinding {
    Password {
        version: i32,
    },
    OAuth2 {
        provider_instance_name: String,
        provider_user_id: String,
    },
    WebAuthn {
        credential_id: Vec<u8>,
    },
    Email {
        email: String,
    },
}

/// JWT claims structure
///
/// Role and permissions are loaded from the database at request time so token
/// authorization reflects current account state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,
    pub typ: String,
    pub jti: String,
    pub iat: i64,
    pub exp: i64,
    pub pv: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub amr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cbm: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opi: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ops: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eml: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wcid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
}

impl Claims {
    pub fn user_id(&self) -> Result<UserId> {
        self.sub
            .parse()
            .map_err(|error| Error::Authentication(format!("Invalid user id claim: {error}")))
    }

    #[must_use]
    pub fn is_access_token(&self) -> bool {
        self.typ == "access"
    }

    #[must_use]
    pub fn is_refresh_token(&self) -> bool {
        self.typ == "refresh"
    }

    #[must_use]
    pub fn is_guest_token(&self) -> bool {
        self.typ == "guest"
    }

    #[must_use]
    pub fn satisfies_two_factor_requirement(&self) -> bool {
        matches!(self.amr.as_deref(), Some("local_2fa" | "oauth2"))
    }

    pub fn credential_binding(&self) -> Result<TokenCredentialBinding> {
        match self.cbm.as_deref() {
            Some("password") => Ok(TokenCredentialBinding::Password { version: self.pv }),
            Some("oauth2") => {
                Ok(TokenCredentialBinding::OAuth2 {
                    provider_instance_name: self.opi.clone().ok_or_else(|| {
                        Error::Authentication("Authentication failed".to_string())
                    })?,
                    provider_user_id: self.ops.clone().ok_or_else(|| {
                        Error::Authentication("Authentication failed".to_string())
                    })?,
                })
            }
            Some("webauthn") => {
                let credential_id = base64::engine::general_purpose::URL_SAFE_NO_PAD
                    .decode(self.wcid.as_deref().ok_or_else(|| {
                        Error::Authentication("Authentication failed".to_string())
                    })?)
                    .map_err(|_| Error::Authentication("Authentication failed".to_string()))?;
                Ok(TokenCredentialBinding::WebAuthn { credential_id })
            }
            Some("email") => Ok(TokenCredentialBinding::Email {
                email: self
                    .eml
                    .clone()
                    .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?,
            }),
            Some(_) | None => Err(Error::Authentication("Authentication failed".to_string())),
        }
    }
}

/// Guest token claims structure (stateless guest authentication)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestClaims {
    pub sub: String,
    pub room_id: String,
    pub session_id: String,
    pub jti: String,
    pub typ: String,
    pub iat: i64,
    pub exp: i64,
    #[serde(default)]
    pub gv: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
}

impl GuestClaims {
    pub fn room_id(&self) -> Result<RoomId> {
        self.room_id
            .parse()
            .map_err(|error| Error::Authentication(format!("Invalid room id claim: {error}")))
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    #[must_use]
    pub fn is_guest(&self) -> bool {
        self.sub.starts_with("guest:")
    }
}
