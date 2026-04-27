// ! Bootstrap user initialization

use sqlx::PgPool;
use tracing::{info, warn};

use crate::{
    config::BootstrapConfig,
    models::{SignupMethod, User, UserRole},
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
        "SELECT EXISTS(SELECT 1 FROM users WHERE role = $1 AND deleted_at IS NULL LIMIT 1)",
    )
    .bind(UserRole::Root)
    .fetch_one(pool)
    .await?;

    if root_exists {
        info!("Root user already exists, skipping bootstrap");
        return Ok(());
    }

    // If the configured root username is taken by a non-root user,
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

    info!("Creating root user '{}'...", config.root_username);

    let password_hash = hash_password(&config.root_password).await?;

    let mut user = User::new(
        config.root_username.clone(),
        (!config.root_email.is_empty()).then(|| config.root_email.clone()),
        password_hash,
        SignupMethod::AdminCreated, // Root user created via bootstrap config
    );

    user.role = UserRole::Root;

    let created_user = repository.create(&user).await?;

    info!("Root user created successfully:");
    info!("  ID: {}", created_user.id);
    info!("  Username: {}", created_user.username);
    info!("  Role: {:?}", created_user.role);

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

/// Check whether any active administrator-capable users exist in the database.
///
/// Startup may continue after bootstrap failure only when the system already has
/// an existing administrative account (`root` or `admin`) that can manage it.
pub async fn has_any_admin_users(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(
            SELECT 1
            FROM users
            WHERE deleted_at IS NULL
              AND (role = $1 OR role = $2)
            LIMIT 1
        )",
    )
    .bind(UserRole::Root)
    .bind(UserRole::Admin)
    .fetch_one(pool)
    .await
    .unwrap_or(false)
}
