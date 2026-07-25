use std::{net::IpAddr, time::Duration};

use hmac::{Hmac, KeyInit, Mac};
use rand::Rng;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use synctv_common::ExecutionControl;

use crate::{
    models::UserId,
    service::{AuthFactorMethod, AuthenticatedLogin, SensitiveVerificationOutcome, UserService},
    Error, Result,
};

const TOTP_PERIOD_SECONDS: i64 = 30;
const TOTP_DIGITS: u32 = 6;
const TOTP_SETUP_TTL: Duration = Duration::from_secs(600);
const RECOVERY_CODE_COUNT: usize = 10;
const RECOVERY_CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const TOTP_BRUTE_FORCE_PREFIX: &str = "auth:totp";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpSetup {
    pub setup_id: String,
    pub secret: String,
    pub otpauth_uri: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpRecoveryCodes {
    pub recovery_codes: Vec<String>,
}

impl UserService {
    pub async fn start_totp_setup(
        &self,
        user_id: &UserId,
        verification_id: &str,
    ) -> Result<TotpSetup> {
        if self
            .totp_credential_repository
            .get(user_id)
            .await?
            .is_some_and(|credential| credential.confirmed_at.is_some())
        {
            return Err(Error::AlreadyExists(
                "An authenticator app is already configured".to_string(),
            ));
        }
        let encryption = self.credential_encryption.as_ref().ok_or_else(|| {
            Error::ServiceUnavailable(
                "TOTP requires security.credential_encryption_key".to_string(),
            )
        })?;
        let user = self.get_user(user_id).await?;
        let mut secret_bytes = [0_u8; 20];
        rand::rng().fill_bytes(&mut secret_bytes);
        let secret = base32_encode(&secret_bytes);
        let encrypted_secret = encryption.encrypt_to_value(&serde_json::json!({
            "secret": secret,
        }))?;
        let setup_id = synctv_common::snanoid!(48);
        let now = crate::SystemClock.now();
        let expires_at = now
            + chrono::Duration::from_std(TOTP_SETUP_TTL)
                .map_err(|error| Error::Internal(error.to_string()))?;
        let mut uri = url::Url::parse(&format!(
            "otpauth://totp/{}",
            percent_encoding::utf8_percent_encode(
                &format!("SyncTV:{}", user.username),
                percent_encoding::NON_ALPHANUMERIC,
            )
        ))
        .map_err(|error| Error::Internal(format!("Failed to build TOTP URI: {error}")))?;
        uri.query_pairs_mut()
            .append_pair("secret", &secret)
            .append_pair("issuer", "SyncTV")
            .append_pair("algorithm", "SHA1")
            .append_pair("digits", "6")
            .append_pair("period", "30");
        self.consume_sensitive_operation_verification(user_id, verification_id)
            .await?;
        if !self
            .totp_credential_repository
            .start_setup(user_id, &encrypted_secret, &setup_id, expires_at)
            .await?
        {
            return Err(Error::AlreadyExists(
                "An authenticator app is already configured".to_string(),
            ));
        }
        Ok(TotpSetup {
            setup_id,
            secret,
            otpauth_uri: uri.to_string(),
            expires_at: expires_at.timestamp(),
        })
    }

    pub async fn finish_totp_setup(
        &self,
        user_id: &UserId,
        setup_id: &str,
        code: &str,
    ) -> Result<TotpRecoveryCodes> {
        let now = crate::SystemClock.now();
        let credential = self
            .totp_credential_repository
            .get_pending(user_id, setup_id, now)
            .await?
            .ok_or_else(authentication_failed)?;
        let secret = self.decrypt_totp_secret(&credential.encrypted_secret)?;
        let accepted_step = verify_totp_at(&secret, code, now.timestamp(), None)
            .ok_or_else(authentication_failed)?;
        let recovery_codes = generate_recovery_codes();
        let hashes = recovery_codes
            .iter()
            .map(|code| recovery_code_hash(code))
            .collect::<Vec<_>>();
        if !self
            .totp_credential_repository
            .confirm(user_id, setup_id, accepted_step, &hashes, now)
            .await?
        {
            return Err(authentication_failed());
        }
        Ok(TotpRecoveryCodes { recovery_codes })
    }

    pub async fn complete_mfa_totp_with_control(
        &self,
        session_id: &str,
        code: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let user = self
            .get_mfa_session_user_for_method(session_id, AuthFactorMethod::Totp)
            .await?;
        self.verify_totp_with_control(&user.id, code, client_ip, control)
            .await?;
        self.complete_mfa_session_with_control(
            session_id,
            AuthFactorMethod::Totp,
            client_ip,
            control,
        )
        .await
    }

    pub async fn complete_mfa_recovery_code_with_control(
        &self,
        session_id: &str,
        code: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<AuthenticatedLogin> {
        let user = self
            .get_mfa_session_user_for_method(session_id, AuthFactorMethod::RecoveryCode)
            .await?;
        let code_hash = self
            .validate_recovery_code_with_control(&user.id, code, client_ip, control)
            .await?;
        let login = self
            .complete_mfa_session_with_control(
                session_id,
                AuthFactorMethod::RecoveryCode,
                client_ip,
                control,
            )
            .await?;
        self.consume_validated_recovery_code(&user.id, &code_hash)
            .await?;
        Ok(login)
    }

    pub async fn finish_sensitive_operation_totp_verification(
        &self,
        session_id: &str,
        code: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<SensitiveVerificationOutcome> {
        let user = self
            .get_sensitive_operation_user_for_method(session_id, AuthFactorMethod::Totp)
            .await?;
        self.verify_totp_with_control(&user.id, code, client_ip, control)
            .await?;
        self.finish_sensitive_operation_verified_method(session_id, AuthFactorMethod::Totp)
            .await
    }

    pub async fn finish_sensitive_operation_recovery_code_verification(
        &self,
        session_id: &str,
        code: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<SensitiveVerificationOutcome> {
        let user = self
            .get_sensitive_operation_user_for_method(session_id, AuthFactorMethod::RecoveryCode)
            .await?;
        let code_hash = self
            .validate_recovery_code_with_control(&user.id, code, client_ip, control)
            .await?;
        let outcome = self
            .finish_sensitive_operation_verified_method(session_id, AuthFactorMethod::RecoveryCode)
            .await?;
        self.consume_validated_recovery_code(&user.id, &code_hash)
            .await?;
        Ok(outcome)
    }

    async fn verify_totp_with_control(
        &self,
        user_id: &UserId,
        code: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        let brute_force_key = format!("{TOTP_BRUTE_FORCE_PREFIX}:{}", user_id.as_i64());
        self.brute_force
            .check_subject_key_allowed_with_control(&brute_force_key, client_ip, control)
            .await?;
        let credential = self
            .totp_credential_repository
            .get(user_id)
            .await?
            .filter(|credential| credential.confirmed_at.is_some())
            .ok_or_else(authentication_failed)?;
        let secret = self.decrypt_totp_secret(&credential.encrypted_secret)?;
        let accepted_step = verify_totp_at(
            &secret,
            code,
            crate::SystemClock.now().timestamp(),
            credential.last_used_step,
        );
        let accepted = if let Some(step) = accepted_step {
            self.totp_credential_repository
                .advance_step(user_id, step)
                .await?
        } else {
            false
        };
        self.finish_totp_attempt(accepted, user_id, &brute_force_key, client_ip, control)
            .await
    }

    async fn validate_recovery_code_with_control(
        &self,
        user_id: &UserId,
        code: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<Vec<u8>> {
        let brute_force_key = format!("{TOTP_BRUTE_FORCE_PREFIX}:{}", user_id.as_i64());
        self.brute_force
            .check_subject_key_allowed_with_control(&brute_force_key, client_ip, control)
            .await?;
        let normalized = normalize_recovery_code(code).ok_or_else(authentication_failed)?;
        let code_hash = recovery_code_hash(&normalized);
        let accepted = self
            .totp_credential_repository
            .get(user_id)
            .await?
            .filter(|credential| credential.confirmed_at.is_some())
            .is_some_and(|credential| credential.recovery_code_hashes.contains(&code_hash));
        self.finish_totp_attempt(accepted, user_id, &brute_force_key, client_ip, control)
            .await?;
        Ok(code_hash)
    }

    async fn consume_validated_recovery_code(
        &self,
        user_id: &UserId,
        code_hash: &[u8],
    ) -> Result<()> {
        if self
            .totp_credential_repository
            .consume_recovery_code(user_id, code_hash)
            .await?
        {
            return Ok(());
        }
        Err(authentication_failed())
    }

    async fn finish_totp_attempt(
        &self,
        accepted: bool,
        user_id: &UserId,
        brute_force_key: &str,
        client_ip: Option<IpAddr>,
        control: Option<&ExecutionControl>,
    ) -> Result<()> {
        if accepted {
            self.brute_force
                .reset_subject_key_with_control(brute_force_key, control)
                .await?;
            return Ok(());
        }
        if let Err(error) = self
            .brute_force
            .record_subject_key_failure_with_control(brute_force_key, client_ip, control)
            .await
        {
            tracing::warn!(error = %error, %user_id, "Failed to record TOTP verification failure");
        }
        Err(authentication_failed())
    }

    fn decrypt_totp_secret(&self, encrypted: &serde_json::Value) -> Result<String> {
        let value = self
            .credential_encryption
            .as_ref()
            .ok_or_else(|| Error::ServiceUnavailable("TOTP encryption is unavailable".to_string()))?
            .decrypt_value(encrypted)?;
        value
            .get("secret")
            .and_then(serde_json::Value::as_str)
            .filter(|secret| !secret.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| Error::Internal("Encrypted TOTP secret is invalid".to_string()))
    }

    pub async fn regenerate_totp_recovery_codes(
        &self,
        user_id: &UserId,
        verification_id: &str,
    ) -> Result<TotpRecoveryCodes> {
        if self
            .totp_credential_repository
            .get(user_id)
            .await?
            .is_none_or(|credential| credential.confirmed_at.is_none())
        {
            return Err(Error::NotFound(
                "Authenticator app is not configured".to_string(),
            ));
        }
        self.consume_sensitive_operation_verification(user_id, verification_id)
            .await?;
        let recovery_codes = generate_recovery_codes();
        let hashes = recovery_codes
            .iter()
            .map(|code| recovery_code_hash(code))
            .collect::<Vec<_>>();
        if !self
            .totp_credential_repository
            .replace_recovery_codes(user_id, &hashes)
            .await?
        {
            return Err(Error::NotFound(
                "Authenticator app is not configured".to_string(),
            ));
        }
        Ok(TotpRecoveryCodes { recovery_codes })
    }

    pub async fn delete_totp(&self, user_id: &UserId, verification_id: &str) -> Result<bool> {
        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        self.repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))?;
        let mut remaining = self
            .user_preferences_repository
            .auth_factors_with_excluded_passkey(user_id, None, &mut *tx)
            .await?;
        if !remaining.totp {
            tx.commit().await?;
            return Ok(false);
        }
        remaining.totp = false;
        remaining.totp_recovery_codes_remaining = 0;
        if self
            .user_preferences_repository
            .two_factor_enabled_with_executor(user_id, &mut *tx)
            .await?
            && !remaining.supports_two_factor()
        {
            return Err(Error::InvalidInput(
                "Disable two-factor authentication before removing this authenticator app"
                    .to_string(),
            ));
        }
        self.consume_sensitive_operation_verification(user_id, verification_id)
            .await?;
        let deleted = self
            .totp_credential_repository
            .delete_with_executor(user_id, &mut *tx)
            .await?;
        tx.commit().await?;
        Ok(deleted)
    }
}

fn authentication_failed() -> Error {
    Error::Authentication("Authentication failed".to_string())
}

fn verify_totp_at(
    secret: &str,
    code: &str,
    timestamp: i64,
    last_used_step: Option<i64>,
) -> Option<i64> {
    let code = code.trim();
    if code.len() != TOTP_DIGITS as usize || !code.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let key = base32_decode(secret)?;
    let current_step = timestamp.max(0) / TOTP_PERIOD_SECONDS;
    [current_step, current_step - 1, current_step + 1]
        .into_iter()
        .filter(|step| *step >= 0 && last_used_step.is_none_or(|last| *step > last))
        .find(|step| totp_code(&key, *step).is_some_and(|expected| expected == code))
}

fn totp_code(key: &[u8], step: i64) -> Option<String> {
    let mut mac = Hmac::<Sha1>::new_from_slice(key).ok()?;
    mac.update(&u64::try_from(step).ok()?.to_be_bytes());
    let digest = mac.finalize().into_bytes();
    let offset = usize::from(digest[digest.len() - 1] & 0x0f);
    let binary = ((u32::from(digest[offset]) & 0x7f) << 24)
        | (u32::from(digest[offset + 1]) << 16)
        | (u32::from(digest[offset + 2]) << 8)
        | u32::from(digest[offset + 3]);
    Some(format!("{:06}", binary % 10_u32.pow(TOTP_DIGITS)))
}

fn base32_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut output = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            output.push(char::from(ALPHABET[((buffer >> bits) & 0x1f) as usize]));
        }
    }
    if bits > 0 {
        output.push(char::from(
            ALPHABET[((buffer << (5 - bits)) & 0x1f) as usize],
        ));
    }
    output
}

fn base32_decode(value: &str) -> Option<Vec<u8>> {
    let mut output = Vec::with_capacity(value.len() * 5 / 8);
    let mut buffer = 0_u32;
    let mut bits = 0_u8;
    for byte in value.bytes().filter(|byte| *byte != b'=') {
        let value = match byte.to_ascii_uppercase() {
            b'A'..=b'Z' => byte.to_ascii_uppercase() - b'A',
            b'2'..=b'7' => byte - b'2' + 26,
            _ => return None,
        };
        buffer = (buffer << 5) | u32::from(value);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    (!output.is_empty()).then_some(output)
}

fn generate_recovery_codes() -> Vec<String> {
    let mut rng = rand::rng();
    (0..RECOVERY_CODE_COUNT)
        .map(|_| {
            let raw = (0..12)
                .map(|_| {
                    let index = (rng.next_u32() as usize) % RECOVERY_CODE_ALPHABET.len();
                    char::from(RECOVERY_CODE_ALPHABET[index])
                })
                .collect::<String>();
            format!("{}-{}-{}", &raw[..4], &raw[4..8], &raw[8..])
        })
        .collect()
}

fn normalize_recovery_code(code: &str) -> Option<String> {
    let normalized = code
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '-')
        .flat_map(char::to_uppercase)
        .collect::<String>();
    (normalized.len() == 12
        && normalized
            .bytes()
            .all(|byte| RECOVERY_CODE_ALPHABET.contains(&byte)))
    .then_some(normalized)
}

fn recovery_code_hash(code: &str) -> Vec<u8> {
    let normalized = normalize_recovery_code(code).unwrap_or_else(|| code.to_string());
    Sha256::digest(format!("synctv-totp-recovery-v1:{normalized}")).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6238_sha1_vectors_are_truncated_to_six_digits() {
        let secret = base32_encode(b"12345678901234567890");
        for (timestamp, expected) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
            (20_000_000_000, "353130"),
        ] {
            assert_eq!(
                totp_code(b"12345678901234567890", timestamp / 30).as_deref(),
                Some(expected)
            );
            assert_eq!(
                verify_totp_at(&secret, expected, timestamp, None),
                Some(timestamp / 30)
            );
        }
    }

    #[test]
    fn replayed_step_and_malformed_codes_are_rejected() {
        let secret = base32_encode(b"12345678901234567890");
        let timestamp = 1_234_567_890;
        let step = timestamp / 30;
        assert_eq!(
            verify_totp_at(&secret, "005924", timestamp, Some(step)),
            None
        );
        assert_eq!(verify_totp_at(&secret, "5924", timestamp, None), None);
        assert_eq!(verify_totp_at(&secret, "00A924", timestamp, None), None);
    }

    #[test]
    fn recovery_codes_normalize_and_hash_consistently() {
        assert_eq!(
            recovery_code_hash("ABCD-EFGH-JKLM"),
            recovery_code_hash("abcd efgh jklm")
        );
        assert_eq!(
            normalize_recovery_code("abcd efgh jklm").as_deref(),
            Some("ABCDEFGHJKLM")
        );
    }
}
