use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2, ParamsBuilder, Version,
};
use tokio::task;

use crate::{Error, Result};

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
        // Generate a random salt
        let salt = SaltString::generate(&mut OsRng);

        // Configure Argon2id parameters (PHC 2023 recommended)
        let params = ParamsBuilder::new()
            .m_cost(65536) // 64 MB
            .t_cost(3)     // 3 iterations
            .p_cost(4)     // 4 parallel threads
            .output_len(32) // 32 bytes output
            .build()
            .map_err(|e| Error::Internal(format!("Failed to build Argon2 params: {e}")))?;

        let argon2 = Argon2::new(
            argon2::Algorithm::Argon2id,
            Version::V0x13,
            params,
        );

        // Hash the password
        let password_hash = argon2
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| Error::Internal(format!("Failed to hash password: {e}")))?
            .to_string();

        Ok(password_hash)
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
        // Parse the PHC string
        let parsed_hash = PasswordHash::new(&hash)
            .map_err(|e| Error::Internal(format!("Invalid password hash format: {e}")))?;

        // Verify the password
        let argon2 = Argon2::default();
        match argon2.verify_password(password.as_bytes(), &parsed_hash) {
            Ok(()) => Ok(true),
            Err(argon2::password_hash::Error::Password) => Ok(false),
            Err(e) => Err(Error::Internal(format!("Password verification failed: {e}"))),
        }
    })
    .await
    .map_err(|e| Error::Internal(format!("Password verification task failed: {e}")))?
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let hash = hash_password(password).await.unwrap();

        let is_valid = verify_password(password, &hash).await.unwrap();
        assert!(is_valid);
    }

    #[tokio::test]
    async fn test_verify_password_incorrect() {
        let password = "test_password_123";
        let hash = hash_password(password).await.unwrap();

        let is_valid = verify_password("wrong_password", &hash).await.unwrap();
        assert!(!is_valid);
    }

    #[tokio::test]
    async fn test_hash_uniqueness() {
        let password = "test_password_123";
        let hash1 = hash_password(password).await.unwrap();
        let hash2 = hash_password(password).await.unwrap();

        // Same password should produce different hashes (different salts)
        assert_ne!(hash1, hash2);

        // But both should verify correctly
        assert!(verify_password(password, &hash1).await.unwrap());
        assert!(verify_password(password, &hash2).await.unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_long_password() {
        // Test with a very long password (should still work)
        let password = "a".repeat(1000);
        let hash = hash_password(&password).await.unwrap();
        assert!(verify_password(&password, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_unicode() {
        // Test with unicode characters
        let password = "密码测试🔐🔒123";
        let hash = hash_password(password).await.unwrap();
        assert!(verify_password(password, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_empty() {
        // Empty password should still hash (validation happens elsewhere)
        let password = "";
        let hash = hash_password(password).await.unwrap();
        assert!(verify_password(password, &hash).await.unwrap());
    }

    #[tokio::test]
    async fn test_hash_password_special_chars() {
        // Test with special characters
        let password = "P@ssw0rd!#$%^&*()_+-=[]{}|;':\",./<>?`~";
        let hash = hash_password(password).await.unwrap();
        assert!(verify_password(password, &hash).await.unwrap());
    }
}
