use chrono::Duration;
use std::fmt;

/// Email token type stored in `auth_email_tokens.token_type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i16)]
pub enum EmailTokenType {
    EmailVerification = 1,
    PasswordReset = 2,
    EmailLogin = 3,
}

impl EmailTokenType {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::EmailVerification => "email_verification",
            Self::PasswordReset => "password_reset",
            Self::EmailLogin => "email_login",
        }
    }

    #[must_use]
    pub const fn expiration_duration(&self) -> Duration {
        match self {
            Self::EmailVerification => Duration::hours(24),
            Self::PasswordReset => Duration::hours(1),
            Self::EmailLogin => Duration::minutes(15),
        }
    }

    #[must_use]
    pub const fn keeps_multiple_unused_tokens(self) -> bool {
        matches!(self, Self::EmailLogin)
    }
}

impl fmt::Display for EmailTokenType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl From<EmailTokenType> for i16 {
    fn from(value: EmailTokenType) -> Self {
        value as Self
    }
}

impl TryFrom<i16> for EmailTokenType {
    type Error = String;

    fn try_from(value: i16) -> std::result::Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::EmailVerification),
            2 => Ok(Self::PasswordReset),
            3 => Ok(Self::EmailLogin),
            other => Err(format!("Invalid EmailTokenType value: {other}")),
        }
    }
}
