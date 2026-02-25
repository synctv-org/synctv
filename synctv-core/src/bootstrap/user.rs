// ! Bootstrap user initialization

use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    config::BootstrapConfig,
    models::{User, UserRole, UserStatus},
    repository::UserRepository,
    service::auth::hash_password,
    Error, Result,
};

/// Bootstrap root user on first startup.
///
/// Creates a root user if none exists and bootstrap is enabled. On first
/// deployment (no users in the database at all), bootstrap failure is fatal
/// because the system would have no way to be administered. If users already
/// exist, the failure is logged as a warning and startup continues.
///
/// Should be called after database migrations but before service initialization.
pub async fn bootstrap_root_user(pool: &PgPool, config: &BootstrapConfig) -> Result<()> {
    if !config.create_root_user {
        info!("Root user bootstrap disabled in config");
        return Ok(());
    }

    let repository = UserRepository::new(pool.clone());

    // Check if any root user exists
    let root_exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM users WHERE role = $1 AND deleted_at IS NULL LIMIT 1)"
    )
    .bind(UserRole::Root)
    .fetch_one(pool)
    .await?;

    if root_exists {
        info!("Root user already exists, skipping bootstrap");
        return Ok(());
    }

    // Issue #40: If the configured root username is taken by a non-root user,
    // fail loudly. Silently skipping root creation would leave the system
    // without a root account, which is a critical operational problem.
    // Operators must either rename the conflicting user or choose a different
    // root username in the bootstrap config.
    if repository.username_exists(&config.root_username).await? {
        return Err(Error::Internal(format!(
            "Bootstrap failed: root username '{}' is already taken by a non-root user. \
             Please either change bootstrap.root_username in your config, \
             or manually promote the existing user to root role.",
            config.root_username
        )));
    }

    // Create root user
    info!("Creating root user '{}'...", config.root_username);

    let password_hash = hash_password(&config.root_password).await?;

    // Create user with root role and active status
    let mut user = User::new(
        config.root_username.clone(),
        None, // No email required for root
        password_hash,
        None, // No signup method for root
    );

    // Override defaults to set root role and active status
    user.role = UserRole::Root;
    user.status = UserStatus::Active;

    let created_user = repository.create(&user).await?;

    info!("Root user created successfully:");
    info!("  ID: {}", created_user.id.as_str());
    info!("  Username: {}", created_user.username);
    info!("  Role: {:?}", created_user.role);
    info!("  Status: {:?}", created_user.status);

    if config.root_password == "root" {
        warn!("WARNING: Root password is set to default value 'root'");
        warn!("Please change the root password immediately after first login!");
    }

    Ok(())
}

/// Check whether any (non-deleted) users exist in the database.
///
/// Used during startup to distinguish first deployment (no users) from
/// subsequent starts. On first deployment, root bootstrap failure is fatal.
pub async fn has_any_users(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM users WHERE deleted_at IS NULL LIMIT 1)",
    )
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SignupMethod;

    #[test]
    fn test_bootstrap_config_defaults() {
        let config = BootstrapConfig::default();
        // Default is false for security - operators must explicitly enable root creation
        assert!(!config.create_root_user);
        assert_eq!(config.root_username, "root");
        // Default password is empty for security
        assert!(config.root_password.is_empty());
    }

    #[test]
    fn test_user_creation_with_root_role() {
        let password_hash = "test_hash".to_string();
        let mut user = User::new(
            "test_root".to_string(),
            None,
            password_hash,
            Some(SignupMethod::Email),
        );

        // Verify defaults
        assert_eq!(user.role, UserRole::User);
        assert_eq!(user.status, UserStatus::Pending);

        // Override to root
        user.role = UserRole::Root;
        user.status = UserStatus::Active;

        assert_eq!(user.role, UserRole::Root);
        assert_eq!(user.status, UserStatus::Active);
    }

    // Integration tests require database connection
    // Run with: cargo test --test bootstrap_integration
}
