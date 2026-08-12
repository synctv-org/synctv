use base64::Engine as _;
use serde::{de::Error as _, Deserialize, Deserializer, Serialize, Serializer};

use crate::{
    models::{RoomId, UserId},
    Error, Result,
};

/// JWT token type
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenType {
    Access,
    Refresh,
    Guest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenAuthContext {
    #[serde(rename = "local_2fa")]
    LocalTwoFactor,
    #[serde(rename = "oauth2")]
    OAuth2,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UserSubject(UserId);

impl UserSubject {
    pub(super) const fn new(user_id: UserId) -> Self {
        Self(user_id)
    }
}

impl Serialize for UserSubject {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0.to_string())
    }
}

impl<'de> Deserialize<'de> for UserSubject {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        String::deserialize(deserializer)?
            .parse()
            .map(Self)
            .map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TokenSessionId(String);

impl TokenSessionId {
    pub(super) fn new(value: &str) -> Result<Self> {
        if value.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Token session id must be non-empty".to_string(),
            ));
        }
        Ok(Self(value.to_string()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TokenId(String);

impl TokenId {
    pub(super) fn new(value: String) -> Result<Self> {
        if value.trim().is_empty() {
            return Err(Error::InvalidInput(
                "Token id must be non-empty".to_string(),
            ));
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for TokenId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TokenId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl Serialize for TokenSessionId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for TokenSessionId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(&value).map_err(D::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct WebAuthnCredentialId(Vec<u8>);

impl Serialize for WebAuthnCredentialId {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for WebAuthnCredentialId {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(encoded)
            .map(Self)
            .map_err(|_| D::Error::custom("invalid WebAuthn credential id"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cbm", rename_all = "snake_case")]
pub(super) enum TokenCredentialBindingClaims {
    Password,
    #[serde(rename = "oauth2")]
    OAuth2 {
        #[serde(rename = "opi")]
        provider_instance_name: String,
        #[serde(rename = "ops")]
        provider_user_id: String,
    },
    WebAuthn {
        #[serde(rename = "wcid")]
        credential_id: WebAuthnCredentialId,
    },
    Email {
        #[serde(rename = "eml")]
        email: String,
    },
}

impl From<&TokenCredentialBinding> for TokenCredentialBindingClaims {
    fn from(binding: &TokenCredentialBinding) -> Self {
        match binding {
            TokenCredentialBinding::Password { .. } => Self::Password,
            TokenCredentialBinding::OAuth2 {
                provider_instance_name,
                provider_user_id,
            } => Self::OAuth2 {
                provider_instance_name: provider_instance_name.clone(),
                provider_user_id: provider_user_id.clone(),
            },
            TokenCredentialBinding::WebAuthn { credential_id } => Self::WebAuthn {
                credential_id: WebAuthnCredentialId(credential_id.clone()),
            },
            TokenCredentialBinding::Email { email } => Self::Email {
                email: email.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "typ", rename_all = "snake_case")]
pub(super) enum UserTokenContext {
    Access {
        #[serde(rename = "sid", skip_serializing_if = "Option::is_none")]
        session_id: Option<TokenSessionId>,
    },
    Refresh {
        #[serde(rename = "sid")]
        session_id: TokenSessionId,
    },
}

/// JWT claims structure
///
/// Role and permissions are loaded from the database at request time so token
/// authorization reflects current account state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    #[serde(rename = "sub")]
    pub(super) subject: UserSubject,
    #[serde(flatten)]
    pub(super) token: UserTokenContext,
    #[serde(rename = "jti")]
    pub(super) token_id: TokenId,
    pub iat: i64,
    pub exp: i64,
    pub pv: i32,
    #[serde(rename = "amr", skip_serializing_if = "Option::is_none")]
    pub(super) auth_context: Option<TokenAuthContext>,
    #[serde(flatten)]
    pub(super) credential_binding: TokenCredentialBindingClaims,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
}

impl Claims {
    #[must_use]
    pub const fn user_id(&self) -> UserId {
        self.subject.0
    }

    #[must_use]
    pub const fn token_type(&self) -> TokenType {
        match &self.token {
            UserTokenContext::Access { .. } => TokenType::Access,
            UserTokenContext::Refresh { .. } => TokenType::Refresh,
        }
    }

    #[must_use]
    pub fn session_id(&self) -> Option<&str> {
        match &self.token {
            UserTokenContext::Access { session_id } => {
                session_id.as_ref().map(TokenSessionId::as_str)
            }
            UserTokenContext::Refresh { session_id } => Some(session_id.as_str()),
        }
    }

    #[must_use]
    pub fn token_id(&self) -> &str {
        self.token_id.as_str()
    }

    #[must_use]
    pub const fn auth_context(&self) -> Option<TokenAuthContext> {
        self.auth_context
    }

    #[must_use]
    pub fn satisfies_two_factor_requirement(&self) -> bool {
        self.auth_context.is_some()
    }

    pub fn credential_binding(&self) -> TokenCredentialBinding {
        match &self.credential_binding {
            TokenCredentialBindingClaims::Password => {
                TokenCredentialBinding::Password { version: self.pv }
            }
            TokenCredentialBindingClaims::OAuth2 {
                provider_instance_name,
                provider_user_id,
            } => TokenCredentialBinding::OAuth2 {
                provider_instance_name: provider_instance_name.clone(),
                provider_user_id: provider_user_id.clone(),
            },
            TokenCredentialBindingClaims::WebAuthn { credential_id } => {
                TokenCredentialBinding::WebAuthn {
                    credential_id: credential_id.0.clone(),
                }
            }
            TokenCredentialBindingClaims::Email { email } => TokenCredentialBinding::Email {
                email: email.clone(),
            },
        }
    }

    #[cfg(test)]
    pub(crate) fn test_access(user_id: UserId, password_version: i32, iat: i64, exp: i64) -> Self {
        Self {
            subject: UserSubject::new(user_id),
            token: UserTokenContext::Access { session_id: None },
            token_id: TokenId("test-jti".to_string()),
            iat,
            exp,
            pv: password_version,
            auth_context: None,
            credential_binding: TokenCredentialBindingClaims::Password,
            iss: None,
            aud: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "typ", rename_all = "snake_case")]
pub(super) enum GuestTokenContext {
    Guest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GuestSubject {
    room_id: RoomId,
    session_id: TokenSessionId,
}

impl GuestSubject {
    pub(super) fn new(room_id: RoomId, session_id: &str) -> Result<Self> {
        Ok(Self {
            room_id,
            session_id: TokenSessionId::new(session_id)?,
        })
    }
}

impl Serialize for GuestSubject {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&format!(
            "guest:{}:{}",
            self.room_id,
            self.session_id.as_str()
        ))
    }
}

impl<'de> Deserialize<'de> for GuestSubject {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let Some((room_id, session_id)) = value
            .strip_prefix("guest:")
            .and_then(|value| value.split_once(':'))
        else {
            return Err(D::Error::custom("invalid guest token subject"));
        };
        if session_id.contains(':') {
            return Err(D::Error::custom("invalid guest token subject"));
        }
        Ok(Self {
            room_id: room_id.parse().map_err(D::Error::custom)?,
            session_id: TokenSessionId::new(session_id).map_err(D::Error::custom)?,
        })
    }
}

/// Guest token claims structure (stateless guest authentication)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestClaims {
    #[serde(rename = "sub")]
    pub(super) subject: GuestSubject,
    #[serde(flatten)]
    pub(super) token: GuestTokenContext,
    #[serde(rename = "jti")]
    pub(super) token_id: TokenId,
    pub iat: i64,
    pub exp: i64,
    pub gv: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iss: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aud: Option<String>,
}

impl GuestClaims {
    #[must_use]
    pub const fn room_id(&self) -> RoomId {
        self.subject.room_id
    }

    #[must_use]
    pub fn session_id(&self) -> &str {
        self.subject.session_id.as_str()
    }

    #[must_use]
    pub fn token_id(&self) -> &str {
        self.token_id.as_str()
    }
}
