// ! Bootstrap user initialization

use sqlx::PgPool;
use tracing::info;

use synctv_core::{
    models::{SignupMethod, User, UserRole},
    repository::{PasswordCredentialMaterial, UserPasswordRepository, UserRepository},
    service::OpaquePasswordService,
    Error, Result,
};

const KNOWN_DEV_ROOT_PASSWORDS: &[&str] = &["Rootpasswd1234567890!", "DevRootPass12345"];

#[derive(Debug, Clone)]
pub struct RootUserBootstrapOptions {
    pub create_root_user: bool,
    pub root_username: String,
    pub root_password: String,
}

impl Default for RootUserBootstrapOptions {
    fn default() -> Self {
        Self {
            create_root_user: false,
            root_username: "root".to_string(),
            root_password: String::new(),
        }
    }
}

impl RootUserBootstrapOptions {
    #[must_use]
    pub fn validate_root_password_for_creation(&self) -> Vec<String> {
        let mut errors = Vec::new();
        let pwd = &self.root_password;
        if pwd.is_empty() {
            errors.push("Root password is empty".to_string());
            return errors;
        }
        if pwd == "root" || KNOWN_DEV_ROOT_PASSWORDS.contains(&pwd.as_str()) {
            errors.push("Root password is set to default value 'root'".to_string());
            return errors;
        }
        if pwd.len() < 12 {
            errors.push("Root password must be at least 12 characters".to_string());
        }
        if !pwd.chars().any(char::is_uppercase) {
            errors.push("Root password must contain at least one uppercase letter".to_string());
        }
        if !pwd.chars().any(char::is_lowercase) {
            errors.push("Root password must contain at least one lowercase letter".to_string());
        }
        if !pwd.chars().any(|c| c.is_ascii_digit()) {
            errors.push("Root password must contain at least one digit".to_string());
        }
        errors
    }
}

fn bootstrap_exists_value(value: Option<bool>, query_description: &str) -> Result<bool> {
    value.ok_or_else(|| {
        Error::Internal(format!(
            "{query_description} EXISTS query returned no scalar value"
        ))
    })
}

/// Bootstrap root user on first startup.
///
/// Creates a root user if none exists and bootstrap is enabled. On first
/// deployment (no users in the database at all), bootstrap failure is fatal
/// because the system would have no way to be administered. If users already
/// exist, the failure is logged as a warning and startup continues.
///
/// Should be called after database migrations but before service initialization.
pub async fn bootstrap_root_user(
    pool: &PgPool,
    options: &RootUserBootstrapOptions,
    opaque_server_setup_secret: &str,
) -> Result<()> {
    if !options.create_root_user {
        info!("Root user bootstrap disabled in options");
        return Ok(());
    }

    let repository = UserRepository::new(pool.clone());
    let user_password_repository = UserPasswordRepository::new(pool.clone());

    // Check if any root user exists
    let root_exists = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM users WHERE role = $1 AND deleted_at IS NULL LIMIT 1)",
        i16::from(UserRole::Root)
    )
    .fetch_one(pool)
    .await?;
    let root_exists = bootstrap_exists_value(root_exists, "root user bootstrap")?;

    if root_exists {
        info!("Root user already exists, skipping bootstrap");
        return Ok(());
    }

    // A root username collision leaves the system without an administrator.
    // Operators can rename the conflicting user or choose a different root
    // username in the startup settings.
    if repository.username_exists(&options.root_username).await? {
        return Err(Error::Internal(format!(
            "Bootstrap failed: root username '{}' is already taken by a non-root user. \
             Change bootstrap.root_username in startup settings, \
             or promote the existing user to root role.",
            options.root_username
        )));
    }

    let password_errors = options.validate_root_password_for_creation();
    if !password_errors.is_empty() {
        return Err(Error::Internal(format!(
            "Bootstrap failed: invalid root password: {}",
            password_errors.join("; ")
        )));
    }

    info!("Creating root user '{}'...", options.root_username);

    let opaque_password_service =
        OpaquePasswordService::derive_from_secret(opaque_server_setup_secret.as_bytes());
    let opaque_record = opaque_password_service.register_password(
        format!("synctv:user:{}", options.root_username.trim()).as_bytes(),
        &options.root_password,
    )?;

    let mut user = User::new(
        options.root_username.clone(),
        SignupMethod::AdminCreated, // Root user created via bootstrap options
    );

    user.role = UserRole::Root;

    let mut tx = pool.begin().await?;
    let created_user = repository.create_with_executor(&user, &mut *tx).await?;
    user_password_repository
        .create_for_user_with_executor(
            &created_user,
            PasswordCredentialMaterial::opaque_only(&opaque_record),
            &mut *tx,
        )
        .await?;
    tx.commit().await?;

    info!("Root user created successfully:");
    info!("  ID: {}", created_user.id);
    info!("  Username: {}", created_user.username);
    info!("  Role: {:?}", created_user.role);

    Ok(())
}

/// Check whether any active administrator-capable users exist in the database.
///
/// Startup may continue after bootstrap failure only when the system already has
/// an existing administrative account (`root` or `admin`) that can manage it.
pub async fn has_any_admin_users(pool: &PgPool) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        "SELECT EXISTS(
            SELECT 1
            FROM users
            WHERE deleted_at IS NULL
              AND (role = $1 OR role = $2)
            LIMIT 1
        )",
        i16::from(UserRole::Root),
        i16::from(UserRole::Admin)
    )
    .fetch_one(pool)
    .await?;
    bootstrap_exists_value(exists, "administrator user")
}

#[cfg(test)]
mod tests {
    use super::*;
    use synctv_core_testing::TestResultExt;

    #[test]
    fn bootstrap_exists_value_rejects_missing_scalar() {
        let error =
            bootstrap_exists_value(None, "administrator user").failed("missing scalar fails");

        assert!(matches!(
            error,
            Error::Internal(message) if message.contains("administrator user")
        ));
    }

    #[test]
    fn bootstrap_exists_value_accepts_scalar() {
        assert!(bootstrap_exists_value(Some(true), "administrator user")
            .checked("operation should succeed"));
        assert!(!bootstrap_exists_value(Some(false), "administrator user")
            .checked("operation should succeed"));
    }
}
