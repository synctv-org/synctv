use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, ParamsBuilder, Version,
};
use std::sync::LazyLock;
use tokio::task;

use crate::{Error, Result};

fn build_argon2() -> Result<Argon2<'static>> {
    let params = ParamsBuilder::new()
        .m_cost(65536)
        .t_cost(3)
        .p_cost(4)
        .output_len(32)
        .build()
        .map_err(|e| Error::Internal(format!("Failed to build Argon2 params: {e}")))?;

    Ok(Argon2::new(
        argon2::Algorithm::Argon2id,
        Version::V0x13,
        params,
    ))
}

fn hash_password_with_argon2(
    password: &str,
    salt: &SaltString,
    argon2: &Argon2<'_>,
) -> Result<String> {
    argon2
        .hash_password(password.as_bytes(), salt)
        .map_err(|e| Error::Internal(format!("Failed to hash password: {e}")))
        .map(|password_hash| password_hash.to_string())
}

fn verify_password_with_argon2(password: &str, hash: &str, argon2: &Argon2<'_>) -> Result<bool> {
    let parsed_hash = PasswordHash::new(hash)
        .map_err(|e| Error::Internal(format!("Invalid password hash format: {e}")))?;

    match argon2.verify_password(password.as_bytes(), &parsed_hash) {
        Ok(()) => Ok(true),
        Err(argon2::password_hash::Error::Password) => Ok(false),
        Err(e) => Err(Error::Internal(format!(
            "Password verification failed: {e}"
        ))),
    }
}

static DUMMY_PASSWORD_HASH: LazyLock<String> = LazyLock::new(|| {
    let salt = SaltString::encode_b64(b"synctv-dummy-salt")
        .unwrap_or_else(|e| panic!("Failed to build dummy password salt: {e}"));
    let argon2 =
        build_argon2().unwrap_or_else(|e| panic!("Failed to build dummy Argon2 instance: {e}"));

    argon2
        .hash_password(b"synctv-dummy-password", &salt)
        .unwrap_or_else(|e| panic!("Failed to hash dummy password: {e}"))
        .to_string()
});

#[must_use]
pub fn dummy_password_hash() -> &'static str {
    DUMMY_PASSWORD_HASH.as_str()
}

/// Hash a password using Argon2id with recommended parameters
///
/// Uses PHC 2023 winner Argon2id with parameters:
/// - Memory: 64 MB
/// - Iterations: 3
/// - Parallelism: 4
/// - Output length: 32 bytes
///
/// This is a CPU-intensive operation and should be run on a blocking thread.
pub async fn hash_password(password: &str) -> Result<String> {
    let password = password.to_string();

    task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = build_argon2()?;
        hash_password_with_argon2(&password, &salt, &argon2)
    })
    .await
    .map_err(|e| Error::Internal(format!("Password hashing task failed: {e}")))?
}

/// Verify a password against a stored hash
///
/// This is a CPU-intensive operation and should be run on a blocking thread.
pub async fn verify_password(password: &str, hash: &str) -> Result<bool> {
    let password = password.to_string();
    let hash = hash.to_string();

    task::spawn_blocking(move || {
        let argon2 = Argon2::default();
        verify_password_with_argon2(&password, &hash, &argon2)
    })
    .await
    .map_err(|e| Error::Internal(format!("Password verification task failed: {e}")))?
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn build_test_argon2() -> Argon2<'static> {
        let params = ParamsBuilder::new()
            .m_cost(8 * 1024)
            .t_cost(1)
            .p_cost(1)
            .output_len(32)
            .build()
            .expect("test Argon2 params should build");

        Argon2::new(argon2::Algorithm::Argon2id, Version::V0x13, params)
    }

    fn hash_password_for_tests(password: &str) -> String {
        let salt = SaltString::generate(&mut OsRng);
        let argon2 = build_test_argon2();
        hash_password_with_argon2(password, &salt, &argon2).expect("test password hashing")
    }

    #[tokio::test]
    async fn test_hash_password() {
        let password = "test_password_123";
        let hash = hash_password(password).await.unwrap();

        // PHC format: $argon2id$v=19$m=65536,t=3,p=4$...
        assert!(hash.starts_with("$argon2id$"));
        assert!(hash.len() > 50);
    }

    #[tokio::test]
    async fn test_verify_password_correct() {
        let password = "test_password_123";
        let hash = hash_password_for_tests(password);

        let is_valid = verify_password_with_argon2(password, &hash, &build_test_argon2()).unwrap();
        assert!(is_valid);
    }

    #[tokio::test]
    async fn test_verify_password_incorrect() {
        let password = "test_password_123";
        let hash = hash_password_for_tests(password);

        let is_valid =
            verify_password_with_argon2("wrong_password", &hash, &build_test_argon2()).unwrap();
        assert!(!is_valid);
    }

    #[tokio::test]
    async fn test_hash_uniqueness() {
        let password = "test_password_123";
        let hash1 = hash_password_for_tests(password);
        let hash2 = hash_password_for_tests(password);

        assert_ne!(hash1, hash2);
        assert!(verify_password_with_argon2(password, &hash1, &build_test_argon2()).unwrap());
        assert!(verify_password_with_argon2(password, &hash2, &build_test_argon2()).unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_long_password() {
        let password = "a".repeat(1000);
        let hash = hash_password_for_tests(&password);
        assert!(verify_password_with_argon2(&password, &hash, &build_test_argon2()).unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_unicode() {
        let password = "密码测试🔐🔒123";
        let hash = hash_password_for_tests(password);
        assert!(verify_password_with_argon2(password, &hash, &build_test_argon2()).unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_empty() {
        let password = "";
        let hash = hash_password_for_tests(password);
        assert!(verify_password_with_argon2(password, &hash, &build_test_argon2()).unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_special_chars() {
        let password = "P@ssw0rd!#$%^&*()_+-=[]{}|;':\",./<>?`~";
        let hash = hash_password_for_tests(password);
        assert!(verify_password_with_argon2(password, &hash, &build_test_argon2()).unwrap());
    }

    #[test]
    fn test_dummy_password_hash_is_valid() {
        let hash = dummy_password_hash();
        assert!(hash.starts_with("$argon2id$"));
        assert!(PasswordHash::new(hash).is_ok());
    }
}
