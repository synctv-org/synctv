use sqlx::PgPool;
use std::collections::HashMap;

use std::sync::Arc;

use crate::{
    cache::{CacheInvalidationService, UsernameCache},
    config::PasswordComplexityConfig,
    models::{User, UserId, SignupMethod},
    models::oauth2_client::OAuth2Provider,
    repository::{UserRepository, UserOAuthProviderRepository},
    service::auth::{hash_password, verify_password, JwtService, TokenType, BruteForceProtection},
    Error, Result,
};

/// User service for business logic
#[derive(Clone)]
pub struct UserService {
    pub(crate) repository: UserRepository,
    jwt_service: JwtService,
    username_cache: UsernameCache,
    /// Optional cache invalidation service for cross-replica user cache sync
    cache_invalidation: Option<Arc<CacheInvalidationService>>,
    /// Password complexity configuration from config file
    password_complexity: PasswordComplexityConfig,
    /// Optional brute-force protection for login attempts
    brute_force: Option<BruteForceProtection>,
    /// Whether email verification is required for login (true when email service is configured)
    email_verification_required: bool,
}

impl std::fmt::Debug for UserService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserService")
            .field("username_cache", &self.username_cache)
            .finish()
    }
}

impl UserService {
    #[must_use]
    pub const fn new(
        pool: PgPool,
        jwt_service: JwtService,
        username_cache: UsernameCache,
        password_complexity: PasswordComplexityConfig,
    ) -> Self {
        Self {
            repository: UserRepository::new(pool),
            jwt_service,
            username_cache,
            cache_invalidation: None,
            password_complexity,
            brute_force: None,
            email_verification_required: false,
        }
    }

    /// Set the cache invalidation service for cross-replica user cache sync
    pub fn set_cache_invalidation(&mut self, service: Arc<CacheInvalidationService>) {
        self.cache_invalidation = Some(service);
    }

    /// Set the brute-force protection service for per-account login rate limiting
    pub fn set_brute_force_protection(&mut self, service: BruteForceProtection) {
        self.brute_force = Some(service);
    }

    /// Enable email verification requirement for login (call when email service is configured)
    pub const fn set_email_verification_required(&mut self, required: bool) {
        self.email_verification_required = required;
    }

    /// Register a new user
    ///
    /// Uniqueness of username/email is enforced atomically by the database
    /// UNIQUE constraints, avoiding any check-then-act (TOCTOU) race condition.
    ///
    /// When email verification is required (email service is configured), tokens
    /// are NOT returned -- the user must verify their email first. When email
    /// verification is not required, tokens are returned immediately.
    pub async fn register(
        &self,
        username: String,
        email: Option<String>,
        password: String,
    ) -> Result<(User, Option<String>, Option<String>)> {
        // Validate input
        self.validate_username(&username)?;
        if let Some(ref email) = email {
            self.validate_email(email)?;
        }
        self.validate_password(&password)?;

        // Hash password
        let password_hash = hash_password(&password).await?;

        // Set initial status based on email verification config.
        // When verification is required, users start as Pending and must verify
        // their email before they can log in. When verification is disabled,
        // users start as Active and receive tokens immediately.
        let initial_status = if self.email_verification_required {
            crate::models::UserStatus::Pending
        } else {
            crate::models::UserStatus::Active
        };

        // Create user with email signup method.
        // The database UNIQUE constraints on username and email will reject
        // duplicates atomically -- no separate existence check needed.
        let user = User::new_with_status(username.clone(), email.clone(), password_hash, Some(SignupMethod::Email), initial_status);
        let created_user = self.repository.create(&user).await?;

        // Populate username cache
        self.username_cache.set(&created_user.id, &username).await?;

        // When email verification is required, do NOT issue tokens for pending users.
        // The user must complete email verification before they can authenticate.
        if self.email_verification_required {
            return Ok((created_user, None, None));
        }

        // Generate JWT tokens (role will be fetched from DB on each request)
        let access_token = self
            .jwt_service
            .sign_token(&created_user.id, TokenType::Access, created_user.password_version)?;
        let refresh_token = self
            .jwt_service
            .sign_token(&created_user.id, TokenType::Refresh, created_user.password_version)?;

        Ok((created_user, Some(access_token), Some(refresh_token)))
    }

    /// Register a new user using a provided executor (pool or transaction)
    pub async fn register_with_executor<'e, E>(
        &self,
        username: String,
        email: Option<String>,
        password: String,
        signup_method: SignupMethod,
        executor: E,
    ) -> Result<User>
    where
        E: sqlx::PgExecutor<'e>,
    {
        self.validate_username(&username)?;
        if let Some(ref email) = email {
            self.validate_email(email)?;
        }
        self.validate_password(&password)?;
        let password_hash = hash_password(&password).await?;
        let user = User::new(username, email, password_hash, Some(signup_method));
        self.repository.create_with_executor(&user, executor).await
    }

    /// Create a user with a specific role (for admin user creation).
    ///
    /// Validates input, hashes the password, creates the user with the given
    /// role atomically, and populates the username cache.
    pub async fn create_user_with_role(
        &self,
        username: String,
        email: Option<String>,
        password: String,
        role: Option<crate::models::UserRole>,
    ) -> Result<User> {
        self.validate_username(&username)?;
        if let Some(ref email) = email {
            self.validate_email(email)?;
        }
        self.validate_password(&password)?;

        let password_hash = hash_password(&password).await?;
        let mut user = User::new(username.clone(), email, password_hash, Some(SignupMethod::Email));
        if let Some(role) = role {
            user.role = role;
        }
        let created_user = self.repository.create(&user).await?;
        self.username_cache.set(&created_user.id, &username).await?;
        Ok(created_user)
    }

    /// Generate JWT tokens and populate username cache for a newly created user.
    pub async fn finalize_registration(&self, user: &User) -> Result<(String, String)> {
        self.username_cache.set(&user.id, &user.username).await?;
        let access_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Access, user.password_version)?;
        let refresh_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Refresh, user.password_version)?;
        Ok((access_token, refresh_token))
    }

    /// Login user
    ///
    /// Timing-safe: always performs password verification regardless of user existence
    /// to prevent username enumeration via response time analysis.
    ///
    /// Includes per-account and per-IP brute-force protection: after repeated failures,
    /// accounts/IPs are temporarily locked with exponential backoff (1min / 5min / 15min).
    pub async fn login(
        &self,
        username: String,
        password: String,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<(User, String, String)> {
        // Check brute-force lockout before expensive Argon2 verification.
        // This applies to all usernames (existing or not) to prevent
        // distributed attacks while also saving CPU on locked accounts.
        if let Some(ref bf) = self.brute_force {
            bf.check_allowed(&username, client_ip).await?;
        }

        // Get user by username
        let maybe_user = self
            .repository
            .get_by_username(&username)
            .await?;

        // Always perform password verification to prevent timing side-channel.
        // If the user doesn't exist, verify against a dummy hash so the response
        // time is indistinguishable from a real verification.
        let (is_valid, user) = if let Some(user) = maybe_user {
            let valid = verify_password(&password, &user.password_hash).await?;
            (valid, Some(user))
        } else {
            // Dummy Argon2 verification to match timing of real verification.
            // This hash is pre-computed and never matches any real password.
            let dummy_hash = "$argon2id$v=19$m=65536,t=3,p=4$c29tZXNhbHQ$RdescudvJCsgt3ub+b+daw";
            let _ = verify_password(&password, dummy_hash).await;
            (false, None)
        };

        // After constant-time verification, check all failure conditions
        let user = match user {
            Some(u) if is_valid => u,
            _ => {
                // Record failed attempt for brute-force tracking
                if let Some(ref bf) = self.brute_force {
                    if let Err(e) = bf.record_failure(&username, client_ip).await {
                        tracing::warn!(error = %e, "Failed to record login failure for brute-force tracking");
                    }
                }
                return Err(Error::Authentication("Authentication failed".to_string()));
            }
        };

        // Check if user is banned, pending, or soft-deleted (generic message to prevent enumeration)
        if user.status == crate::models::UserStatus::Banned
            || user.status == crate::models::UserStatus::Pending
            || user.deleted_at.is_some()
        {
            // Record failure (account is locked/deleted but attacker shouldn't know)
            if let Some(ref bf) = self.brute_force {
                if let Err(e) = bf.record_failure(&username, client_ip).await {
                    tracing::warn!(error = %e, "Failed to record login failure for brute-force tracking");
                }
            }
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        // Check email verification when email service is configured
        if self.email_verification_required && user.email.is_some() && !user.email_verified {
            return Err(Error::Authentication(
                "Email not verified. Please check your inbox for a verification link.".to_string(),
            ));
        }

        // Successful login: reset brute-force counter
        if let Some(ref bf) = self.brute_force {
            if let Err(e) = bf.reset(&username).await {
                tracing::warn!(error = %e, "Failed to reset brute-force counter after successful login");
            }
        }

        // Generate JWT tokens (role will be fetched from DB on each request)
        let access_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Access, user.password_version)?;
        let refresh_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Refresh, user.password_version)?;

        Ok((user, access_token, refresh_token))
    }

    /// Generate token pair for `OAuth2` login (user already authenticated by `OAuth2` provider)
    ///
    /// This method generates access and refresh tokens for a user who has been
    /// authenticated via `OAuth2`. Unlike `login()`, this skips password verification.
    pub async fn login_oauth2(&self, user_id: &UserId) -> Result<(User, String, String)> {
        // Get user to ensure they exist and are active
        let user = self.repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        // Check user status (generic message to prevent user enumeration)
        if user.is_deleted()
            || user.status == crate::models::UserStatus::Banned
            || user.status == crate::models::UserStatus::Pending
        {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        // Generate JWT tokens
        let access_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Access, user.password_version)?;
        let refresh_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Refresh, user.password_version)?;

        Ok((user, access_token, refresh_token))
    }

    /// Refresh access token
    ///
    /// **Security Note**: Without Redis-based token blacklisting, this implementation
    /// does not provide refresh token replay protection. A stolen refresh token can
    /// be used until it expires naturally. Consider these trade-offs:
    /// - Tokens are still validated for signature, expiration, and password changes
    /// - Shorter token lifetimes reduce the replay window
    /// - Password changes immediately invalidate all tokens
    pub async fn refresh_token(&self, refresh_token: String) -> Result<(String, String)> {
        // Verify refresh token
        let claims = self.jwt_service.verify_refresh_token(&refresh_token)?;
        let user_id = UserId::from_string(claims.sub);

        // Get user to ensure they still exist and are active
        let user = self
            .repository
            .get_by_id(&user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        // Reject banned, pending, or soft-deleted users (generic message to prevent enumeration)
        if user.status == crate::models::UserStatus::Banned
            || user.status == crate::models::UserStatus::Pending
            || user.deleted_at.is_some()
        {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        // Reject refresh tokens issued with an old password version
        if let Some(token_pv) = claims.pv {
            if token_pv < user.password_version {
                return Err(Error::Authentication("Authentication failed".to_string()));
            }
        } else {
            // Legacy tokens without pv: fall back to iat-based check
            if claims.iat < user.password_changed_at.timestamp() {
                return Err(Error::Authentication("Authentication failed".to_string()));
            }
        }

        // Generate new tokens (role will be fetched from DB on each request)
        let new_access_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Access, user.password_version)?;
        let new_refresh_token = self
            .jwt_service
            .sign_token(&user.id, TokenType::Refresh, user.password_version)?;

        Ok((new_access_token, new_refresh_token))
    }

    /// Get user by ID
    pub async fn get_user(&self, user_id: &UserId) -> Result<User> {
        self.repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))
    }

    /// Get user by email
    pub async fn get_by_email(&self, email: &str) -> Result<Option<User>> {
        self.repository.get_by_email(email).await
    }

    /// Update user (entire user object)
    pub async fn update_user(&self, user: &User) -> Result<User> {
        let updated = self.repository.update(user).await?;
        self.notify_user_invalidation(&user.id).await;
        Ok(updated)
    }

    /// Change user password (requires old password verification)
    pub async fn change_password(&self, user_id: &UserId, old_password: &str, new_password: &str) -> Result<User> {
        // Get user to verify old password
        let user = self.get_user(user_id).await?;

        // Verify old password
        let is_valid = verify_password(old_password, &user.password_hash).await?;
        if !is_valid {
            return Err(Error::Authentication("Invalid current password".to_string()));
        }

        // Delegate to set_password for the actual update
        self.set_password(user_id, new_password).await
    }

    /// Set user password (admin use, no old password required)
    ///
    /// After updating the password, all existing tokens for the user are
    /// invalidated. This is done by updating the `password_changed_at`
    /// timestamp in the database, which causes all tokens with iat <
    /// `password_changed_at` to be rejected by the security pipeline.
    pub async fn set_password(&self, user_id: &UserId, new_password: &str) -> Result<User> {
        // Validate new password
        self.validate_password(new_password)?;

        // Hash new password
        let password_hash = hash_password(new_password).await?;

        // Update password in database (this also updates password_changed_at,
        // which invalidates all tokens issued before this moment)
        let updated_user = self.repository.update_password(user_id, &password_hash).await?;

        // Invalidate user cache across all replicas
        self.notify_user_invalidation(user_id).await;

        tracing::info!("Password updated for user {}", user_id.as_str());

        Ok(updated_user)
    }

    /// Set user email verification status
    pub async fn set_email_verified(&self, user_id: &UserId, email_verified: bool) -> Result<User> {
        let updated_user = self.repository.update_email_verified(user_id, email_verified).await?;

        // Invalidate user cache across all replicas
        self.notify_user_invalidation(user_id).await;

        tracing::info!(
            "Email verification status set to {} for user {}",
            email_verified,
            user_id.as_str()
        );

        Ok(updated_user)
    }

    /// List users with query (admin function)
    pub async fn list_users(&self, query: &crate::models::UserListQuery) -> Result<(Vec<User>, i64)> {
        self.repository.list(query).await
    }

    /// Delete all `OAuth2` provider mappings for a user.
    ///
    /// Used during user deletion to clean up OAuth bindings.
    pub async fn cleanup_oauth_providers(&self, user_id: &UserId) -> Result<u64> {
        let repo = UserOAuthProviderRepository::new(self.repository.pool().clone());
        repo.delete_all_for_user(user_id).await
    }

    /// Soft-delete the currently authenticated user's own account.
    ///
    /// This is the self-service account deletion endpoint. It sets `deleted_at = NOW()`
    /// on the user row so all subsequent token validation will fail (the security pipeline
    /// checks `is_deleted()`). `OAuth2` mappings are cleaned up in the same transaction.
    pub async fn delete_self(&self, user_id: &UserId) -> Result<()> {
        self.delete_user(user_id).await
    }

    /// Soft-delete a user and clean up all related resources.
    ///
    /// Performs the following cleanup in order:
    /// 1. Within a single DB transaction:
    ///    a. Soft-delete the user row
    ///    b. Remove all `OAuth2` provider mappings
    /// 2. Invalidate username cache (best-effort)
    /// 3. Invalidate user cache across replicas (best-effort)
    ///
    /// Steps 1a and 1b are atomic: if `OAuth2` cleanup fails, the soft-delete
    /// is rolled back to prevent orphaned mappings.
    ///
    /// **Token Invalidation**: Tokens are invalidated implicitly because the
    /// security pipeline checks for deleted users (`deleted_at` IS NOT NULL).
    pub async fn delete_user(&self, user_id: &UserId) -> Result<()> {
        let user = self.get_user(user_id).await?;
        if user.deleted_at.is_some() {
            return Err(Error::InvalidInput("User is already deleted".to_string()));
        }

        // 1. Soft-delete + OAuth2 cleanup in a single transaction
        let pool = self.repository.pool();
        let mut tx = pool.begin().await?;

        self.repository.delete_with_executor(user_id, &mut *tx).await?;

        let oauth_repo = UserOAuthProviderRepository::new(pool.clone());
        oauth_repo.delete_all_for_user_with_executor(user_id, &mut *tx).await?;

        tx.commit().await?;

        // 2. Invalidate username cache (best-effort)
        if let Err(e) = self.invalidate_username_cache(user_id).await {
            tracing::warn!(
                error = %e,
                user_id = %user_id.as_str(),
                "Failed to invalidate username cache during user deletion"
            );
        }

        // 3. Invalidate user cache across replicas (best-effort)
        self.notify_user_invalidation(user_id).await;

        tracing::info!(user_id = %user_id.as_str(), "User soft-deleted with full cleanup");

        Ok(())
    }

    /// Create a new user for an `OAuth2` login.
    ///
    /// This method is called during `OAuth2` login flow when no existing provider
    /// mapping was found (the caller must check provider-based lookup first).
    /// It creates a new user with a random password.
    ///
    /// If the desired username is already taken (detected atomically via DB
    /// UNIQUE constraint), a numeric suffix is appended (e.g., "alice" ->
    /// "`alice_2`", "`alice_3`") to avoid collisions. This prevents account
    /// takeover where an `OAuth2` user with a matching username would silently
    /// gain access to an existing local account.
    ///
    /// Note: This method doesn't save the `OAuth2` provider mapping - that's handled
    /// by `OAuth2Service::upsert_user_provider`.
    /// Note: Email is optional for `OAuth2` users.
    pub async fn create_or_load_by_oauth2(
        &self,
        provider: &OAuth2Provider,
        provider_user_id: &str,
        username: &str,
        email: Option<&str>,
    ) -> Result<User> {
        // Sanitize OAuth2 username: remove invalid characters and trim
        let sanitized_username = username
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect::<String>()
            .trim()
            .to_string();

        // If sanitization resulted in empty username, use provider user ID
        let base_username = if sanitized_username.is_empty() {
            format!("user_{}", &provider_user_id[..provider_user_id.len().min(20)])
        } else {
            sanitized_username
        };

        // Validate the sanitized username
        self.validate_username(&base_username)?;

        // Generate a random password (OAuth2 users don't need password login)
        let random_password = nanoid::nanoid!(32);

        // Use provided email, or None if not provided
        let user_email = email.map(std::string::ToString::to_string);

        // Hash password
        let password_hash = hash_password(&random_password).await?;

        // Try to create user with the desired username first. If the DB UNIQUE
        // constraint rejects it, fall back to random-suffixed variants. Using
        // random suffixes (instead of sequential) avoids thundering herd under
        // concurrent OAuth2 signups with the same base username.
        let max_attempts = 10;
        let mut candidates = Vec::with_capacity(max_attempts);
        candidates.push(base_username.clone());
        for _ in 1..max_attempts {
            // Cap the base to leave room for the suffix within the 50-char limit
            let max_base_len = 42;
            // Use character count instead of byte length to avoid panics on multi-byte UTF-8
            let base = if base_username.chars().count() > max_base_len {
                base_username.chars().take(max_base_len).collect::<String>()
            } else {
                base_username.clone()
            };
            // Random 6-char alphanumeric suffix
            let suffix = nanoid::nanoid!(6);
            candidates.push(format!("{base}_{suffix}"));
        }

        for candidate in &candidates {
            let user = User::new(
                candidate.clone(),
                user_email.clone(),
                password_hash.clone(),
                Some(SignupMethod::OAuth2),
            );
            match self.repository.create(&user).await {
                Ok(created_user) => {
                    // Populate username cache
                    self.username_cache.set(&created_user.id, candidate).await?;

                    if candidate == &base_username {
                        tracing::info!(
                            "Created new user {} (username='{}', sanitized from '{}') via OAuth2 provider {} (provider_user_id={})",
                            created_user.id.as_str(),
                            candidate,
                            username,
                            provider.as_str(),
                            provider_user_id
                        );
                    } else {
                        tracing::info!(
                            "Username '{}' was taken; created user {} as '{}' (original '{}') via OAuth2 provider {} (provider_user_id={})",
                            base_username,
                            created_user.id.as_str(),
                            candidate,
                            username,
                            provider.as_str(),
                            provider_user_id
                        );
                    }

                    return Ok(created_user);
                }
                Err(Error::AlreadyExists(ref msg)) if msg.contains("username") || msg.contains("Username") => {
                    // Username conflict -- try next candidate
                    continue;
                }
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(format!(
            "Could not generate a unique username for base '{username}' after {max_attempts} attempts"
        )))
    }

    /// Validate username using production-grade validator
    fn validate_username(&self, username: &str) -> Result<()> {
        crate::validation::UsernameValidator::new()
            .validate(username)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    /// Validate email using regex-based validator
    fn validate_email(&self, email: &str) -> Result<()> {
        let email = email.trim();
        if email.is_empty() {
            return Err(Error::InvalidInput("Email cannot be empty".to_string()));
        }
        crate::validation::EmailValidator::new()
            .validate(email)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    /// Validate password with complexity requirements from config
    fn validate_password(&self, password: &str) -> Result<()> {
        crate::validation::PasswordValidator::from_config(&self.password_complexity)
            .validate(password)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    /// Get username for a user ID (from cache or database)
    ///
    /// This method checks the cache first, then falls back to the database.
    /// The cache is automatically populated on cache miss.
    pub async fn get_username(&self, user_id: &UserId) -> Result<Option<String>> {
        // Check cache first
        if let Some(username) = self.username_cache.get(user_id).await? {
            return Ok(Some(username));
        }

        // Cache miss - fetch from database
        if let Some(user) = self.repository.get_by_id(user_id).await? {
            // Populate cache
            let username = user.username.clone();
            self.username_cache.set(user_id, &username).await?;
            Ok(Some(username))
        } else {
            Ok(None)
        }
    }

    /// Get multiple usernames at once (more efficient)
    ///
    /// Returns a map of `user_id` -> username.
    pub async fn get_usernames(&self, user_ids: &[UserId]) -> Result<HashMap<UserId, String>> {
        // Try batch cache lookup first
        let mut result = self.username_cache.get_batch(user_ids).await?;
        let missing_ids: Vec<UserId> = user_ids
            .iter()
            .filter(|id| !result.contains_key(*id))
            .cloned()
            .collect();

        // Fetch missing usernames from database in a single batch query
        if !missing_ids.is_empty() {
            let users = self.repository.get_by_ids(&missing_ids).await?;
            for user in users {
                let user_id = user.id.clone();
                let username = user.username.clone();
                self.username_cache.set(&user_id, &username).await?;
                result.insert(user_id, username);
            }
        }

        Ok(result)
    }

    /// Invalidate username cache for a user
    ///
    /// This should be called when a user's username is changed.
    pub async fn invalidate_username_cache(&self, user_id: &UserId) -> Result<()> {
        self.username_cache.invalidate(user_id).await
    }

    /// Get the database pool (for creating dependent services)
    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        self.repository.pool()
    }

    /// Get the access token duration in seconds from the JWT service
    ///
    /// Used by `OAuth2` token response to report the correct `expires_in` value.
    #[must_use]
    pub const fn access_token_duration_seconds(&self) -> i64 {
        self.jwt_service.access_token_duration_seconds()
    }

    /// Get the username cache (for creating dependent services)
    #[must_use]
    pub const fn username_cache(&self) -> &UsernameCache {
        &self.username_cache
    }

    /// Health check - verify database connectivity
    ///
    /// Executes a simple query to verify the database connection is working.
    /// Used by readiness probes in Kubernetes deployments.
    ///
    /// # Returns
    /// - `Ok(())` if the database is accessible
    /// - `Err` if the database connection fails
    pub async fn health_check(&self) -> Result<()> {
        // Execute a simple query to verify database connectivity
        sqlx::query("SELECT 1")
            .execute(self.pool())
            .await?;

        Ok(())
    }

    /// Broadcast a user cache invalidation message to other replicas.
    ///
    /// Best-effort: logs a warning on failure but does not propagate the error,
    /// since cache invalidation is not critical to the mutation itself.
    async fn notify_user_invalidation(&self, user_id: &UserId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service.invalidate_user(user_id).await {
                tracing::warn!(
                    error = %e,
                    user_id = %user_id.as_str(),
                    "Failed to broadcast user cache invalidation"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a test service with dummy JWT secret
    fn create_test_service() -> UserService {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();

        let jwt = JwtService::new("test-secret-for-user-service-long-enough-1234567890").unwrap();
        let username_cache = UsernameCache::new(None, "test:".to_string(), 10, 0);
        UserService::new(pool, jwt, username_cache, PasswordComplexityConfig::default())
    }

    #[tokio::test]
    async fn test_validate_username() {
        let service = create_test_service();

        assert!(service.validate_username("abc").is_ok());
        assert!(service.validate_username("user123").is_ok());
        assert!(service.validate_username("user_name").is_ok());
        assert!(service.validate_username("user-name").is_ok());

        assert!(service.validate_username("ab").is_err()); // Too short
        assert!(service.validate_username(&"a".repeat(51)).is_err()); // Too long
        assert!(service.validate_username("user@name").is_err()); // Invalid char
    }

    #[tokio::test]
    async fn test_validate_password() {
        let service = create_test_service();

        // PasswordValidator requires: min 8 chars, uppercase, lowercase, digit
        assert!(service.validate_password("Password123").is_ok());
        assert!(service.validate_password("Pass123!").is_ok());

        assert!(service.validate_password("short").is_err()); // Too short
        assert!(service.validate_password("password123").is_err()); // No uppercase
        assert!(service.validate_password(&"a".repeat(129)).is_err()); // Too long
    }

    // ========== Username Validation Edge Cases ==========

    #[tokio::test]
    async fn test_validate_username_empty() {
        let service = create_test_service();
        assert!(service.validate_username("").is_err());
    }

    #[tokio::test]
    async fn test_validate_username_exact_min_length() {
        let service = create_test_service();
        assert!(service.validate_username("abc").is_ok());
    }

    #[tokio::test]
    async fn test_validate_username_exact_max_length() {
        let service = create_test_service();
        assert!(service.validate_username(&"a".repeat(50)).is_ok());
    }

    #[tokio::test]
    async fn test_validate_username_starts_with_underscore() {
        let service = create_test_service();
        assert!(service.validate_username("_username").is_err());
    }

    #[tokio::test]
    async fn test_validate_username_starts_with_hyphen() {
        let service = create_test_service();
        assert!(service.validate_username("-username").is_err());
    }

    #[tokio::test]
    async fn test_validate_username_special_chars() {
        let service = create_test_service();
        assert!(service.validate_username("user@name").is_err());
        assert!(service.validate_username("user name").is_err());
        assert!(service.validate_username("user.name").is_err());
        assert!(service.validate_username("user!name").is_err());
    }

    #[tokio::test]
    async fn test_validate_username_alphanumeric_with_underscores_hyphens() {
        let service = create_test_service();
        assert!(service.validate_username("user_name-123").is_ok());
        assert!(service.validate_username("User123").is_ok());
        assert!(service.validate_username("a-b-c").is_ok());
        assert!(service.validate_username("a_b_c").is_ok());
    }

    // ========== Email Validation ==========

    #[tokio::test]
    async fn test_validate_email_valid() {
        let service = create_test_service();
        assert!(service.validate_email("user@example.com").is_ok());
        assert!(service.validate_email("user.name@example.co.uk").is_ok());
        assert!(service.validate_email("user+tag@example.com").is_ok());
    }

    #[tokio::test]
    async fn test_validate_email_invalid() {
        let service = create_test_service();
        assert!(service.validate_email("notanemail").is_err());
        assert!(service.validate_email("@example.com").is_err());
        assert!(service.validate_email("user@").is_err());
        assert!(service.validate_email("user@example").is_err());
    }

    #[tokio::test]
    async fn test_validate_email_empty() {
        let service = create_test_service();
        assert!(service.validate_email("").is_err());
    }

    #[tokio::test]
    async fn test_validate_email_whitespace_trimmed() {
        let service = create_test_service();
        assert!(service.validate_email("  user@example.com  ").is_ok());
    }

    #[tokio::test]
    async fn test_validate_email_only_whitespace() {
        let service = create_test_service();
        assert!(service.validate_email("   ").is_err());
    }

    // ========== Password Validation Edge Cases ==========

    #[tokio::test]
    async fn test_validate_password_empty() {
        let service = create_test_service();
        assert!(service.validate_password("").is_err());
    }

    #[tokio::test]
    async fn test_validate_password_no_lowercase() {
        let service = create_test_service();
        assert!(service.validate_password("PASSWORD123").is_err());
    }

    #[tokio::test]
    async fn test_validate_password_no_digit() {
        let service = create_test_service();
        assert!(service.validate_password("Passworddd").is_err());
    }

    #[tokio::test]
    async fn test_validate_password_exact_min_length() {
        let service = create_test_service();
        assert!(service.validate_password("Abcdefg1").is_ok());
    }

    #[tokio::test]
    async fn test_validate_password_one_below_min() {
        let service = create_test_service();
        assert!(service.validate_password("Abcdef1").is_err());
    }

    #[tokio::test]
    async fn test_validate_password_exact_max_length() {
        let service = create_test_service();
        // Build a 128-char password that satisfies complexity: uppercase, lowercase, digit, no long repeats
        let pwd = "Ab1".repeat(42) + "Ab";
        assert_eq!(pwd.len(), 128);
        assert!(service.validate_password(&pwd).is_ok());
    }

    #[tokio::test]
    async fn test_validate_password_over_max_length() {
        let service = create_test_service();
        let pwd = "Ab1".repeat(43);
        assert_eq!(pwd.len(), 129);
        assert!(service.validate_password(&pwd).is_err());
    }

    // ========== Error Type Validation ==========

    #[tokio::test]
    async fn test_validate_username_returns_invalid_input_error() {
        let service = create_test_service();
        let err = service.validate_username("ab").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_validate_email_returns_invalid_input_error() {
        let service = create_test_service();
        let err = service.validate_email("notanemail").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[tokio::test]
    async fn test_validate_password_returns_invalid_input_error() {
        let service = create_test_service();
        let err = service.validate_password("short").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    // ========== Integration Tests ==========

}
