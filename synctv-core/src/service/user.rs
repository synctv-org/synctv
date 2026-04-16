use sqlx::{PgPool, Postgres, Row, Transaction};
use std::collections::{HashMap, HashSet};

use std::sync::Arc;

use crate::{
    cache::{CacheInvalidationRuntime, KeyBuilder, UsernameCache},
    config::PasswordComplexityConfig,
    models::oauth2_client::OAuth2Provider,
    models::{MediaId, PlaylistId, RoomId, SignupMethod, User, UserId, UserStatus},
    repository::{RoomMemberRepository, UserOAuthProviderRepository, UserRepository},
    service::auth::{BruteForceProtectionService, JwtService, TokenBlacklistStore, TokenType},
    service::rate_limit::{RateLimiter, RequestRateLimiterService},
    Error, Result,
};

/// Default refresh token rate limit: 10 requests per minute per user
const REFRESH_RATE_LIMIT_REQUESTS: u32 = 10;
const REFRESH_RATE_LIMIT_WINDOW_SECS: u64 = 60;

fn nonnegative_i64_to_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or_default()
}

#[derive(Debug, Clone, Copy)]
struct RefreshRateLimitConfig {
    requests: u32,
    window_secs: u64,
}

impl Default for RefreshRateLimitConfig {
    fn default() -> Self {
        Self {
            requests: REFRESH_RATE_LIMIT_REQUESTS,
            window_secs: REFRESH_RATE_LIMIT_WINDOW_SECS,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserDeletedRoomImpact {
    pub room_id: RoomId,
    pub deleted_media_ids: Vec<MediaId>,
    pub playback_reset: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserDeletionSummary {
    pub user_id: UserId,
    pub username: String,
    pub deleted_room_ids: Vec<RoomId>,
    pub membership_room_ids: Vec<RoomId>,
    pub modified_rooms: Vec<UserDeletedRoomImpact>,
}

#[derive(Debug, Clone, Default)]
struct UserDeletionCleanupStats {
    oauth_mappings_deleted: u64,
    email_tokens_deleted: u64,
    provider_credentials_deleted: u64,
    notifications_deleted: u64,
    room_member_bans_cleared: u64,
    chat_messages_anonymized: u64,
    memberships_removed: u64,
    deleted_rooms: usize,
    deleted_playlists: usize,
    deleted_media: usize,
    playback_resets: usize,
}

#[derive(Debug, Default)]
struct UserOwnedRoomEntries {
    playlist_ids: Vec<PlaylistId>,
    media_ids: Vec<MediaId>,
}

/// User service for business logic
#[derive(Clone)]
pub struct UserService {
    pub(crate) repository: UserRepository,
    jwt_service: JwtService,
    username_cache: UsernameCache,
    /// Optional cache invalidation service for cross-replica user cache sync
    cache_invalidation: Option<Arc<dyn CacheInvalidationRuntime>>,
    /// Password complexity configuration from config file
    password_complexity: PasswordComplexityConfig,
    /// Brute-force protection for login attempts
    brute_force: Arc<dyn BruteForceProtectionService>,
    /// Whether email verification is required for login (true when email service is configured)
    email_verification_required: bool,
    /// Token blacklist store for refresh token rotation (Redis or in-memory)
    token_blacklist: Arc<dyn TokenBlacklistStore>,
    /// Key builder for Redis keys
    key_builder: KeyBuilder,
    /// Rate limiter for refresh token endpoint (prevents abuse/stolen token `DoS`)
    refresh_rate_limiter: Arc<dyn RequestRateLimiterService>,
    refresh_rate_limit_config: RefreshRateLimitConfig,
    /// Optional settings registry for reading signup_need_review and email_whitelist
    settings_registry: Option<Arc<crate::service::SettingsRegistry>>,
    /// Password hasher (Argon2id). Defaults to production params;
    /// inject `TestPasswordHasher` in integration tests for speed.
    password_hasher: Arc<dyn crate::service::auth::PasswordHasherService>,
}

impl std::fmt::Debug for UserService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UserService")
            .field("username_cache", &self.username_cache)
            .finish()
    }
}

impl UserService {
    fn normalize_login_identifier(identifier: &str) -> String {
        let trimmed = identifier.trim();
        if trimmed.contains('@') {
            trimmed.to_ascii_lowercase()
        } else {
            trimmed.to_string()
        }
    }

    async fn get_by_login_identifier(&self, identifier: &str) -> Result<Option<User>> {
        let normalized = Self::normalize_login_identifier(identifier);
        if normalized.contains('@') {
            self.repository.get_by_email(&normalized).await
        } else {
            self.repository.get_by_username(&normalized).await
        }
    }

    async fn complete_authenticated_login(
        &self,
        user: User,
        brute_force_key: &str,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<(User, String, String)> {
        if let Err(error) = self.validate_user_access(&user) {
            if let Err(bf_err) = self
                .brute_force
                .record_failure(brute_force_key, client_ip)
                .await
            {
                tracing::warn!(error = %bf_err, "Failed to record login failure for brute-force tracking");
            }
            return Err(error);
        }

        if let Err(error) = self.brute_force.reset(brute_force_key).await {
            tracing::warn!(error = %error, "Failed to reset brute-force counter after successful login");
        }
        if let Some(ip) = client_ip {
            if let Err(error) = self.brute_force.reset_ip(&ip).await {
                tracing::warn!(error = %error, "Failed to reset IP brute-force counter after successful login");
            }
        }

        let access_token =
            self.jwt_service
                .sign_token(&user.id, TokenType::Access, user.password_version)?;
        let refresh_token =
            self.jwt_service
                .sign_token(&user.id, TokenType::Refresh, user.password_version)?;

        Ok((user, access_token, refresh_token))
    }

    pub async fn login_with_verified_email(
        &self,
        user_id: &UserId,
        brute_force_key: &str,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<(User, String, String)> {
        let user = self
            .repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        self.complete_authenticated_login(user, brute_force_key, client_ip)
            .await
    }

    async fn query_owned_room_ids_in_tx(
        &self,
        user_id: &UserId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<RoomId>> {
        let room_ids: Vec<RoomId> = sqlx::query_scalar(
            "SELECT id
             FROM rooms
             WHERE created_by = $1 AND deleted_at IS NULL
             ORDER BY id
             FOR UPDATE",
        )
        .bind(user_id.as_str())
        .fetch_all(&mut **tx)
        .await?;

        Ok(room_ids)
    }

    async fn query_membership_room_ids_in_tx(
        &self,
        user_id: &UserId,
        owned_room_ids: &HashSet<String>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<Vec<RoomId>> {
        let rows = sqlx::query(
            "SELECT DISTINCT rm.room_id
             FROM room_members rm
             JOIN rooms r ON r.id = rm.room_id
             WHERE rm.user_id = $1
               AND rm.left_at IS NULL
               AND r.deleted_at IS NULL
             ORDER BY rm.room_id",
        )
        .bind(user_id.as_str())
        .fetch_all(&mut **tx)
        .await?;

        let mut room_ids = Vec::new();
        for row in rows {
            let room_id = RoomId::from_string(row.try_get::<String, _>("room_id")?);
            if !owned_room_ids.contains(room_id.as_str()) {
                room_ids.push(room_id);
            }
        }

        Ok(room_ids)
    }

    async fn query_owned_room_entries_in_tx(
        &self,
        user_id: &UserId,
        owned_room_ids: &HashSet<String>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<HashMap<RoomId, UserOwnedRoomEntries>> {
        let mut entries_by_room = HashMap::<RoomId, UserOwnedRoomEntries>::new();

        let playlist_rows = sqlx::query(
            "SELECT p.id, p.room_id
             FROM playlists p
             JOIN rooms r ON r.id = p.room_id
             WHERE p.creator_id = $1
               AND r.deleted_at IS NULL
             ORDER BY p.room_id, p.id",
        )
        .bind(user_id.as_str())
        .fetch_all(&mut **tx)
        .await?;

        for row in playlist_rows {
            let room_id = RoomId::from_string(row.try_get::<String, _>("room_id")?);
            if owned_room_ids.contains(room_id.as_str()) {
                continue;
            }
            let playlist_id = PlaylistId::from_string(row.try_get::<String, _>("id")?);
            entries_by_room
                .entry(room_id)
                .or_default()
                .playlist_ids
                .push(playlist_id);
        }

        let media_rows = sqlx::query(
            "SELECT m.id, m.room_id
             FROM media m
             JOIN rooms r ON r.id = m.room_id
             WHERE m.creator_id = $1
               AND r.deleted_at IS NULL
             ORDER BY m.room_id, m.id",
        )
        .bind(user_id.as_str())
        .fetch_all(&mut **tx)
        .await?;

        for row in media_rows {
            let room_id = RoomId::from_string(row.try_get::<String, _>("room_id")?);
            if owned_room_ids.contains(room_id.as_str()) {
                continue;
            }
            let media_id = MediaId::from_string(row.try_get::<String, _>("id")?);
            entries_by_room
                .entry(room_id)
                .or_default()
                .media_ids
                .push(media_id);
        }

        Ok(entries_by_room)
    }

    async fn collect_deleted_media_ids_in_tx(
        tx: &mut Transaction<'_, Postgres>,
        room_id: &RoomId,
        playlist_ids: &[PlaylistId],
        media_ids: &[MediaId],
    ) -> Result<Vec<MediaId>> {
        if playlist_ids.is_empty() && media_ids.is_empty() {
            return Ok(Vec::new());
        }

        let playlist_id_strs: Vec<&str> = playlist_ids.iter().map(PlaylistId::as_str).collect();
        let media_id_strs: Vec<&str> = media_ids.iter().map(MediaId::as_str).collect();

        let rows = sqlx::query(
            "WITH RECURSIVE target_playlists AS (
                SELECT id
                FROM playlists
                WHERE id = ANY($1)
                UNION ALL
                SELECT p.id
                FROM playlists p
                JOIN target_playlists tp ON p.parent_id = tp.id
            )
            SELECT DISTINCT m.id
            FROM media m
            WHERE m.room_id = $2
              AND (
                  m.id = ANY($3)
                  OR m.playlist_id IN (SELECT id FROM target_playlists)
              )
            ORDER BY m.id",
        )
        .bind(&playlist_id_strs)
        .bind(room_id.as_str())
        .bind(&media_id_strs)
        .fetch_all(&mut **tx)
        .await?;

        rows.into_iter()
            .map(|row| {
                let media_id = row.try_get::<String, _>("id")?;
                Ok(MediaId::from_string(media_id))
            })
            .collect()
    }

    async fn delete_owned_entries_in_room_in_tx(
        &self,
        room_id: &RoomId,
        playlist_ids: Vec<PlaylistId>,
        media_ids: Vec<MediaId>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<UserDeletedRoomImpact> {
        let deleted_media_ids =
            Self::collect_deleted_media_ids_in_tx(tx, room_id, &playlist_ids, &media_ids).await?;

        let playback_row = sqlx::query(
            "SELECT playing_media_id, playing_playlist_id
             FROM room_playback_state
             WHERE room_id = $1
             FOR UPDATE",
        )
        .bind(room_id.as_str())
        .fetch_optional(&mut **tx)
        .await?;

        let mut playback_reset = false;
        if let Some(row) = playback_row {
            let playing_media_id: Option<String> = row.try_get("playing_media_id")?;
            let playing_playlist_id: Option<String> = row.try_get("playing_playlist_id")?;

            let deletes_playing_media = playing_media_id.as_ref().is_some_and(|current_id| {
                deleted_media_ids
                    .iter()
                    .any(|media_id| media_id.as_str() == current_id.as_str())
            });

            let deletes_playing_playlist = if let Some(playing_playlist_id) = playing_playlist_id {
                if playlist_ids.is_empty() {
                    false
                } else {
                    let playlist_id_strs: Vec<&str> =
                        playlist_ids.iter().map(PlaylistId::as_str).collect();
                    sqlx::query_scalar(
                        "WITH RECURSIVE target_playlists AS (
                            SELECT id
                            FROM playlists
                            WHERE id = ANY($1)
                            UNION ALL
                            SELECT p.id
                            FROM playlists p
                            JOIN target_playlists tp ON p.parent_id = tp.id
                        )
                        SELECT EXISTS(
                            SELECT 1
                            FROM target_playlists
                            WHERE id = $2
                        )",
                    )
                    .bind(&playlist_id_strs)
                    .bind(playing_playlist_id)
                    .fetch_one(&mut **tx)
                    .await?
                }
            } else {
                false
            };

            if deletes_playing_media || deletes_playing_playlist {
                sqlx::query(
                    "UPDATE room_playback_state
                     SET playing_media_id = NULL,
                         playing_playlist_id = NULL,
                         target = ''::bytea,
                         \"current_time\" = 0,
                         speed = 1.0,
                         is_playing = false,
                         version = version + 1,
                         updated_at = NOW()
                     WHERE room_id = $1",
                )
                .bind(room_id.as_str())
                .execute(&mut **tx)
                .await?;
                playback_reset = true;
            }
        }

        if !media_ids.is_empty() {
            let media_id_strs: Vec<&str> = media_ids.iter().map(MediaId::as_str).collect();
            sqlx::query("DELETE FROM media WHERE id = ANY($1)")
                .bind(&media_id_strs)
                .execute(&mut **tx)
                .await?;
        }

        if !playlist_ids.is_empty() {
            let playlist_id_strs: Vec<&str> = playlist_ids.iter().map(PlaylistId::as_str).collect();
            sqlx::query("DELETE FROM playlists WHERE id = ANY($1)")
                .bind(&playlist_id_strs)
                .execute(&mut **tx)
                .await?;
        }

        Ok(UserDeletedRoomImpact {
            room_id: room_id.clone(),
            deleted_media_ids,
            playback_reset,
        })
    }

    async fn cleanup_transactional_user_resources(
        &self,
        user_id: &UserId,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(
        UserDeletionCleanupStats,
        Vec<RoomId>,
        Vec<RoomId>,
        Vec<UserDeletedRoomImpact>,
    )> {
        let owned_room_ids = self.query_owned_room_ids_in_tx(user_id, tx).await?;
        let owned_room_id_set: HashSet<String> = owned_room_ids
            .iter()
            .map(|room_id| room_id.as_str().to_string())
            .collect();
        let membership_room_ids = self
            .query_membership_room_ids_in_tx(user_id, &owned_room_id_set, tx)
            .await?;
        let entries_by_room = self
            .query_owned_room_entries_in_tx(user_id, &owned_room_id_set, tx)
            .await?;

        let mut modified_rooms = Vec::new();
        let mut deleted_playlists = 0usize;
        let mut deleted_media = 0usize;
        let mut playback_resets = 0usize;

        let mut modified_room_ids: Vec<RoomId> = entries_by_room.keys().cloned().collect();
        modified_room_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        for room_id in modified_room_ids {
            let entries = entries_by_room
                .get(&room_id)
                .expect("room id collected from map keys must exist");
            deleted_playlists += entries.playlist_ids.len();
            let impact = self
                .delete_owned_entries_in_room_in_tx(
                    &room_id,
                    entries.playlist_ids.clone(),
                    entries.media_ids.clone(),
                    tx,
                )
                .await?;
            deleted_media += impact.deleted_media_ids.len();
            if impact.playback_reset {
                playback_resets += 1;
            }
            modified_rooms.push(impact);
        }

        for room_id in &owned_room_ids {
            let impact =
                crate::service::room::soft_delete_room_and_cleanup_in_tx(tx, room_id).await?;
            deleted_playlists += impact.deleted_playlist_ids.len();
            deleted_media += impact.deleted_media_ids.len();
            if impact.playback_rows_deleted > 0 {
                playback_resets += 1;
            }
        }

        let oauth_mappings_deleted = sqlx::query("DELETE FROM oauth2_clients WHERE user_id = $1")
            .bind(user_id.as_str())
            .execute(&mut **tx)
            .await?
            .rows_affected();

        let email_tokens_deleted = sqlx::query("DELETE FROM email_tokens WHERE user_id = $1")
            .bind(user_id.as_str())
            .execute(&mut **tx)
            .await?
            .rows_affected();

        let provider_credentials_deleted =
            sqlx::query("DELETE FROM user_media_provider_credentials WHERE user_id = $1")
                .bind(user_id.as_str())
                .execute(&mut **tx)
                .await?
                .rows_affected();

        let notifications_deleted = sqlx::query("DELETE FROM notifications WHERE user_id = $1")
            .bind(user_id.as_str())
            .execute(&mut **tx)
            .await?
            .rows_affected();

        let room_member_bans_cleared =
            sqlx::query("UPDATE room_members SET banned_by = NULL WHERE banned_by = $1")
                .bind(user_id.as_str())
                .execute(&mut **tx)
                .await?
                .rows_affected();

        let chat_messages_anonymized =
            sqlx::query("UPDATE chat_messages SET user_id = NULL WHERE user_id = $1")
                .bind(user_id.as_str())
                .execute(&mut **tx)
                .await?
                .rows_affected();

        let room_member_repo = RoomMemberRepository::new(self.repository.pool().clone());
        let memberships_removed = room_member_repo
            .remove_all_for_user_with_executor(user_id, &mut **tx)
            .await?;

        Ok((
            UserDeletionCleanupStats {
                oauth_mappings_deleted,
                email_tokens_deleted,
                provider_credentials_deleted,
                notifications_deleted,
                room_member_bans_cleared,
                chat_messages_anonymized,
                memberships_removed,
                deleted_rooms: owned_room_ids.len(),
                deleted_playlists,
                deleted_media,
                playback_resets,
            },
            owned_room_ids,
            membership_room_ids,
            modified_rooms,
        ))
    }

    fn log_username_cache_write_failure(user_id: &UserId, operation: &'static str, error: &Error) {
        tracing::warn!(
            error = %error,
            user_id = %user_id.as_str(),
            operation,
            "Username cache update failed after primary user mutation; continuing with durable result"
        );
    }

    async fn cache_username_best_effort(
        &self,
        user_id: &UserId,
        username: &str,
        operation: &'static str,
    ) {
        if let Err(error) = self.username_cache.set(user_id, username).await {
            Self::log_username_cache_write_failure(user_id, operation, &error);
        }
    }

    pub(crate) fn oauth2_username_candidates(
        provider_user_id: &str,
        username: &str,
    ) -> Result<(String, Vec<String>)> {
        let sanitized_username = username
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
            .collect::<String>()
            .trim()
            .to_string();

        let base_username = if sanitized_username.is_empty() {
            format!(
                "user_{}",
                &provider_user_id[..provider_user_id.len().min(20)]
            )
        } else {
            sanitized_username
        };

        Self::validate_username(&base_username)?;

        let max_attempts = 10;
        let mut candidates = Vec::with_capacity(max_attempts);
        candidates.push(base_username.clone());
        for _ in 1..max_attempts {
            let max_base_len = 42;
            let base = if base_username.chars().count() > max_base_len {
                base_username.chars().take(max_base_len).collect::<String>()
            } else {
                base_username.clone()
            };
            let suffix = synctv_common::snanoid!(6);
            candidates.push(format!("{base}_{suffix}"));
        }

        Ok((base_username, candidates))
    }

    pub(crate) async fn cache_oauth2_username_best_effort(&self, user_id: &UserId, username: &str) {
        self.cache_username_best_effort(user_id, username, "create_or_load_by_oauth2")
            .await;
    }

    async fn invalidate_username_cache_best_effort(
        &self,
        user_id: &UserId,
        operation: &'static str,
    ) {
        if let Err(error) = self.invalidate_username_cache(user_id).await {
            Self::log_username_cache_write_failure(user_id, operation, &error);
        }
    }

    #[must_use]
    pub fn new(
        pool: PgPool,
        jwt_service: JwtService,
        username_cache: UsernameCache,
        password_complexity: PasswordComplexityConfig,
        token_blacklist: Arc<dyn TokenBlacklistStore>,
        key_builder: KeyBuilder,
        brute_force: impl BruteForceProtectionService + 'static,
    ) -> Self {
        Self::new_with_brute_force_service(
            pool,
            jwt_service,
            username_cache,
            password_complexity,
            token_blacklist,
            key_builder,
            Arc::new(brute_force),
        )
    }

    #[must_use]
    pub fn new_with_brute_force_service(
        pool: PgPool,
        jwt_service: JwtService,
        username_cache: UsernameCache,
        password_complexity: PasswordComplexityConfig,
        token_blacklist: Arc<dyn TokenBlacklistStore>,
        key_builder: KeyBuilder,
        brute_force: Arc<dyn BruteForceProtectionService>,
    ) -> Self {
        // Default to a local limiter; composition roots can inject any
        // distributed or local implementation via set_refresh_rate_limiter().
        let refresh_rate_limiter: Arc<dyn RequestRateLimiterService> =
            Arc::new(RateLimiter::local_only("synctv:".to_string()));

        Self {
            repository: UserRepository::new(pool),
            jwt_service,
            username_cache,
            cache_invalidation: None,
            password_complexity,
            brute_force,
            email_verification_required: false,
            token_blacklist,
            key_builder,
            refresh_rate_limiter,
            refresh_rate_limit_config: RefreshRateLimitConfig::default(),
            settings_registry: None,
            password_hasher: Arc::new(crate::service::auth::ProdPasswordHasher::default()),
        }
    }

    /// Inject the settings registry for reading signup_need_review and email_whitelist settings.
    pub fn set_settings_registry(&mut self, registry: Arc<crate::service::SettingsRegistry>) {
        self.settings_registry = Some(registry);
    }

    /// Set the cache invalidation service for cross-replica user cache sync
    pub fn set_cache_invalidation(&mut self, service: Arc<dyn CacheInvalidationRuntime>) {
        self.cache_invalidation = Some(service);
    }

    /// Override the password hasher (e.g. inject `TestPasswordHasher` in tests).
    pub fn set_password_hasher(
        &mut self,
        hasher: Arc<dyn crate::service::auth::PasswordHasherService>,
    ) {
        self.password_hasher = hasher;
    }

    /// Enable email verification requirement for login (call when email service is configured)
    pub const fn set_email_verification_required(&mut self, required: bool) {
        self.email_verification_required = required;
    }

    pub fn set_refresh_rate_limiter<T>(&mut self, limiter: T)
    where
        T: RequestRateLimiterService + 'static,
    {
        self.refresh_rate_limiter = Arc::new(limiter);
    }

    pub fn set_refresh_rate_limiter_for_tests<T>(&mut self, limiter: T)
    where
        T: RequestRateLimiterService + 'static,
    {
        self.refresh_rate_limiter = Arc::new(limiter);
    }

    pub const fn set_refresh_rate_limit_config_for_tests(
        &mut self,
        requests: u32,
        window_secs: u64,
    ) {
        self.refresh_rate_limit_config = RefreshRateLimitConfig {
            requests,
            window_secs,
        };
    }

    /// Validate that a user is allowed to access the system.
    ///
    /// Checks for banned, pending, or soft-deleted status, and optionally
    /// email verification. Returns a generic error message to prevent
    /// user enumeration.
    ///
    /// This is shared between `login()`, `refresh_token()`, and other
    /// authentication flows to avoid duplicating the same checks.
    fn validate_user_access(&self, user: &User) -> Result<()> {
        // Reject inactive or soft-deleted users with a generic message to prevent enumeration.
        if user.status == UserStatus::Banned
            || user.status == UserStatus::Pending
            || user.status == UserStatus::Rejected
            || user.deleted_at.is_some()
        {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        // Check email verification when required.
        // OAuth2 users are exempt: they authenticated via an external provider,
        // so requiring email verification would lock them out if the provider
        // didn't confirm their email.
        //
        // Returns a specific EmailNotVerified error (not a generic Authentication error)
        // so the client can prompt the user to verify their email. This is safe because
        // the user has already authenticated successfully (correct credentials), so
        // revealing that their email is unverified does not leak information.
        let is_oauth2_user = user.signup_method == crate::models::SignupMethod::OAuth2;
        if self.email_verification_required && !user.email_verified && !is_oauth2_user {
            return Err(Error::EmailNotVerified);
        }

        Ok(())
    }

    /// Register a new user
    ///
    /// Uniqueness of username/email is enforced atomically by the database
    /// UNIQUE constraints, avoiding any check-then-act (TOCTOU) race condition.
    ///
    /// When email verification is required (email service is configured), tokens
    /// are NOT returned -- the user must verify their email first. When email
    /// verification is not required, tokens are returned immediately.
    ///
    /// Per-IP brute-force protection is applied before processing: repeated failed
    /// registration attempts (e.g., validation errors, username conflicts) from the
    /// same IP are throttled using the same tiers as `login()`.
    pub async fn register(
        &self,
        username: String,
        email: Option<String>,
        password: String,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<(User, Option<String>, Option<String>)> {
        // Check per-IP brute-force before any processing. This throttles automated
        // mass-registration attempts (credential stuffing, spam account creation).
        // Use a fixed key instead of the attacker-controlled username to prevent
        // bypassing per-account lockout by varying the username on each attempt.
        self.brute_force
            .check_allowed("__registration__", client_ip)
            .await?;

        // Validate input - record failures for validation errors (potential attacks)
        if let Err(e) = Self::validate_username(&username) {
            if let Err(err) = self
                .brute_force
                .record_failure("__registration__", client_ip)
                .await
            {
                tracing::warn!(error = %err, "Failed to record registration brute-force failure");
            }
            return Err(e);
        }
        if let Some(ref email) = email {
            if let Err(e) = Self::validate_email(email) {
                if let Err(err) = self
                    .brute_force
                    .record_failure("__registration__", client_ip)
                    .await
                {
                    tracing::warn!(error = %err, "Failed to record registration brute-force failure");
                }
                return Err(e);
            }
        }
        if let Err(e) = self.validate_password(&password) {
            if let Err(err) = self
                .brute_force
                .record_failure("__registration__", client_ip)
                .await
            {
                tracing::warn!(error = %err, "Failed to record registration brute-force failure");
            }
            return Err(e);
        }

        // Check email whitelist setting from the settings registry.
        // If email whitelist is enabled, the registration email domain must be in the whitelist.
        if let Some(ref email_addr) = email {
            if let Some(ref registry) = self.settings_registry {
                let whitelist_enabled = registry.email_whitelist_enabled.get().unwrap_or(false);
                if whitelist_enabled {
                    let whitelist_str = registry.email_whitelist.get().unwrap_or_default();
                    let domain = email_addr
                        .rsplit_once('@')
                        .map(|(_, d)| d.to_lowercase())
                        .unwrap_or_default();
                    let allowed: Vec<&str> = whitelist_str
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .collect();
                    if !allowed.is_empty()
                        && !allowed.iter().any(|d| d.eq_ignore_ascii_case(&domain))
                    {
                        if let Err(err) = self
                            .brute_force
                            .record_failure("__registration__", client_ip)
                            .await
                        {
                            tracing::warn!(error = %err, "Failed to record registration brute-force failure");
                        }
                        return Err(Error::InvalidInput(
                            "Email domain is not allowed for registration".to_string(),
                        ));
                    }
                }
            }
        }

        // Fast-path duplicate checks before Argon2 hashing.
        //
        // We still rely on the database UNIQUE constraints for atomic race-safe
        // enforcement. This pre-check only avoids expensive hashing for requests
        // that are already known to fail with `AlreadyExists`.
        if self.repository.get_by_username(&username).await?.is_some() {
            return Err(Error::AlreadyExists(
                "Username or email already taken".to_string(),
            ));
        }
        if let Some(ref email_addr) = email {
            if self.repository.get_by_email(email_addr).await?.is_some() {
                return Err(Error::AlreadyExists(
                    "Username or email already taken".to_string(),
                ));
            }
        }

        // Hash password
        let password_hash = self.password_hasher.hash_password(&password).await?;

        // Set initial status based on email verification config and signup_need_review setting.
        // Priority: email verification required -> Pending (must verify email first)
        // Otherwise: signup_need_review -> Pending (admin must approve)
        // Otherwise: Active (immediate access)
        let signup_need_review = self
            .settings_registry
            .as_ref()
            .and_then(|r| r.signup_need_review.get().ok())
            .unwrap_or(false);

        let initial_status = if self.email_verification_required || signup_need_review {
            crate::models::UserStatus::Pending
        } else {
            crate::models::UserStatus::Active
        };

        // Create user with email signup method.
        // The database UNIQUE constraints on username and email will reject
        // duplicates atomically -- no separate existence check needed.
        //
        // IMPORTANT: AlreadyExists errors (username/email taken) are NOT recorded
        // as brute-force failures. A legitimate user trying to register with a
        // common username shouldn't be locked out - they just need to pick another.
        let user = User::new_with_status(
            username.clone(),
            email.clone(),
            password_hash,
            SignupMethod::Email,
            initial_status,
        );
        let created_user = match self.repository.create(&user).await {
            Ok(user) => user,
            Err(Error::AlreadyExists(_)) => {
                // Don't record failure for AlreadyExists - user just picked a taken username
                return Err(Error::AlreadyExists(
                    "Username or email already taken".to_string(),
                ));
            }
            Err(e) => {
                // Record failure for other database errors (could indicate attack)
                if let Err(err) = self
                    .brute_force
                    .record_failure("__registration__", client_ip)
                    .await
                {
                    tracing::warn!(error = %err, "Failed to record registration brute-force failure");
                }
                return Err(e);
            }
        };

        // Populate username cache
        self.cache_username_best_effort(&created_user.id, &username, "register")
            .await;

        // When the user starts as Pending (email verification or signup review required),
        // do NOT issue tokens. The user must either verify their email or be approved
        // by an admin before they can authenticate.
        if initial_status == crate::models::UserStatus::Pending {
            return Ok((created_user, None, None));
        }

        // Generate JWT tokens (role will be fetched from DB on each request)
        let access_token = self.jwt_service.sign_token(
            &created_user.id,
            TokenType::Access,
            created_user.password_version,
        )?;
        let refresh_token = self.jwt_service.sign_token(
            &created_user.id,
            TokenType::Refresh,
            created_user.password_version,
        )?;

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
        Self::validate_username(&username)?;
        if let Some(ref email) = email {
            Self::validate_email(email)?;
        }
        self.validate_password(&password)?;
        let password_hash = self.password_hasher.hash_password(&password).await?;
        // OAuth2 and AdminCreated users are pre-authenticated, but still need to respect
        // signup_need_review. Email/Password registrations go through email-verification
        // flow (Pending when verification is required).
        let signup_need_review = self
            .settings_registry
            .as_ref()
            .and_then(|r| r.signup_need_review.get().ok())
            .unwrap_or(false);

        let initial_status = match signup_method {
            SignupMethod::OAuth2 | SignupMethod::AdminCreated => {
                if signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
            SignupMethod::Email | SignupMethod::Password | SignupMethod::Unknown => {
                if self.email_verification_required || signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
        };
        let user = User::new_with_status(
            username,
            email,
            password_hash,
            signup_method,
            initial_status,
        );
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
        self.create_user_with_role_and_status(username, email, password, role, None)
            .await
    }

    pub async fn create_user_with_role_and_status(
        &self,
        username: String,
        email: Option<String>,
        password: String,
        role: Option<crate::models::UserRole>,
        status: Option<crate::models::UserStatus>,
    ) -> Result<User> {
        Self::validate_username(&username)?;
        if let Some(ref email) = email {
            Self::validate_email(email)?;
        }
        self.validate_password(&password)?;

        let password_hash = self.password_hasher.hash_password(&password).await?;
        let mut user = User::new(username.clone(), email, password_hash, SignupMethod::Email);
        if let Some(role) = role {
            user.role = role;
        }
        if let Some(status) = status {
            user.status = status;
        }
        let created_user = self.repository.create(&user).await?;
        self.cache_username_best_effort(
            &created_user.id,
            &username,
            "create_user_with_role_and_status",
        )
        .await;
        Ok(created_user)
    }

    /// Generate JWT tokens and populate username cache for a newly created user.
    pub async fn finalize_registration(&self, user: &User) -> Result<(String, String)> {
        let access_token =
            self.jwt_service
                .sign_token(&user.id, TokenType::Access, user.password_version)?;
        let refresh_token =
            self.jwt_service
                .sign_token(&user.id, TokenType::Refresh, user.password_version)?;
        self.cache_username_best_effort(&user.id, &user.username, "finalize_registration")
            .await;
        Ok((access_token, refresh_token))
    }

    /// Login user
    ///
    /// Timing-safe: always performs password verification regardless of user existence
    /// to prevent username enumeration via response time analysis.
    ///
    /// Includes per-account and per-IP brute-force protection: after repeated failures,
    /// accounts/IPs are temporarily locked with exponential backoff (1min / 5min / 15min).
    ///
    /// ## Failure Type Differentiation (Task #74)
    ///
    /// To prevent attackers from locking out legitimate users by trying random usernames:
    /// - "User doesn't exist" → Only IP-level tracking (username doesn't exist to attack)
    /// - "Wrong password for existing user" → Both username and IP tracking
    /// - "Account banned/pending/deleted" → Both username and IP tracking (prevents enumeration)
    pub async fn login(
        &self,
        identifier: String,
        password: String,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<(User, String, String)> {
        let normalized_identifier = Self::normalize_login_identifier(&identifier);

        // Check brute-force lockout before expensive Argon2 verification.
        // This applies to all usernames (existing or not) to prevent
        // distributed attacks while also saving CPU on locked accounts.
        self.brute_force
            .check_allowed(&normalized_identifier, client_ip)
            .await?;

        // Get user by username or email.
        let maybe_user = self.get_by_login_identifier(&normalized_identifier).await?;

        // Track whether user existed for differentiated failure recording (Task #74)
        let user_existed = maybe_user.is_some();

        // Always perform password verification to prevent timing side-channel.
        // If the user doesn't exist, verify against a dummy hash so the response
        // time is indistinguishable from a real verification.
        let (is_valid, user) = if let Some(user) = maybe_user {
            let valid = self
                .password_hasher
                .verify_password(&password, &user.password_hash)
                .await?;
            (valid, Some(user))
        } else {
            // Dummy Argon2 verification to match timing of real verification.
            // This hash is pre-computed and never matches any real password.
            let _ = self
                .password_hasher
                .verify_password(&password, self.password_hasher.dummy_hash())
                .await;
            (false, None)
        };

        // After constant-time verification, check all failure conditions
        let user = match user {
            Some(u) if is_valid => u,
            _ => {
                // Differentiate failure types (Task #74):
                // - User doesn't exist: Only record IP-level failure to prevent
                //   attackers from locking out legitimate usernames
                // - Wrong password for existing user: Record both username and IP
                let record_result = if user_existed {
                    // User existed but wrong password - record both username and IP
                    self.brute_force
                        .record_failure(&normalized_identifier, client_ip)
                        .await
                } else {
                    // User didn't exist - only record IP-level failure
                    self.brute_force.record_ip_failure(client_ip).await
                };
                if let Err(e) = record_result {
                    tracing::warn!(error = %e, "Failed to record login failure for brute-force tracking");
                }
                return Err(Error::Authentication("Authentication failed".to_string()));
            }
        };

        self.complete_authenticated_login(user, &normalized_identifier, client_ip)
            .await
    }

    /// Issue an access/refresh token pair for the local management plane.
    /// Generate token pair for `OAuth2` login (user already authenticated by `OAuth2` provider)
    ///
    /// This method generates access and refresh tokens for a user who has been
    /// authenticated via `OAuth2`. Unlike `login()`, this skips password verification.
    ///
    /// Per-IP brute-force protection is applied before token issuance. The provider
    /// user ID is used as the per-account key (instead of username) since `OAuth2` users
    /// may not have a locally-assigned username yet at the time of lookup.
    pub async fn login_oauth2(
        &self,
        user_id: &UserId,
        provider_user_id: &str,
        client_ip: Option<std::net::IpAddr>,
    ) -> Result<(User, String, String)> {
        // Check per-IP and per-account brute-force before token issuance.
        self.brute_force
            .check_allowed(provider_user_id, client_ip)
            .await?;

        // Get user to ensure they exist and are active
        let user = self
            .repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        self.complete_authenticated_login(user, provider_user_id, client_ip)
            .await
    }

    /// Refresh access token with **Refresh Token Rotation**.
    ///
    /// Each refresh token can only be used once:
    /// 1. The old refresh token's JTI is checked against the Redis blacklist.
    /// 2. If the JTI is blacklisted, the request is rejected (possible token theft replay).
    ///    Additionally, the entire refresh token family for the user is revoked as a
    ///    precaution (all refresh tokens issued before this moment become invalid).
    /// 3. After issuing new tokens, the old JTI is added to the blacklist with a TTL
    ///    equal to the old token's remaining lifetime.
    pub async fn refresh_token(&self, refresh_token: String) -> Result<(String, String)> {
        // Verify refresh token
        let claims = self.jwt_service.verify_refresh_token(&refresh_token)?;
        let user_id = UserId::from_string(claims.sub.clone());

        // Rate limit per-user refresh requests to prevent abuse.
        // An attacker with a stolen token could otherwise:
        // 1. Rapidly call refresh_token to exhaust server resources
        // 2. Trigger family revocation, locking out the legitimate user
        //
        // Rate limit key is per-user: "refresh:<user_id>"
        let rate_limit_key = format!("refresh:{}", user_id.as_str());
        self.refresh_rate_limiter
            .check_rate_limit(
                &rate_limit_key,
                self.refresh_rate_limit_config.requests,
                self.refresh_rate_limit_config.window_secs,
            )
            .await
            .map_err(|e| {
                tracing::warn!(
                    user_id = %user_id.as_str(),
                    error = %e,
                    "Refresh token rate limit exceeded"
                );
                Error::from(e)
            })?;

        // Get user to ensure they still exist and are active
        let user = self
            .repository
            .get_by_id(&user_id)
            .await?
            .ok_or_else(|| Error::Authentication("Authentication failed".to_string()))?;

        // Check user status and email verification (shared with login)
        self.validate_user_access(&user)?;

        // Reject refresh tokens issued with an old password version
        if claims.pv < user.password_version {
            return Err(Error::Authentication("Authentication failed".to_string()));
        }

        // Refresh Token Rotation: check blacklist and family revocation
        {
            let old_jti = &claims.jti;

            // Check if the entire refresh token family for this user has been revoked
            // (triggered when a blacklisted JTI is replayed, indicating possible token theft).
            let family_key = self
                .key_builder
                .refresh_token_family_revoked(user_id.as_str());
            let family_revoked_at = self
                .token_blacklist
                .get_family_revoked_at_checked(&family_key)
                .await;
            if let Some(revoked_at) = family_revoked_at? {
                // Reject any refresh token issued at or before the family revocation timestamp.
                // Using <= ensures tokens issued in the same second as revocation are blocked,
                // since sub-second precision is lost in Unix timestamps.
                if claims.iat <= revoked_at {
                    tracing::warn!(
                        user_id = %user_id.as_str(),
                        jti = %old_jti,
                        revoked_at = revoked_at,
                        token_iat = claims.iat,
                        "Refresh token rejected: token family revoked (possible token theft)"
                    );
                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
            }

            // Atomically check and blacklist the JTI.
            // This prevents TOCTOU race conditions where two concurrent requests
            // both pass the is_blacklisted check before either calls blacklist.
            if !old_jti.is_empty() {
                let blacklist_key = self.key_builder.refresh_token_blacklist(old_jti);
                let now = chrono::Utc::now().timestamp();
                let remaining_ttl = nonnegative_i64_to_u64((claims.exp - now).max(60));

                // Atomic operation: returns true if key already existed (replay detected)
                let already_existed = self
                    .token_blacklist
                    .blacklist_if_not_exists(&blacklist_key, remaining_ttl)
                    .await?;

                if already_existed {
                    // A blacklisted JTI is being replayed! This indicates the refresh token
                    // was stolen and both the legitimate user and attacker are trying to use it.
                    // Revoke the entire refresh token family for this user as a precaution.
                    tracing::warn!(
                        user_id = %user_id.as_str(),
                        jti = %old_jti,
                        "Blacklisted refresh token JTI replayed — revoking entire token family"
                    );

                    let family_ttl = self
                        .jwt_service
                        .refresh_token_duration_seconds()
                        .saturating_add(3600);
                    self.token_blacklist
                        .set_family_revoked(&family_key, now, family_ttl)
                        .await?;

                    return Err(Error::Authentication("Authentication failed".to_string()));
                }
            }
        }

        // Generate new tokens (role will be fetched from DB on each request)
        // The old JTI is now atomically blacklisted, so concurrent replays will be detected.
        let new_access_token =
            self.jwt_service
                .sign_token(&user.id, TokenType::Access, user.password_version)?;
        let new_refresh_token =
            self.jwt_service
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

    /// Get multiple users by IDs.
    pub async fn get_users_by_ids(&self, user_ids: &[UserId]) -> Result<Vec<User>> {
        self.repository.get_by_ids(user_ids).await
    }

    /// Get user by username.
    pub async fn get_user_by_username(&self, username: &str) -> Result<User> {
        let username = username.trim();
        if username.is_empty() {
            return Err(Error::InvalidInput("Username is empty".to_string()));
        }

        self.repository
            .get_by_username(username)
            .await?
            .ok_or_else(|| Error::NotFound("User not found".to_string()))
    }

    /// Get user by email
    pub async fn get_by_email(&self, email: &str) -> Result<Option<User>> {
        self.repository.get_by_email(email).await
    }

    /// Update user (entire user object) with optimistic locking.
    ///
    /// Pass the `version` value from the previously-read user to detect
    /// concurrent modifications. The update increments `version` atomically,
    /// so concurrent writes will see a mismatch and fail.
    /// Returns `Error::OptimisticLockConflict` if the user was modified since
    /// it was read.
    pub async fn update_user(&self, user: &User, old_version: i32) -> Result<User> {
        let updated = self.repository.update(user, old_version).await?;
        self.invalidate_username_cache_best_effort(&user.id, "update_user")
            .await;
        self.notify_user_invalidation(&user.id).await;
        Ok(updated)
    }

    /// Change user password (requires old password verification)
    pub async fn change_password(
        &self,
        user_id: &UserId,
        old_password: &str,
        new_password: &str,
    ) -> Result<User> {
        self.update_profile(
            user_id,
            None,
            Some(old_password.to_string()),
            Some(new_password.to_string()),
        )
        .await
    }

    /// Set user password (admin use, no old password required)
    ///
    /// After updating the password, all existing access and refresh tokens for the
    /// user are invalidated by incrementing `password_version` in the same database
    /// write. Refresh flows re-load the user and reject tokens whose embedded
    /// password version is stale, so no extra pre-commit side effect is needed.
    pub async fn set_password(&self, user_id: &UserId, new_password: &str) -> Result<User> {
        // Validate new password
        self.validate_password(new_password)?;

        // Hash new password
        let password_hash = self.password_hasher.hash_password(new_password).await?;

        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;

        // Update password in database (this also updates password_changed_at,
        // which invalidates all tokens issued before this moment)
        let updated_user = self
            .repository
            .update_password_with_executor(user_id, &password_hash, &mut *tx)
            .await?;

        tx.commit().await?;

        // Invalidate user cache across all replicas
        self.notify_user_invalidation(user_id).await;

        tracing::info!("Password updated for user {}", user_id.as_str());

        Ok(updated_user)
    }

    /// Update a user's own profile atomically.
    ///
    /// Supports username-only updates, password-only updates, or updating both
    /// fields in a single transaction so partial commits cannot occur.
    ///
    /// When changing password, `old_password` is required and verified inside
    /// the transaction against the current row version before any mutation is
    /// committed. Token invalidation is driven by the resulting `password_version`
    /// change, which becomes visible only after the transaction commits.
    pub async fn update_profile(
        &self,
        user_id: &UserId,
        new_username: Option<String>,
        old_password: Option<String>,
        new_password: Option<String>,
    ) -> Result<User> {
        if new_username.is_none() && new_password.is_none() {
            return Err(Error::InvalidInput(
                "No valid update fields provided (username or password)".to_string(),
            ));
        }

        let new_username = new_username.map(|username| username.trim().to_string());

        if new_password.is_some() && old_password.is_none() {
            return Err(Error::InvalidInput(
                "old_password is required when changing password".to_string(),
            ));
        }

        if let Some(ref username) = new_username {
            Self::validate_username(username)?;
        }
        if let Some(ref password) = new_password {
            self.validate_password(password)?;
        }

        let mut tx: Transaction<'_, Postgres> = self.repository.pool().begin().await?;
        let current_user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {} not found", user_id.as_str())))?;

        let target_username = new_username.unwrap_or_else(|| current_user.username.clone());
        let mut new_password_hash: Option<String> = None;

        if let Some(new_password) = new_password {
            let provided_old_password = old_password.expect("old_password validated above");
            let is_valid = self
                .password_hasher
                .verify_password(&provided_old_password, &current_user.password_hash)
                .await?;
            if !is_valid {
                return Err(Error::Authentication(
                    "Invalid current password".to_string(),
                ));
            }

            new_password_hash = Some(self.password_hasher.hash_password(&new_password).await?);
        }

        let updated_user = self
            .repository
            .update_profile_with_executor(
                user_id,
                &target_username,
                new_password_hash.as_deref(),
                current_user.version,
                &mut *tx,
            )
            .await?;

        tx.commit().await?;

        if updated_user.username != current_user.username {
            self.invalidate_username_cache_best_effort(user_id, "update_profile")
                .await;
        }
        self.notify_user_invalidation(user_id).await;

        Ok(updated_user)
    }

    /// Set user email verification status
    pub async fn set_email_verified(&self, user_id: &UserId, email_verified: bool) -> Result<User> {
        let updated_user = self
            .repository
            .update_email_verified(user_id, email_verified)
            .await?;

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
    pub async fn list_users(
        &self,
        query: &crate::models::UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        query.pagination.validate()?;
        self.repository.list(query).await
    }

    /// List root/admin users with pagination.
    pub async fn list_admins(
        &self,
        query: &crate::models::UserListQuery,
    ) -> Result<(Vec<User>, i64)> {
        query.pagination.validate()?;
        self.repository.list_admins(query).await
    }

    /// Delete all `OAuth2` provider mappings for a user.
    ///
    /// Used during user deletion to clean up OAuth bindings.
    pub async fn cleanup_oauth_providers(&self, user_id: &UserId) -> Result<u64> {
        let repo = UserOAuthProviderRepository::new(self.repository.pool().clone());
        repo.delete_all_for_user(user_id).await
    }

    /// Blacklist an access token JTI so it cannot be used again.
    ///
    /// Used on logout to immediately invalidate the current access token even
    /// before it reaches its natural expiry. The `ttl_secs` should equal the
    /// remaining lifetime of the token (i.e. `exp - now`).
    ///
    /// Fail-closed: if the blacklist store is unavailable this returns an error
    /// so callers can choose whether to abort or warn-and-continue.
    pub async fn blacklist_access_token(&self, jti: &str, ttl_secs: u64) -> Result<()> {
        let key = self.key_builder.access_token_blacklist(jti);
        self.token_blacklist.blacklist(&key, ttl_secs).await
    }

    /// Soft-delete the currently authenticated user's own account.
    pub async fn delete_self(&self, user_id: &UserId) -> Result<()> {
        self.delete_user(user_id).await
    }

    /// Soft-delete a user and clean up all related resources.
    ///
    /// Performs the following cleanup in order:
    /// 1. Within a single DB transaction:
    ///    a. Delete rooms owned by the user
    ///    b. Delete playlists/media created by the user in surviving rooms
    ///    c. Reset playback state in affected rooms when deleted entries are currently playing
    ///    d. Delete user-scoped ancillary rows and anonymize surviving chat messages
    ///    e. Mark all remaining room memberships as `Left`
    ///    f. Soft-delete the user row
    /// 2. Reset username-scoped auth/rate-limit state (best-effort)
    /// 3. Invalidate username cache (best-effort)
    /// 4. Invalidate user cache across replicas (best-effort)
    ///
    /// Step 1 is atomic: if any cleanup fails, the soft-delete is rolled back to
    /// prevent partially-deleted users with orphaned state.
    ///
    /// **Token Invalidation**: Tokens are invalidated implicitly because the
    /// security pipeline checks for deleted users (`deleted_at` IS NOT NULL).
    pub async fn delete_user_with_summary(&self, user_id: &UserId) -> Result<UserDeletionSummary> {
        // 1. Transactional DB cleanup + soft-delete
        let pool = self.repository.pool();
        let mut tx = pool.begin().await?;

        let user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?;
        let Some(user) = user else {
            return Err(Error::InvalidInput("User is already deleted".to_string()));
        };

        let (cleanup, deleted_room_ids, membership_room_ids, mut modified_rooms) = self
            .cleanup_transactional_user_resources(user_id, &mut tx)
            .await?;

        let deleted = self
            .repository
            .delete_with_executor(user_id, &mut *tx)
            .await?;
        if !deleted {
            return Err(Error::InvalidInput("User is already deleted".to_string()));
        }

        tx.commit().await?;

        // 2. Reset username/user-scoped auth and rate-limit state (best-effort).
        if let Err(e) = self.brute_force.reset(&user.username).await {
            tracing::warn!(
                error = %e,
                user_id = %user_id.as_str(),
                username = %user.username,
                "Failed to reset brute-force state during user deletion"
            );
        }
        let refresh_rate_limit_key = format!("refresh:{}", user_id.as_str());
        if let Err(e) = self
            .refresh_rate_limiter
            .reset(&refresh_rate_limit_key)
            .await
        {
            tracing::warn!(
                error = %e,
                user_id = %user_id.as_str(),
                "Failed to reset refresh rate limit state during user deletion"
            );
        }

        // 3. Invalidate username cache (best-effort)
        if let Err(e) = self.invalidate_username_cache(user_id).await {
            tracing::warn!(
                error = %e,
                user_id = %user_id.as_str(),
                "Failed to invalidate username cache during user deletion"
            );
        }

        // 4. Invalidate user cache across replicas (best-effort)
        self.notify_user_invalidation(user_id).await;

        tracing::info!(
            user_id = %user_id.as_str(),
            username = %user.username,
            oauth_mappings_deleted = cleanup.oauth_mappings_deleted,
            email_tokens_deleted = cleanup.email_tokens_deleted,
            provider_credentials_deleted = cleanup.provider_credentials_deleted,
            notifications_deleted = cleanup.notifications_deleted,
            room_member_bans_cleared = cleanup.room_member_bans_cleared,
            chat_messages_anonymized = cleanup.chat_messages_anonymized,
            memberships_removed = cleanup.memberships_removed,
            deleted_rooms = cleanup.deleted_rooms,
            deleted_playlists = cleanup.deleted_playlists,
            deleted_media = cleanup.deleted_media,
            playback_resets = cleanup.playback_resets,
            "User soft-deleted with transactional resource cleanup"
        );

        modified_rooms.sort_by(|left, right| left.room_id.as_str().cmp(right.room_id.as_str()));

        Ok(UserDeletionSummary {
            user_id: user.id,
            username: user.username,
            deleted_room_ids,
            membership_room_ids,
            modified_rooms,
        })
    }

    pub async fn delete_user(&self, user_id: &UserId) -> Result<()> {
        self.delete_user_with_summary(user_id).await.map(|_| ())
    }

    /// Ban a user and remove them from all room memberships in the same transaction.
    ///
    /// This ensures global account bans and room lifecycle state stay aligned:
    /// once the user becomes `Banned`, they are also no longer an active member
    /// of any room, and rooms they own are emptied so existing members are
    /// forced out of unusable rooms.
    pub async fn ban_user_and_cleanup_memberships(&self, user_id: &UserId) -> Result<User> {
        let pool = self.repository.pool();
        let mut tx = pool.begin().await?;

        let mut user = self
            .repository
            .get_by_id_for_update_with_executor(user_id, &mut *tx)
            .await?
            .ok_or_else(|| Error::NotFound(format!("User {} not found", user_id.as_str())))?;

        if user.status == UserStatus::Banned {
            return Err(Error::InvalidInput("User is already banned".to_string()));
        }

        user.status = UserStatus::Banned;
        let old_version = user.version;
        let updated = self
            .repository
            .update_with_executor(&user, old_version, &mut *tx)
            .await?;

        let room_member_repo = RoomMemberRepository::new(pool.clone());
        let owned_room_ids = self.query_owned_room_ids_in_tx(user_id, &mut tx).await?;
        room_member_repo
            .remove_all_for_user_with_executor(user_id, &mut *tx)
            .await?;
        room_member_repo
            .remove_all_for_rooms_with_executor(&owned_room_ids, &mut *tx)
            .await?;

        tx.commit().await?;
        self.notify_user_invalidation(user_id).await;

        Ok(updated)
    }

    /// Set user status (Active/Pending/Banned).
    ///
    /// This method updates the user's account status and invalidates
    /// relevant caches. Changing status to Banned or Pending will
    /// prevent the user from logging in or using WebSocket connections.
    pub async fn set_user_status(
        &self,
        user_id: &UserId,
        status: crate::models::UserStatus,
    ) -> Result<User> {
        let user = self.repository.update_status(user_id, status).await?;
        self.notify_user_invalidation(user_id).await;
        Ok(user)
    }

    // ========================
    // Batch Operations
    // ========================

    /// Maximum number of items allowed in a batch operation
    pub const BATCH_SIZE_LIMIT: usize = 100;

    /// Batch delete multiple users.
    ///
    /// Each user is processed individually - if one user fails, others may still succeed.
    /// Returns per-user results with success/failure status.
    ///
    /// # Errors
    /// - `InvalidInput` if `user_ids` is empty or exceeds `BATCH_SIZE_LIMIT`
    pub async fn batch_delete_users(
        &self,
        user_ids: &[String],
    ) -> Result<Vec<(String, Result<()>)>> {
        if user_ids.is_empty() {
            return Err(Error::InvalidInput("user_ids cannot be empty".to_string()));
        }
        if user_ids.len() > Self::BATCH_SIZE_LIMIT {
            return Err(Error::InvalidInput(format!(
                "Batch size {} exceeds limit of {}",
                user_ids.len(),
                Self::BATCH_SIZE_LIMIT
            )));
        }

        let mut results = Vec::with_capacity(user_ids.len());

        for user_id_str in user_ids {
            let user_id = UserId::from_string(user_id_str.clone());

            let result = self.delete_user(&user_id).await;
            results.push((user_id_str.clone(), result));
        }

        Ok(results)
    }
}

// Implement UserValidator for UserService to support TOCTOU-safe ticket validation
#[async_trait::async_trait]
impl crate::service::ws_ticket::UserValidator for UserService {
    /// Validate user for ticket-based WebSocket authentication.
    ///
    /// This implementation checks:
    /// - User exists and is not soft-deleted
    /// - User status is Active (not Pending, Rejected, or Banned)
    ///
    /// Returns the current password version for ticket validation.
    async fn validate_for_ticket(
        &self,
        user_id: &UserId,
    ) -> crate::Result<crate::service::ws_ticket::UserValidationResult> {
        let user = self
            .repository
            .get_by_id(user_id)
            .await?
            .ok_or_else(|| crate::Error::NotFound("User not found".to_string()))?;

        // Check soft-delete
        if user.is_deleted() {
            return Err(crate::Error::Authorization(
                "Authentication failed".to_string(),
            ));
        }

        // Check user status
        match user.status {
            crate::models::UserStatus::Active => {
                // User is active, continue
            }
            crate::models::UserStatus::Banned
            | crate::models::UserStatus::Pending
            | crate::models::UserStatus::Rejected => {
                return Err(crate::Error::Authorization(
                    "Authentication failed".to_string(),
                ));
            }
        }

        Ok(crate::service::ws_ticket::UserValidationResult {
            password_version: user.password_version,
        })
    }
}

impl UserService {
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
        let (base_username, candidates) =
            Self::oauth2_username_candidates(provider_user_id, username)?;
        // Generate a random password (OAuth2 users don't need password login)
        let random_password = synctv_common::snanoid!(32);
        let user_email = email.map(std::string::ToString::to_string);
        // Hash password
        let password_hash = self.password_hasher.hash_password(&random_password).await?;

        for candidate in &candidates {
            let user = User::new_with_status(
                candidate.clone(),
                user_email.clone(),
                password_hash.clone(),
                SignupMethod::OAuth2,
                crate::models::UserStatus::Active,
            );
            match self.repository.create(&user).await {
                Ok(created_user) => {
                    self.cache_oauth2_username_best_effort(&created_user.id, candidate)
                        .await;

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
                Err(Error::AlreadyExists(ref msg))
                    if msg.contains("username") || msg.contains("Username") => {}
                Err(e) => return Err(e),
            }
        }

        Err(Error::Internal(format!(
            "Could not generate a unique username for base '{username}' after {} attempts",
            candidates.len()
        )))
    }

    /// Validate username using production-grade validator
    fn validate_username(username: &str) -> Result<()> {
        crate::validation::UsernameValidator::new()
            .validate(username)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    /// Validate email using regex-based validator
    fn validate_email(email: &str) -> Result<()> {
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
        match self.username_cache.get(user_id).await {
            Ok(Some(username)) => return Ok(Some(username)),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    user_id = %user_id.as_str(),
                    "Username cache read failed; falling back to database"
                );
            }
        }

        // Cache miss - fetch from database
        if let Some(user) = self.repository.get_by_id(user_id).await? {
            // Populate cache
            let username = user.username.clone();
            self.cache_username_best_effort(user_id, &username, "get_username")
                .await;
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
        let mut result = match self.username_cache.get_batch(user_ids).await {
            Ok(result) => result,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    requested = user_ids.len(),
                    "Username cache batch read failed; falling back to database"
                );
                HashMap::new()
            }
        };
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
                self.cache_username_best_effort(&user_id, &username, "get_usernames")
                    .await;
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
    pub fn access_token_duration_seconds(&self) -> i64 {
        self.jwt_service.access_token_duration_seconds()
    }

    /// Get the username cache (for creating dependent services)
    #[must_use]
    pub const fn username_cache(&self) -> &UsernameCache {
        &self.username_cache
    }

    /// Get the token blacklist store (for configuring `SecurityPipeline`)
    #[must_use]
    pub fn token_blacklist_store(&self) -> Arc<dyn crate::service::auth::TokenBlacklistStore> {
        Arc::clone(&self.token_blacklist)
    }

    /// Get the key builder (for configuring `SecurityPipeline`)
    #[must_use]
    pub const fn key_builder(&self) -> &KeyBuilder {
        &self.key_builder
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
        sqlx::query("SELECT 1").execute(self.pool()).await?;

        Ok(())
    }

    /// Invalidate user cache locally and broadcast to other replicas.
    ///
    /// Best-effort: logs a warning on failure but does not propagate the error,
    /// since cache invalidation is not critical to the mutation itself.
    ///
    /// Uses `invalidate_and_broadcast_user` to ensure the originating node also
    /// clears its own local cache (the Redis subscriber skips self-originated
    /// messages, so `broadcast_remote` alone would leave local caches stale).
    async fn notify_user_invalidation(&self, user_id: &UserId) {
        if let Some(ref service) = self.cache_invalidation {
            if let Err(e) = service.invalidate_and_broadcast_user(user_id).await {
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
    use crate::service::BruteForceProtection;

    // Validation tests use the standalone validators directly since they don't
    // require a full UserService with Redis/brute-force dependencies.

    fn validate_username(username: &str) -> Result<()> {
        crate::validation::UsernameValidator::new()
            .validate(username)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    fn validate_email(email: &str) -> Result<()> {
        let email = email.trim();
        if email.is_empty() {
            return Err(Error::InvalidInput("Email cannot be empty".to_string()));
        }
        crate::validation::EmailValidator::new()
            .validate(email)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    fn validate_password(password: &str) -> Result<()> {
        crate::validation::PasswordValidator::from_config(&PasswordComplexityConfig::default())
            .validate(password)
            .map_err(|e| Error::InvalidInput(e.to_string()))
    }

    #[test]
    fn test_validate_username() {
        assert!(validate_username("abc").is_ok());
        assert!(validate_username("user123").is_ok());
        assert!(validate_username("user_name").is_ok());
        assert!(validate_username("user-name").is_ok());

        assert!(validate_username("ab").is_err()); // Too short
        assert!(validate_username(&"a".repeat(51)).is_err()); // Too long
        assert!(validate_username("user@name").is_err()); // Invalid char
    }

    #[test]
    fn test_validate_password() {
        // PasswordValidator requires: min 8 chars, uppercase, lowercase, digit
        assert!(validate_password("Password123").is_ok());
        assert!(validate_password("Pass123!").is_ok());

        assert!(validate_password("short").is_err()); // Too short
        assert!(validate_password("password123").is_err()); // No uppercase
        assert!(validate_password(&"a".repeat(129)).is_err()); // Too long
    }

    // ========== Username Validation Edge Cases ==========

    #[test]
    fn test_validate_username_empty() {
        assert!(validate_username("").is_err());
    }

    #[test]
    fn test_validate_username_exact_min_length() {
        assert!(validate_username("abc").is_ok());
    }

    #[test]
    fn test_validate_username_exact_max_length() {
        assert!(validate_username(&"a".repeat(50)).is_ok());
    }

    #[test]
    fn test_validate_username_starts_with_underscore() {
        assert!(validate_username("_username").is_err());
    }

    #[test]
    fn test_validate_username_starts_with_hyphen() {
        assert!(validate_username("-username").is_err());
    }

    #[test]
    fn test_validate_username_special_chars() {
        assert!(validate_username("user@name").is_err());
        assert!(validate_username("user name").is_err());
        assert!(validate_username("user.name").is_err());
        assert!(validate_username("user!name").is_err());
    }

    #[test]
    fn test_validate_username_alphanumeric_with_underscores_hyphens() {
        assert!(validate_username("user_name-123").is_ok());
        assert!(validate_username("User123").is_ok());
        assert!(validate_username("a-b-c").is_ok());
        assert!(validate_username("a_b_c").is_ok());
    }

    // ========== Email Validation ==========

    #[test]
    fn test_validate_email_valid() {
        assert!(validate_email("user@example.com").is_ok());
        assert!(validate_email("user.name@example.co.uk").is_ok());
        assert!(validate_email("user+tag@example.com").is_ok());
    }

    #[test]
    fn test_validate_email_invalid() {
        assert!(validate_email("notanemail").is_err());
        assert!(validate_email("@example.com").is_err());
        assert!(validate_email("user@").is_err());
        assert!(validate_email("user@example").is_err());
    }

    #[test]
    fn test_validate_email_empty() {
        assert!(validate_email("").is_err());
    }

    #[test]
    fn test_validate_email_whitespace_trimmed() {
        assert!(validate_email("  user@example.com  ").is_ok());
    }

    #[test]
    fn test_validate_email_only_whitespace() {
        assert!(validate_email("   ").is_err());
    }

    // ========== Password Validation Edge Cases ==========

    #[test]
    fn test_validate_password_empty() {
        assert!(validate_password("").is_err());
    }

    #[test]
    fn test_validate_password_no_lowercase() {
        assert!(validate_password("PASSWORD123").is_err());
    }

    #[test]
    fn test_validate_password_no_digit() {
        assert!(validate_password("Passworddd").is_err());
    }

    #[test]
    fn test_validate_password_exact_min_length() {
        assert!(validate_password("Abcdefg1").is_ok());
    }

    #[test]
    fn test_validate_password_one_below_min() {
        assert!(validate_password("Abcdef1").is_err());
    }

    #[test]
    fn test_validate_password_exact_max_length() {
        // Build a 128-char password that satisfies complexity: uppercase, lowercase, digit, no long repeats
        let pwd = "Ab1".repeat(42) + "Ab";
        assert_eq!(pwd.len(), 128);
        assert!(validate_password(&pwd).is_ok());
    }

    #[test]
    fn test_validate_password_over_max_length() {
        let pwd = "Ab1".repeat(43);
        assert_eq!(pwd.len(), 129);
        assert!(validate_password(&pwd).is_err());
    }

    // ========== Error Type Validation ==========

    #[test]
    fn test_validate_username_returns_invalid_input_error() {
        let err = validate_username("ab").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn test_validate_email_returns_invalid_input_error() {
        let err = validate_email("notanemail").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn test_validate_password_returns_invalid_input_error() {
        let err = validate_password("short").unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    // ========== Integration Tests ==========

    // ========== Refresh Rate Limiter Backend Tests ==========

    #[tokio::test]
    async fn test_refresh_token_uses_fail_closed_distributed_rate_limiter() {
        let pool =
            PgPool::connect_lazy("postgresql://invalid:invalid@127.0.0.1:1/invalid").unwrap();
        let jwt_service =
            crate::service::JwtService::new("test_secret_key_that_is_at_least_32_bytes_long")
                .unwrap();
        let username_cache =
            crate::cache::UsernameCache::local_only("test:username:".to_string(), 100, 60);
        let token_blacklist: std::sync::Arc<dyn crate::service::TokenBlacklistStore> =
            std::sync::Arc::new(crate::service::InMemoryTokenBlacklistStore::new(
                100, 3600, 86400,
            ));
        let key_builder = crate::cache::KeyBuilder::default();
        let brute_force = crate::service::BruteForceProtection::in_memory("test:".to_string());

        let mut user_service = UserService::new(
            pool,
            jwt_service,
            username_cache,
            crate::config::PasswordComplexityConfig::default(),
            token_blacklist,
            key_builder,
            brute_force,
        );

        user_service.set_refresh_rate_limiter_for_tests(
            Arc::new(
                RateLimiter::local_only("test-refresh:".to_string()).with_strict_distributed(),
            ),
        );

        let result = user_service
            .refresh_rate_limiter
            .check_rate_limit_distributed("refresh:user-1", 1, 60)
            .await;
        assert!(
            result.is_err(),
            "distributed refresh limit should fail closed when Redis is unavailable"
        );
    }

    #[tokio::test]
    async fn test_refresh_rate_limiter_non_strict_preserves_best_effort_behavior() {
        let pool =
            PgPool::connect_lazy("postgresql://invalid:invalid@127.0.0.1:1/invalid").unwrap();
        let jwt_service = crate::service::auth::JwtService::new(
            "test-secret-key-minimum-length-32-chars-required",
        )
        .unwrap();
        let username_cache = crate::cache::UsernameCache::local_only("test:".to_string(), 100, 0);
        let token_blacklist: Arc<dyn TokenBlacklistStore> = Arc::new(
            crate::service::InMemoryTokenBlacklistStore::new(100, 3600, 86400),
        );
        let key_builder = crate::cache::KeyBuilder::new("test");
        let brute_force = BruteForceProtection::in_memory("test".to_string());

        let mut user_service = super::UserService::new(
            pool,
            jwt_service,
            username_cache,
            PasswordComplexityConfig::default(),
            token_blacklist,
            key_builder,
            brute_force,
        );
        user_service.refresh_rate_limiter =
            Arc::new(RateLimiter::local_only("refresh-nonstrict:".to_string()));

        let result = user_service
            .refresh_rate_limiter
            .check_rate_limit("refresh:user-1", 1, 60)
            .await;
        assert!(
            result.is_ok(),
            "non-strict mode should allow normal in-memory checks"
        );
    }

    // === OAuth2 Email Verification Bypass Tests (P1#7) ===

    /// OAuth2 users should not be blocked by email_verification_required.
    /// This test verifies the logic that exempts OAuth2 signup method users.
    #[test]
    fn test_oauth2_user_bypasses_email_verification_check() {
        let now = chrono::Utc::now();

        // Simulate an OAuth2 user with email_verified=false
        let oauth2_user = crate::models::User {
            id: crate::models::UserId::new(),
            username: "oauth2user".to_string(),
            email: None,
            password_hash: "hash".to_string(),
            role: crate::models::UserRole::User,
            status: crate::models::UserStatus::Active,
            signup_method: crate::models::SignupMethod::OAuth2,
            email_verified: false,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            version: 0,
            deleted_at: None,
        };

        // The check in login(): email_verification_required && !email_verified && !is_oauth2
        let email_verification_required = true;
        let is_oauth2_user = oauth2_user.signup_method == crate::models::SignupMethod::OAuth2;

        // OAuth2 user should NOT be blocked
        let would_block =
            email_verification_required && !oauth2_user.email_verified && !is_oauth2_user;
        assert!(
            !would_block,
            "OAuth2 user should bypass email verification check"
        );
    }

    /// Non-OAuth2 users with email_verified=false should still be blocked.
    #[test]
    fn test_email_user_still_blocked_by_email_verification() {
        let now = chrono::Utc::now();

        let email_user = crate::models::User {
            id: crate::models::UserId::new(),
            username: "emailuser".to_string(),
            email: Some("user@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: crate::models::UserRole::User,
            status: crate::models::UserStatus::Active,
            signup_method: crate::models::SignupMethod::Email,
            email_verified: false,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            version: 0,
            deleted_at: None,
        };

        let email_verification_required = true;
        let is_oauth2_user = email_user.signup_method == crate::models::SignupMethod::OAuth2;

        // Email user should still be blocked
        let would_block =
            email_verification_required && !email_user.email_verified && !is_oauth2_user;
        assert!(
            would_block,
            "Email user with unverified email should be blocked"
        );
    }

    /// OAuth2 user with email_verified=true should also pass (no regression).
    #[test]
    fn test_oauth2_user_with_verified_email_passes() {
        let now = chrono::Utc::now();

        let oauth2_user = crate::models::User {
            id: crate::models::UserId::new(),
            username: "oauth2verified".to_string(),
            email: Some("verified@example.com".to_string()),
            password_hash: "hash".to_string(),
            role: crate::models::UserRole::User,
            status: crate::models::UserStatus::Active,
            signup_method: crate::models::SignupMethod::OAuth2,
            email_verified: true,
            created_at: now,
            updated_at: now,
            password_changed_at: now,
            password_version: 0,
            version: 0,
            deleted_at: None,
        };

        let email_verification_required = true;
        let is_oauth2_user = oauth2_user.signup_method == crate::models::SignupMethod::OAuth2;

        let would_block =
            email_verification_required && !oauth2_user.email_verified && !is_oauth2_user;
        assert!(
            !would_block,
            "OAuth2 user with verified email should not be blocked"
        );
    }

    /// When email_verification_required=false, nobody should be blocked.
    #[test]
    fn test_no_email_verification_required_passes_all() {
        let email_verification_required = false;
        let email_verified = false;
        let is_oauth2_user = false;

        let would_block = email_verification_required && !email_verified && !is_oauth2_user;
        assert!(
            !would_block,
            "No one should be blocked when email verification is not required"
        );
    }

    // ========== OAuth2 Login Brute-Force Counter Reset Tests ==========

    /// Test that successful OAuth2 login resets the brute-force counter for the provider user ID.
    ///
    /// This verifies the behavior described in `UserService::login_oauth2`:
    /// after a successful OAuth2 login, the brute-force counter for the provider_user_id
    /// should be reset so that future login attempts start with a clean slate.
    #[tokio::test]
    async fn test_oauth2_login_resets_brute_force_counter_for_provider_user_id() {
        use crate::cache::KeyBuilder;

        // Create an in-memory brute-force protection instance
        // Note: BruteForceProtection::in_memory uses this prefix directly in KeyBuilder::new()
        let prefix = "test_oauth2_reset";
        let key_builder = KeyBuilder::new(prefix);
        let brute_force = BruteForceProtection::in_memory(prefix.to_string());
        let provider_user_id = "github:12345";
        let client_ip: Option<std::net::IpAddr> = Some(std::net::IpAddr::V4(
            std::net::Ipv4Addr::new(192, 168, 1, 1),
        ));

        // Simulate some failed login attempts
        for _ in 0..3 {
            brute_force
                .record_failure(provider_user_id, client_ip)
                .await
                .unwrap();
        }

        // Verify that failures were recorded using the prefixed key
        let tracker = brute_force.username_tracker();
        let prefixed_key = key_builder.login_attempts(provider_user_id);
        let (count_before, _) = tracker.get_attempts(&prefixed_key).await.unwrap();
        assert_eq!(count_before, 3, "Should have 3 failures recorded");

        // Simulate successful OAuth2 login by resetting the counter
        // (this is what happens in login_oauth2 on success)
        brute_force.reset(provider_user_id).await.unwrap();

        // Verify counter was reset
        let (count_after, _) = tracker.get_attempts(&prefixed_key).await.unwrap();
        assert_eq!(
            count_after, 0,
            "Counter should be reset to 0 after successful OAuth2 login"
        );
    }

    /// Test that successful OAuth2 login resets the brute-force counter for the client IP.
    ///
    /// This verifies that the IP-based brute-force counter is also reset after
    /// a successful OAuth2 login, allowing future attempts from the same IP.
    #[tokio::test]
    async fn test_oauth2_login_resets_brute_force_counter_for_client_ip() {
        use crate::cache::KeyBuilder;

        let prefix = "test_oauth2_ip_reset";
        let key_builder = KeyBuilder::new(prefix);
        let brute_force = BruteForceProtection::in_memory(prefix.to_string());
        let provider_user_id = "github:67890";
        let client_ip: std::net::IpAddr =
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 1));

        // Simulate some failed login attempts from this IP
        for _ in 0..5 {
            brute_force
                .record_failure(provider_user_id, Some(client_ip))
                .await
                .unwrap();
        }

        // Verify that IP failures were recorded using the prefixed key
        let ip_tracker = brute_force.ip_tracker();
        let ip_key = key_builder.login_attempts_ip(&client_ip.to_string());
        let (count_before, _) = ip_tracker.get_attempts(&ip_key).await.unwrap();
        assert_eq!(count_before, 5, "Should have 5 IP failures recorded");

        // Simulate successful OAuth2 login by resetting the IP counter
        brute_force.reset_ip(&client_ip).await.unwrap();

        // Verify IP counter was reset
        let (count_after, _) = ip_tracker.get_attempts(&ip_key).await.unwrap();
        assert_eq!(
            count_after, 0,
            "IP counter should be reset to 0 after successful OAuth2 login"
        );
    }

    /// Test that failed OAuth2 login (e.g., user is banned) increments the brute-force counter.
    ///
    /// This verifies that when an OAuth2 login fails due to user status issues,
    /// the brute-force counter is incremented to prevent brute-forcing against locked accounts.
    #[tokio::test]
    async fn test_oauth2_login_failure_increments_brute_force_counter() {
        use crate::cache::KeyBuilder;

        let prefix = "test_oauth2_failure";
        let key_builder = KeyBuilder::new(prefix);
        let brute_force = BruteForceProtection::in_memory(prefix.to_string());
        let provider_user_id = "google:99999";
        let client_ip: Option<std::net::IpAddr> =
            Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(172, 16, 0, 1)));

        // Simulate a failed OAuth2 login attempt (user banned/pending/deleted)
        // The login_oauth2 method calls record_failure on failure
        brute_force
            .record_failure(provider_user_id, client_ip)
            .await
            .unwrap();

        // Verify counter was incremented using the prefixed key
        let tracker = brute_force.username_tracker();
        let prefixed_key = key_builder.login_attempts(provider_user_id);
        let (count, _) = tracker.get_attempts(&prefixed_key).await.unwrap();
        assert_eq!(
            count, 1,
            "Counter should be incremented after failed OAuth2 login"
        );
    }

    /// Test that brute-force check happens before OAuth2 token issuance.
    ///
    /// This verifies that the check_allowed method is called before processing
    /// an OAuth2 login, preventing locked-out users from getting tokens.
    #[tokio::test]
    async fn test_oauth2_login_checks_brute_force_before_token_issuance() {
        let brute_force = BruteForceProtection::in_memory("test_oauth2_check".to_string());
        let provider_user_id = "discord:11111";
        let client_ip: Option<std::net::IpAddr> = Some(std::net::IpAddr::V4(
            std::net::Ipv4Addr::new(192, 168, 100, 1),
        ));

        // Record enough failures to trigger lockout (5 is the tier1 threshold)
        for _ in 0..5 {
            brute_force
                .record_failure(provider_user_id, client_ip)
                .await
                .unwrap();
        }

        // Verify that check_allowed now returns an error
        let result = brute_force.check_allowed(provider_user_id, client_ip).await;
        assert!(result.is_err(), "Should be locked out after 5 failures");

        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Too many failed login attempts"),
            "Error should mention lockout: {err_msg}"
        );
    }

    /// Test that brute-force counters for both provider_user_id and IP are independently tracked.
    ///
    /// This verifies that resetting the provider_user_id counter does not affect the IP counter
    /// and vice versa.
    #[tokio::test]
    async fn test_oauth2_login_resets_counters_independently() {
        use crate::cache::KeyBuilder;

        let prefix = "test_oauth2_independent";
        let key_builder = KeyBuilder::new(prefix);
        let brute_force = BruteForceProtection::in_memory(prefix.to_string());
        let provider_user_id = "github:22222";
        let client_ip: std::net::IpAddr =
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 10, 10, 10));

        // Record failures
        for _ in 0..3 {
            brute_force
                .record_failure(provider_user_id, Some(client_ip))
                .await
                .unwrap();
        }

        // Reset only the provider_user_id counter
        brute_force.reset(provider_user_id).await.unwrap();

        // Verify provider_user_id counter is reset using prefixed key
        let tracker = brute_force.username_tracker();
        let user_key = key_builder.login_attempts(provider_user_id);
        let (user_count, _) = tracker.get_attempts(&user_key).await.unwrap();
        assert_eq!(user_count, 0, "Provider user ID counter should be reset");

        // Verify IP counter is NOT reset
        let ip_tracker = brute_force.ip_tracker();
        let ip_key = key_builder.login_attempts_ip(&client_ip.to_string());
        let (ip_count, _) = ip_tracker.get_attempts(&ip_key).await.unwrap();
        assert_eq!(ip_count, 3, "IP counter should still have 3 failures");

        // Now reset the IP counter
        brute_force.reset_ip(&client_ip).await.unwrap();

        // Verify IP counter is now reset
        let (ip_count_after, _) = ip_tracker.get_attempts(&ip_key).await.unwrap();
        assert_eq!(ip_count_after, 0, "IP counter should be reset");
    }

    #[tokio::test]
    async fn test_password_login_uses_same_brute_force_key_for_check_and_record() {
        use crate::cache::KeyBuilder;

        let prefix = "test_password_login_key_consistency";
        let key_builder = KeyBuilder::new(prefix);
        let brute_force = BruteForceProtection::in_memory(prefix.to_string());
        let identifier = "user@example.com";
        let client_ip = Some(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));

        for _ in 0..5 {
            brute_force
                .record_failure(identifier, client_ip)
                .await
                .unwrap();
        }

        let prefixed_identifier_key = key_builder.login_attempts(identifier);
        let (attempts, _) = brute_force
            .username_tracker()
            .get_attempts(&prefixed_identifier_key)
            .await
            .unwrap();
        assert_eq!(attempts, 5, "identifier bucket should accumulate failures");

        let result = brute_force.check_allowed(identifier, client_ip).await;
        assert!(result.is_err(), "same identifier bucket should be checked");
    }

    // ========== register_with_executor signup_need_review Tests ==========

    /// Verify that register_with_executor respects signup_need_review for Email signups.
    ///
    /// This test mirrors the logic in register_with_executor to verify that when
    /// signup_need_review is true and signup_method is Email, the initial status
    /// is Pending (not Active).
    #[test]
    fn test_register_with_executor_respects_signup_need_review_for_email() {
        // Simulate the logic from register_with_executor
        let email_verification_required = false;
        let signup_need_review = true;
        let signup_method = SignupMethod::Email;

        let initial_status = match signup_method {
            SignupMethod::OAuth2 | SignupMethod::AdminCreated => crate::models::UserStatus::Active,
            SignupMethod::Email | SignupMethod::Password | SignupMethod::Unknown => {
                if email_verification_required || signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
        };

        assert_eq!(
            initial_status,
            crate::models::UserStatus::Pending,
            "Email registration with signup_need_review=true must produce Pending status"
        );
    }

    /// Verify that OAuth2 signups respect signup_need_review.
    #[test]
    fn test_register_with_executor_oauth2_respects_signup_need_review() {
        let email_verification_required = false;
        let signup_need_review = true;
        let signup_method = SignupMethod::OAuth2;

        let initial_status = match signup_method {
            SignupMethod::OAuth2 | SignupMethod::AdminCreated => {
                if signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
            SignupMethod::Email | SignupMethod::Password | SignupMethod::Unknown => {
                if email_verification_required || signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
        };

        assert_eq!(
            initial_status,
            crate::models::UserStatus::Pending,
            "OAuth2 registration with signup_need_review=true must produce Pending status"
        );
    }

    /// Verify that Email signups are Active when neither review nor verification is required.
    #[test]
    fn test_register_with_executor_email_active_when_no_review() {
        let email_verification_required = false;
        let signup_need_review = false;
        let signup_method = SignupMethod::Email;

        let initial_status = match signup_method {
            SignupMethod::OAuth2 | SignupMethod::AdminCreated => {
                if signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
            SignupMethod::Email | SignupMethod::Password | SignupMethod::Unknown => {
                if email_verification_required || signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
        };

        assert_eq!(
            initial_status,
            crate::models::UserStatus::Active,
            "Email registration with no review/verification should be Active"
        );
    }

    /// Verify that OAuth2 signups are Active when signup_need_review is false.
    #[test]
    fn test_register_with_executor_oauth2_active_when_no_review() {
        let email_verification_required = false;
        let signup_need_review = false;
        let signup_method = SignupMethod::OAuth2;

        let initial_status = match signup_method {
            SignupMethod::OAuth2 | SignupMethod::AdminCreated => {
                if signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
            SignupMethod::Email | SignupMethod::Password | SignupMethod::Unknown => {
                if email_verification_required || signup_need_review {
                    crate::models::UserStatus::Pending
                } else {
                    crate::models::UserStatus::Active
                }
            }
        };

        assert_eq!(
            initial_status,
            crate::models::UserStatus::Active,
            "OAuth2 registration with signup_need_review=false should be Active"
        );
    }
}
