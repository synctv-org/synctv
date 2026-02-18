// ! Bootstrap user initialization

use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    config::BootstrapConfig,
    models::{User, UserRole, UserStatus},
    repository::UserRepository,
    service::auth::hash_password,
    Result,
};

/// Bootstrap root user on first startup
///
/// This function creates a root user if none exists and bootstrap is enabled.
/// It should be called after database migrations but before service initialization.
///
/// # Arguments
///
/// * `pool` - Database connection pool
/// * `config` - Bootstrap configuration
///
/// # Returns
///
/// * `Ok(())` if root user exists or was created successfully
/// * `Err` if database error occurs
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

    // Check if username already exists (could be a non-root user)
    if repository.username_exists(&config.root_username).await? {
        warn!(
            "Username '{}' already exists but is not a root user. Skipping root user creation.",
            config.root_username
        );
        warn!("Please manually promote this user to root role or choose a different username.");
        return Ok(());
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

    info!("✓ Root user created successfully:");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::SignupMethod;

    #[test]
    fn test_bootstrap_config_defaults() {
        let config = BootstrapConfig::default();
        assert!(config.create_root_user);
        assert_eq!(config.root_username, "root");
        assert_eq!(config.root_password, "root");
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
