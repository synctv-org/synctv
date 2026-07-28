use chrono::{DateTime, Utc};
use hkdf::Hkdf;
use serde::{Deserialize, Serialize};
use sha2_010::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    credential_encryption::CredentialEncryption,
    models::{EmailTokenType, UserId},
    repository::{EmailOutboxKind, EmailOutboxRepository, NewEmailOutboxJob},
    Error, Result,
};

const KEY_CONTEXT: &[u8] = b"synctv/email-outbox/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EmailOutboxPayload {
    Token {
        version: u8,
        token: String,
        user_id: UserId,
        token_type: i16,
    },
    Registration {
        version: u8,
        token: String,
    },
    Bind {
        version: u8,
        token: String,
        user_id: UserId,
        email: String,
    },
}

impl EmailOutboxPayload {
    #[must_use]
    pub const fn version(&self) -> u8 {
        match self {
            Self::Token { version, .. }
            | Self::Registration { version, .. }
            | Self::Bind { version, .. } => *version,
        }
    }
}

#[derive(Clone)]
pub struct EmailOutboxService {
    repository: EmailOutboxRepository,
    encryption: CredentialEncryption,
}

impl std::fmt::Debug for EmailOutboxService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmailOutboxService").finish_non_exhaustive()
    }
}

impl EmailOutboxService {
    pub fn new(pool: PgPool, shared_secret: &[u8]) -> Result<Self> {
        if shared_secret.is_empty() {
            return Err(Error::Internal(
                "Email outbox encryption requires a shared secret".to_string(),
            ));
        }
        let mut key = [0_u8; 32];
        Hkdf::<Sha256>::new(None, shared_secret)
            .expand(KEY_CONTEXT, &mut key)
            .map_err(|_| Error::Internal("Failed to derive email outbox key".to_string()))?;
        let encryption = CredentialEncryption::new(&key)?;
        key.fill(0);
        Ok(Self {
            repository: EmailOutboxRepository::new(pool),
            encryption,
        })
    }

    #[must_use]
    pub const fn repository(&self) -> &EmailOutboxRepository {
        &self.repository
    }

    pub fn prepare_token(
        &self,
        recipient: &str,
        token: &str,
        user_id: &UserId,
        token_type: EmailTokenType,
        expires_at: DateTime<Utc>,
    ) -> Result<NewEmailOutboxJob> {
        let kind = match token_type {
            EmailTokenType::PasswordReset => EmailOutboxKind::PasswordReset,
            EmailTokenType::EmailLogin => EmailOutboxKind::EmailLogin,
            EmailTokenType::EmailBind => EmailOutboxKind::EmailBind,
        };
        self.prepare(
            kind,
            recipient,
            &EmailOutboxPayload::Token {
                version: 1,
                token: token.to_string(),
                user_id: *user_id,
                token_type: i16::from(token_type),
            },
            expires_at,
        )
    }

    pub fn prepare_registration(
        &self,
        recipient: &str,
        token: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<NewEmailOutboxJob> {
        self.prepare(
            EmailOutboxKind::EmailRegistration,
            recipient,
            &EmailOutboxPayload::Registration {
                version: 1,
                token: token.to_string(),
            },
            expires_at,
        )
    }

    pub fn prepare_bind(
        &self,
        recipient: &str,
        token: &str,
        user_id: &UserId,
        expires_at: DateTime<Utc>,
    ) -> Result<NewEmailOutboxJob> {
        self.prepare(
            EmailOutboxKind::EmailBind,
            recipient,
            &EmailOutboxPayload::Bind {
                version: 1,
                token: token.to_string(),
                user_id: *user_id,
                email: recipient.to_string(),
            },
            expires_at,
        )
    }

    fn prepare(
        &self,
        kind: EmailOutboxKind,
        recipient: &str,
        payload: &EmailOutboxPayload,
        expires_at: DateTime<Utc>,
    ) -> Result<NewEmailOutboxJob> {
        let payload_value = serde_json::to_value(payload)
            .map_err(|error| Error::Internal(format!("Failed to encode email job: {error}")))?;
        let encrypted_payload = self.encryption.encrypt(&payload_value)?;
        let mut digest = Sha256::new();
        digest.update(kind.as_str().as_bytes());
        digest.update([0]);
        digest.update(recipient.trim().to_ascii_lowercase().as_bytes());
        digest.update([0]);
        digest.update(
            serde_json::to_vec(payload)
                .map_err(|error| Error::Internal(format!("Failed to hash email job: {error}")))?,
        );
        Ok(NewEmailOutboxJob {
            id: synctv_common::snanoid!(32),
            kind,
            recipient: recipient.trim().to_ascii_lowercase(),
            encrypted_payload,
            dedupe_key: hex::encode(digest.finalize()),
            expires_at,
        })
    }

    pub fn decrypt_payload(&self, encrypted_payload: &str) -> Result<EmailOutboxPayload> {
        let value = self.encryption.decrypt(encrypted_payload)?;
        let payload: EmailOutboxPayload = serde_json::from_value(value)
            .map_err(|error| Error::Internal(format!("Invalid email outbox payload: {error}")))?;
        if payload.version() != 1 {
            return Err(Error::Internal(format!(
                "Unsupported email outbox payload version: {}",
                payload.version()
            )));
        }
        Ok(payload)
    }

    #[must_use]
    pub fn message_id(job_id: &str) -> String {
        format!("<email-outbox-{job_id}@synctv.local>")
    }
}

#[cfg(test)]
mod tests {
    use chrono::Duration;

    use super::*;

    #[tokio::test]
    async fn payload_is_encrypted_and_key_scoped() {
        let pool = PgPool::connect_lazy("postgres://localhost/synctv")
            .expect("test pool URL should parse");
        let service = EmailOutboxService::new(pool.clone(), b"shared-secret")
            .expect("outbox service should initialize");
        let token = "raw-secret-token";
        let job = service
            .prepare_token(
                "USER@example.com",
                token,
                &UserId::new(),
                EmailTokenType::EmailLogin,
                Utc::now() + Duration::minutes(15),
            )
            .expect("job should be prepared");

        assert!(!job.encrypted_payload.contains(token));
        assert_eq!(job.recipient, "user@example.com");
        assert!(service.decrypt_payload(&job.encrypted_payload).is_ok());

        let other = EmailOutboxService::new(pool, b"other-secret")
            .expect("second outbox service should initialize");
        assert!(other.decrypt_payload(&job.encrypted_payload).is_err());
    }

    #[test]
    fn message_id_is_stable() {
        assert_eq!(
            EmailOutboxService::message_id("job-1"),
            "<email-outbox-job-1@synctv.local>"
        );
    }
}
