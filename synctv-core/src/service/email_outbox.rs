use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2_010::{Digest, Sha256};
use sqlx::PgPool;

use crate::{
    credential_encryption::CredentialEncryption,
    models::{EmailTokenType, UserId},
    repository::{EmailOutboxJob, EmailOutboxKind, EmailOutboxRepository, NewEmailOutboxJob},
    Error, Result,
};

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
    pub fn new(pool: PgPool, encryption_key: &str) -> Result<Self> {
        let encryption =
            CredentialEncryption::from_hex_key_with_domain(encryption_key, "email-outbox-payload")?;
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
            EmailTokenType::EmailBind => {
                return Err(Error::Internal(
                    "Email bind delivery must use the dedicated bind outbox payload".to_string(),
                ));
            }
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
        let id = synctv_common::snanoid!(32);
        let recipient = recipient.trim().to_ascii_lowercase();
        let created_at = Utc::now();
        let expires_at = DateTime::from_timestamp_micros(expires_at.timestamp_micros())
            .ok_or_else(|| Error::Internal("Email outbox expiry is out of range".to_string()))?;
        let payload_value = serde_json::to_value(payload)
            .map_err(|error| Error::Internal(format!("Failed to encode email job: {error}")))?;
        let context = Self::payload_context(&id, kind, &recipient, expires_at);
        let encrypted_payload = self
            .encryption
            .encrypt_with_context(&payload_value, &context)?;
        let mut digest = Sha256::new();
        digest.update(kind.as_str().as_bytes());
        digest.update([0]);
        digest.update(recipient.as_bytes());
        digest.update([0]);
        digest.update(
            serde_json::to_vec(payload)
                .map_err(|error| Error::Internal(format!("Failed to hash email job: {error}")))?,
        );
        Ok(NewEmailOutboxJob {
            id,
            kind,
            recipient,
            encrypted_payload,
            dedupe_key: hex::encode(digest.finalize()),
            attempts: 0,
            next_attempt_at: created_at,
            lock_version: 0,
            expires_at,
            created_at,
        })
    }

    fn payload_context(
        id: &str,
        kind: EmailOutboxKind,
        recipient: &str,
        expires_at: DateTime<Utc>,
    ) -> Vec<u8> {
        format!(
            "email-outbox:v1\0{id}\0{}\0{}\0{}",
            kind.as_str(),
            recipient.trim().to_ascii_lowercase(),
            expires_at.timestamp_micros()
        )
        .into_bytes()
    }

    pub fn decrypt_payload(&self, job: &EmailOutboxJob) -> Result<EmailOutboxPayload> {
        let context = Self::payload_context(&job.id, job.kind, &job.recipient, job.expires_at);
        let value = self
            .encryption
            .decrypt_with_context(&job.encrypted_payload, &context)?;
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

    const TEST_OUTBOX_KEY: &str =
        "4242424242424242424242424242424242424242424242424242424242424242";
    const OTHER_OUTBOX_KEY: &str =
        "4343434343434343434343434343434343434343434343434343434343434343";

    #[tokio::test]
    async fn payload_is_encrypted_and_key_scoped() {
        let pool = PgPool::connect_lazy("postgres://localhost/synctv")
            .expect("test pool URL should parse");
        let service = EmailOutboxService::new(pool.clone(), TEST_OUTBOX_KEY)
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
        let claimed = EmailOutboxJob {
            id: job.id.clone(),
            kind: job.kind,
            recipient: job.recipient.clone(),
            encrypted_payload: job.encrypted_payload.clone(),
            status: crate::repository::EmailOutboxStatus::Processing,
            attempts: 1,
            lock_version: 1,
            expires_at: job.expires_at,
            created_at: Utc::now(),
        };
        assert!(service.decrypt_payload(&claimed).is_ok());

        let other = EmailOutboxService::new(pool, OTHER_OUTBOX_KEY)
            .expect("second outbox service should initialize");
        assert!(other.decrypt_payload(&claimed).is_err());

        let mut transplanted = claimed.clone();
        transplanted.recipient = "attacker@example.com".to_string();
        assert!(service.decrypt_payload(&transplanted).is_err());
    }

    #[test]
    fn message_id_is_stable() {
        assert_eq!(
            EmailOutboxService::message_id("job-1"),
            "<email-outbox-job-1@synctv.local>"
        );
    }

    #[tokio::test]
    async fn generic_token_payload_rejects_email_bind() {
        let pool = PgPool::connect_lazy("postgres://localhost/synctv")
            .expect("test pool URL should parse");
        let service = EmailOutboxService::new(pool, TEST_OUTBOX_KEY)
            .expect("outbox service should initialize");

        let error = service
            .prepare_token(
                "user@example.com",
                "bind-token",
                &UserId::new(),
                EmailTokenType::EmailBind,
                Utc::now() + Duration::hours(24),
            )
            .expect_err("generic payload must reject email bind");

        assert!(error.to_string().contains("dedicated bind outbox payload"));
    }
}
