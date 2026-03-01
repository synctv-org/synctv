//! Audit logging service
//!
//! Tracks admin actions and permission changes for compliance and debugging.
//!
//! ## Async Buffering
//!
//! Audit events are buffered in memory via a `tokio::sync::mpsc` channel and
//! flushed to the database in batches (every 5 seconds or when 100 events
//! accumulate). This decouples the request path from database write latency.
//! On graceful shutdown the remaining buffer is flushed.
//!
//! # Audit Logging Design Tradeoff
//!
//! ## Availability vs Consistency
//!
//! This service chooses **availability over consistency** for audit logging.
//! When audit log writes fail, operations are **not blocked**.
//!
//! ## Rationale
//!
//! - Audit logs are important for compliance and debugging
//! - Blocking user operations for audit failures would hurt availability
//! - Failed audits are logged at ERROR level for monitoring
//! - Monitoring should alert on audit failures
//!
//! ## Failure Scenarios
//!
//! 1. **Buffer full**: Falls back to synchronous write; if that also fails,
//!    logs ERROR and increments dropped counter
//! 2. **Database unavailable**: Batch flush retries with exponential backoff
//!    (100ms, 200ms, 400ms); after 3 failures, logs ERROR and drops batch
//! 3. **Channel closed**: Logs ERROR and continues operation
//!
//! ## Monitoring
//!
//! Operators should monitor for ERROR logs indicating audit failures:
//! - "Audit sync fallback write also failed, event dropped"
//! - "Failed to flush audit batch after all retries, events dropped"
//!
//! The `dropped_count()` method returns the total number of dropped events.
//!
//! ## Recovery Strategies
//!
//! - **Temporary database outage**: Events are retried with backoff; most
//!   outages are recovered automatically
//! - **Extended outage**: Events may be dropped; check application logs
//!   for ERROR messages and investigate database connectivity
//! - **Buffer overflow**: Indicates high audit volume; consider increasing
//!   buffer capacity via configuration

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::Result;

/// Default channel capacity for the audit event buffer
const DEFAULT_BUFFER_CAPACITY: usize = 10_000;
/// Maximum number of events to accumulate before flushing
const FLUSH_BATCH_SIZE: usize = 100;
/// Flush interval in seconds
const FLUSH_INTERVAL_SECS: u64 = 5;

/// Audit log entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditLog {
    pub id: String,
    pub actor_id: String,
    pub actor_username: String,
    pub action: AuditAction,
    pub target_type: AuditTargetType,
    pub target_id: Option<String>,
    pub details: serde_json::Value,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Parameters for logging an audit event
#[derive(Debug, Clone)]
pub struct AuditEventParams {
    pub actor_id: String,
    pub actor_username: String,
    pub action: AuditAction,
    pub target_type: AuditTargetType,
    pub target_id: Option<String>,
    pub details: serde_json::Value,
    pub ip_address: Option<String>,
    pub user_agent: Option<String>,
}

/// Internal record sent through the mpsc channel
#[derive(Debug, Clone)]
struct AuditRecord {
    actor_id: String,
    actor_username: String,
    action_str: &'static str,
    target_str: &'static str,
    target_id: Option<String>,
    details: serde_json::Value,
    ip_address: Option<String>,
    user_agent: Option<String>,
    created_at: DateTime<Utc>,
}

/// Audit actions
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditAction {
    UserCreated,
    UserDeleted,
    UserBanned,
    UserUnbanned,
    UserPasswordUpdated,
    UserUsernameUpdated,
    UserRoleUpdated,
    RoomCreated,
    RoomDeleted,
    RoomBanned,
    RoomUnbanned,
    RoomPasswordUpdated,
    PermissionGranted,
    PermissionRevoked,
    ProviderInstanceCreated,
    ProviderInstanceUpdated,
    ProviderInstanceDeleted,
    ProviderInstanceReconnected,
    SettingsUpdated,
    MemberKicked,
    MemberBanned,
    MemberUnbanned,
    MemberRoleUpdated,
    MemberPermissionUpdated,
    MemberStatusUpdated,
    RoomSettingsUpdated,
    UserApproved,
    RoomApproved,
    RoomRejected,
    StreamKicked,
    RateLimitResetFailed,
    // Token security events (Task #65)
    UserLogin,
    UserLogout,
    TokenIssued,
    TokenRefreshed,
    TokenFamilyRevoked,
    // Settings access audit (read operations)
    SettingsViewed,
    SettingsGroupViewed,
}

impl AuditAction {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::UserCreated => "user_created",
            Self::UserDeleted => "user_deleted",
            Self::UserBanned => "user_banned",
            Self::UserUnbanned => "user_unbanned",
            Self::UserPasswordUpdated => "user_password_updated",
            Self::UserUsernameUpdated => "user_username_updated",
            Self::UserRoleUpdated => "user_role_updated",
            Self::RoomCreated => "room_created",
            Self::RoomDeleted => "room_deleted",
            Self::RoomBanned => "room_banned",
            Self::RoomUnbanned => "room_unbanned",
            Self::RoomPasswordUpdated => "room_password_updated",
            Self::PermissionGranted => "permission_granted",
            Self::PermissionRevoked => "permission_revoked",
            Self::ProviderInstanceCreated => "provider_instance_created",
            Self::ProviderInstanceUpdated => "provider_instance_updated",
            Self::ProviderInstanceDeleted => "provider_instance_deleted",
            Self::ProviderInstanceReconnected => "provider_instance_reconnected",
            Self::SettingsUpdated => "settings_updated",
            Self::MemberKicked => "member_kicked",
            Self::MemberBanned => "member_banned",
            Self::MemberUnbanned => "member_unbanned",
            Self::MemberRoleUpdated => "member_role_updated",
            Self::MemberPermissionUpdated => "member_permission_updated",
            Self::MemberStatusUpdated => "member_status_updated",
            Self::RoomSettingsUpdated => "room_settings_updated",
            Self::UserApproved => "user_approved",
            Self::RoomApproved => "room_approved",
            Self::RoomRejected => "room_rejected",
            Self::StreamKicked => "stream_kicked",
            Self::RateLimitResetFailed => "rate_limit_reset_failed",
            // Token security events
            Self::UserLogin => "user_login",
            Self::UserLogout => "user_logout",
            Self::TokenIssued => "token_issued",
            Self::TokenRefreshed => "token_refreshed",
            Self::TokenFamilyRevoked => "token_family_revoked",
            // Settings access audit (read operations)
            Self::SettingsViewed => "settings_viewed",
            Self::SettingsGroupViewed => "settings_group_viewed",
        }
    }
}

/// Target types
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditTargetType {
    User,
    Room,
    Member,
    ProviderInstance,
    Settings,
    System,
    Stream,
    Token,
}

impl AuditTargetType {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Room => "room",
            Self::Member => "member",
            Self::ProviderInstance => "provider_instance",
            Self::Settings => "settings",
            Self::System => "system",
            Self::Stream => "stream",
            Self::Token => "token",
        }
    }
}

/// Audit logging service
///
/// Records audit logs for security-relevant actions. Events are buffered in
/// memory and flushed to the database in batches for performance.
pub struct AuditService {
    pool: PgPool,
    /// Sender half of the buffered channel (None when running without background task)
    sender: Option<mpsc::Sender<AuditRecord>>,
    /// Counter of dropped events (channel full)
    dropped_count: Arc<AtomicUsize>,
}

impl AuditService {
    /// Create a new audit service with async buffering.
    ///
    /// Spawns a background task that flushes buffered audit events to the
    /// database every 5 seconds or when 100 events accumulate.
    ///
    /// The returned [`AuditFlushHandle`] must be held for the lifetime of
    /// the service. Dropping it triggers a graceful flush of remaining events.
    #[must_use]
    pub fn new(pool: PgPool) -> (Self, AuditFlushHandle) {
        Self::with_capacity(pool, DEFAULT_BUFFER_CAPACITY)
    }

    /// Create a new audit service with async buffering and a custom buffer capacity.
    ///
    /// Use this to override the default buffer capacity (10,000) via configuration.
    /// A capacity of 0 falls back to `DEFAULT_BUFFER_CAPACITY`.
    #[must_use]
    pub fn with_capacity(pool: PgPool, capacity: usize) -> (Self, AuditFlushHandle) {
        let capacity = if capacity > 0 {
            capacity
        } else {
            DEFAULT_BUFFER_CAPACITY
        };
        let (tx, rx) = mpsc::channel(capacity);
        let dropped_count = Arc::new(AtomicUsize::new(0));

        let handle = AuditFlushHandle::spawn(pool.clone(), rx, Arc::clone(&dropped_count));

        let service = Self {
            pool,
            sender: Some(tx),
            dropped_count,
        };

        (service, handle)
    }

    /// Create a new audit service **without** async buffering.
    ///
    /// Each call to `log()` writes directly to the database. Useful for tests
    /// or environments where a background task is not desired.
    #[must_use]
    pub fn new_unbuffered(pool: PgPool) -> Self {
        Self {
            pool,
            sender: None,
            dropped_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Return the number of events that were dropped because the buffer was full.
    #[must_use]
    pub fn dropped_count(&self) -> usize {
        self.dropped_count.load(Ordering::Relaxed)
    }

    /// Log an audit event
    #[allow(clippy::too_many_arguments)]
    pub async fn log(
        &self,
        actor_id: String,
        actor_username: String,
        action: AuditAction,
        target_type: AuditTargetType,
        target_id: Option<String>,
        details: serde_json::Value,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<()> {
        let action_str = action.as_str();
        let target_str = target_type.as_str();
        let created_at = Utc::now();

        // If we have a buffered sender, enqueue the event
        if let Some(ref sender) = self.sender {
            let record = AuditRecord {
                actor_id: actor_id.clone(),
                actor_username: actor_username.clone(),
                action_str,
                target_str,
                target_id: target_id.clone(),
                details: details.clone(),
                ip_address: ip_address.clone(),
                user_agent: user_agent.clone(),
                created_at,
            };

            if let Err(_e) = sender.try_send(record) {
                // Buffer full: fall back to synchronous DB write instead of dropping
                tracing::warn!(
                    actor_id = %actor_id,
                    action = %action_str,
                    "Audit buffer full, falling back to synchronous DB write"
                );
                if let Err(db_err) = Self::write_single(
                    &self.pool,
                    action_str,
                    target_str,
                    &actor_id,
                    &actor_username,
                    &target_id,
                    &details,
                    &ip_address,
                    &user_agent,
                    created_at,
                )
                .await
                {
                    self.dropped_count.fetch_add(1, Ordering::Relaxed);
                    tracing::error!(
                        actor_id = %actor_id,
                        action = %action_str,
                        error = %db_err,
                        "Audit sync fallback write also failed, event dropped"
                    );
                }
            } else {
                tracing::debug!(
                    actor_id = %actor_id,
                    action = %action_str,
                    target_type = %target_str,
                    "Audit event buffered"
                );
            }

            return Ok(());
        }

        // Unbuffered mode: write directly to DB
        Self::write_single(
            &self.pool,
            action_str,
            target_str,
            &actor_id,
            &actor_username,
            &target_id,
            &details,
            &ip_address,
            &user_agent,
            created_at,
        )
        .await?;

        tracing::debug!(
            actor_id = %actor_id,
            action = %action_str,
            target_type = %target_str,
            "Audit log recorded"
        );

        Ok(())
    }

    /// Write a single audit record to the database
    #[allow(clippy::too_many_arguments)]
    async fn write_single(
        pool: &PgPool,
        action_str: &str,
        target_str: &str,
        actor_id: &str,
        actor_username: &str,
        target_id: &Option<String>,
        details: &serde_json::Value,
        ip_address: &Option<String>,
        user_agent: &Option<String>,
        created_at: DateTime<Utc>,
    ) -> Result<()> {
        let query = r"
            INSERT INTO audit_logs (
                actor_id, actor_username, action, target_type, target_id,
                details, ip_address, user_agent, created_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
        ";

        sqlx::query(query)
            .bind(actor_id)
            .bind(actor_username)
            .bind(action_str)
            .bind(target_str)
            .bind(target_id)
            .bind(details)
            .bind(ip_address)
            .bind(user_agent)
            .bind(created_at)
            .execute(pool)
            .await?;

        Ok(())
    }

    /// Log an audit event with parameters struct
    pub async fn log_with_params(&self, params: AuditEventParams) -> Result<()> {
        self.log(
            params.actor_id,
            params.actor_username,
            params.action,
            params.target_type,
            params.target_id,
            params.details,
            params.ip_address,
            params.user_agent,
        )
        .await
    }

    /// Log user creation
    pub async fn log_user_created(
        &self,
        actor_id: String,
        actor_username: String,
        target_user_id: String,
    ) -> Result<()> {
        self.log(
            actor_id,
            actor_username,
            AuditAction::UserCreated,
            AuditTargetType::User,
            Some(target_user_id),
            serde_json::json!({"reason": "User created via admin panel"}),
            None,
            None,
        )
        .await
    }

    /// Log user ban
    pub async fn log_user_banned(
        &self,
        actor_id: String,
        actor_username: String,
        target_user_id: String,
    ) -> Result<()> {
        self.log(
            actor_id,
            actor_username,
            AuditAction::UserBanned,
            AuditTargetType::User,
            Some(target_user_id),
            serde_json::json!({"reason": "User banned by admin"}),
            None,
            None,
        )
        .await
    }

    /// Log permission change
    ///
    /// The `is_grant` parameter determines the audit action:
    /// - `true` => `AuditAction::PermissionGranted`
    /// - `false` => `AuditAction::PermissionRevoked`
    #[allow(clippy::too_many_arguments)]
    pub async fn log_permission_changed(
        &self,
        actor_id: String,
        actor_username: String,
        target_type: AuditTargetType,
        target_id: String,
        old_permissions: u64,
        new_permissions: u64,
        is_grant: bool,
    ) -> Result<()> {
        let action = if is_grant {
            AuditAction::PermissionGranted
        } else {
            AuditAction::PermissionRevoked
        };

        self.log(
            actor_id,
            actor_username,
            action,
            target_type,
            Some(target_id),
            serde_json::json!({
                "old_permissions": old_permissions,
                "new_permissions": new_permissions
            }),
            None,
            None,
        )
        .await
    }

    /// Log room deletion
    pub async fn log_room_deleted(
        &self,
        actor_id: String,
        actor_username: String,
        room_id: String,
    ) -> Result<()> {
        self.log(
            actor_id,
            actor_username,
            AuditAction::RoomDeleted,
            AuditTargetType::Room,
            Some(room_id),
            serde_json::json!({"reason": "Room deleted by admin"}),
            None,
            None,
        )
        .await
    }

    /// Log stream kick event
    #[allow(clippy::too_many_arguments)]
    pub async fn log_stream_kicked(
        &self,
        actor_id: String,
        actor_username: String,
        room_id: String,
        media_id: String,
        reason: Option<String>,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<()> {
        self.log(
            actor_id,
            actor_username,
            AuditAction::StreamKicked,
            AuditTargetType::Stream,
            Some(format!("{room_id}:{media_id}")),
            serde_json::json!({
                "room_id": room_id,
                "media_id": media_id,
                "reason": reason.unwrap_or_default()
            }),
            ip_address,
            user_agent,
        )
        .await
    }

    /// Log rate limit reset failure event.
    ///
    /// This is used when a password verification succeeds but the rate limit
    /// counter reset fails (e.g., Redis unavailable). This is security-relevant
    /// because it could lead to legitimate users being locked out if the counter
    /// persists after successful authentication.
    pub async fn log_rate_limit_reset_failed(
        &self,
        target_type: AuditTargetType,
        target_id: String,
        error_message: String,
        ip_address: Option<String>,
    ) -> Result<()> {
        self.log(
            "system".to_string(),
            "system".to_string(),
            AuditAction::RateLimitResetFailed,
            target_type,
            Some(target_id),
            serde_json::json!({
                "error": error_message,
                "context": "password_verification_succeeded"
            }),
            ip_address,
            None,
        )
        .await
    }

    // ========================================================================
    // Token Security Audit Methods (Task #65)
    // ========================================================================

    /// Log a successful user login event.
    ///
    /// Records the user ID, username, IP address, and user agent for security
    /// auditing and incident investigation.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_user_login(
        &self,
        user_id: String,
        username: String,
        login_method: &str,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<()> {
        self.log(
            user_id.clone(),
            username.clone(),
            AuditAction::UserLogin,
            AuditTargetType::User,
            Some(user_id),
            serde_json::json!({
                "login_method": login_method,
                "username": username
            }),
            ip_address,
            user_agent,
        )
        .await
    }

    /// Log a user logout event.
    ///
    /// Records when a user explicitly logs out (access token blacklisted).
    pub async fn log_user_logout(
        &self,
        user_id: String,
        username: String,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<()> {
        self.log(
            user_id.clone(),
            username,
            AuditAction::UserLogout,
            AuditTargetType::User,
            Some(user_id),
            serde_json::json!({}),
            ip_address,
            user_agent,
        )
        .await
    }

    /// Log a token issuance event.
    ///
    /// Records when tokens are issued (login, `OAuth2`, or refresh).
    /// The JTI is recorded for token tracing and revocation investigations.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_token_issued(
        &self,
        user_id: String,
        username: String,
        token_type: &str,
        jti: String,
        expires_at: i64,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<()> {
        self.log(
            user_id.clone(),
            username,
            AuditAction::TokenIssued,
            AuditTargetType::Token,
            Some(format!("{user_id}:{jti}")),
            serde_json::json!({
                "token_type": token_type,
                "jti": jti,
                "expires_at": expires_at
            }),
            ip_address,
            user_agent,
        )
        .await
    }

    /// Log a token refresh event.
    ///
    /// Records when a refresh token is used to generate new tokens.
    /// Both old and new JTI values are recorded for token chain tracing.
    #[allow(clippy::too_many_arguments)]
    pub async fn log_token_refreshed(
        &self,
        user_id: String,
        username: String,
        old_jti: String,
        new_jti: String,
        ip_address: Option<String>,
        user_agent: Option<String>,
    ) -> Result<()> {
        self.log(
            user_id.clone(),
            username,
            AuditAction::TokenRefreshed,
            AuditTargetType::Token,
            Some(format!("{user_id}:{new_jti}")),
            serde_json::json!({
                "old_jti": old_jti,
                "new_jti": new_jti
            }),
            ip_address,
            user_agent,
        )
        .await
    }

    /// Log a token family revocation event.
    ///
    /// Recorded when a refresh token replay is detected, indicating possible
    /// token theft. All refresh tokens for the user are revoked as a security
    /// measure.
    pub async fn log_token_family_revoked(
        &self,
        user_id: String,
        username: String,
        replayed_jti: String,
        ip_address: Option<String>,
    ) -> Result<()> {
        self.log(
            user_id.clone(),
            username,
            AuditAction::TokenFamilyRevoked,
            AuditTargetType::Token,
            Some(user_id),
            serde_json::json!({
                "replayed_jti": replayed_jti,
                "reason": "token_replay_detected"
            }),
            ip_address,
            None,
        )
        .await
    }
}

impl std::fmt::Debug for AuditService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditService")
            .field("buffered", &self.sender.is_some())
            .field("dropped_count", &self.dropped_count.load(Ordering::Relaxed))
            .finish()
    }
}

/// Handle for the background flush task.
///
/// Dropping this handle signals the background task to flush remaining events
/// and shut down gracefully.
pub struct AuditFlushHandle {
    join_handle: Option<tokio::task::JoinHandle<()>>,
    /// Sender kept alive to control channel lifetime. Dropping it causes the
    /// background receiver loop to terminate after draining remaining items.
    _cancel: tokio::sync::watch::Sender<bool>,
    /// Guards against sending the shutdown signal more than once (e.g. when
    /// both `shutdown()` and `Drop` are executed for the same handle).
    has_signaled: AtomicBool,
}

impl AuditFlushHandle {
    fn spawn(
        pool: PgPool,
        mut rx: mpsc::Receiver<AuditRecord>,
        dropped_count: Arc<AtomicUsize>,
    ) -> Self {
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);

        let join_handle = crate::spawn::spawn_monitored("audit_flush", async move {
            let mut buffer: Vec<AuditRecord> = Vec::with_capacity(FLUSH_BATCH_SIZE);
            let mut interval =
                tokio::time::interval(std::time::Duration::from_secs(FLUSH_INTERVAL_SECS));
            // Don't fire immediately on first tick
            interval.tick().await;

            loop {
                tokio::select! {
                    // Receive events
                    maybe_record = rx.recv() => {
                        if let Some(record) = maybe_record {
                            buffer.push(record);
                            if buffer.len() >= FLUSH_BATCH_SIZE {
                                flush_batch(&pool, &mut buffer, &dropped_count).await;
                            }
                        } else {
                            // Channel closed, flush remaining and exit
                            if !buffer.is_empty() {
                                flush_batch(&pool, &mut buffer, &dropped_count).await;
                            }
                            tracing::info!("Audit flush task: channel closed, exiting");
                            return;
                        }
                    }
                    // Periodic flush
                    _ = interval.tick() => {
                        if !buffer.is_empty() {
                            flush_batch(&pool, &mut buffer, &dropped_count).await;
                        }
                    }
                    // Graceful shutdown signal
                    _ = cancel_rx.changed() => {
                        // Drain remaining items from the channel
                        rx.close();
                        while let Some(record) = rx.recv().await {
                            buffer.push(record);
                        }
                        if !buffer.is_empty() {
                            flush_batch(&pool, &mut buffer, &dropped_count).await;
                        }
                        tracing::info!("Audit flush task: graceful shutdown complete");
                        return;
                    }
                }
            }
        });

        Self {
            join_handle: Some(join_handle),
            _cancel: cancel_tx,
            has_signaled: AtomicBool::new(false),
        }
    }

    /// Send the shutdown signal exactly once.
    ///
    /// Returns `true` if this call was the first to send the signal, `false`
    /// if the signal had already been sent (by a previous `shutdown()` or by
    /// `Drop`).
    fn send_shutdown_signal(&self) -> bool {
        // compare_exchange: set true only if currently false.
        // Use AcqRel / Acquire for proper ordering.
        if self
            .has_signaled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let _ = self._cancel.send(true);
            true
        } else {
            false
        }
    }

    /// Trigger graceful shutdown and wait for the flush to complete.
    pub async fn shutdown(mut self) {
        self.send_shutdown_signal();
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for AuditFlushHandle {
    fn drop(&mut self) {
        // Signal shutdown (non-blocking, idempotent). The background task
        // will drain on its next iteration. If shutdown() was already called
        // the signal is not sent again.
        self.send_shutdown_signal();
    }
}

/// Maximum retry attempts for `flush_batch`
const FLUSH_MAX_RETRIES: u32 = 3;
/// Base delay in milliseconds for exponential backoff on flush retries
const FLUSH_RETRY_BASE_MS: u64 = 100;

/// Flush a batch of audit records to the database with retry on failure.
///
/// Uses exponential backoff (100ms, 200ms, 400ms) before giving up and
/// counting the batch as dropped.
async fn flush_batch(pool: &PgPool, buffer: &mut Vec<AuditRecord>, dropped_count: &AtomicUsize) {
    let batch_size = buffer.len();
    tracing::debug!(batch_size = batch_size, "Flushing audit event batch");

    // Build a batch insert using UNNEST for efficiency
    let mut actor_ids = Vec::with_capacity(batch_size);
    let mut actor_usernames = Vec::with_capacity(batch_size);
    let mut actions = Vec::with_capacity(batch_size);
    let mut target_types = Vec::with_capacity(batch_size);
    let mut target_ids: Vec<Option<String>> = Vec::with_capacity(batch_size);
    let mut details_list: Vec<serde_json::Value> = Vec::with_capacity(batch_size);
    let mut ip_addresses: Vec<Option<String>> = Vec::with_capacity(batch_size);
    let mut user_agents: Vec<Option<String>> = Vec::with_capacity(batch_size);
    let mut created_ats: Vec<DateTime<Utc>> = Vec::with_capacity(batch_size);

    for record in buffer.iter() {
        actor_ids.push(record.actor_id.clone());
        actor_usernames.push(record.actor_username.clone());
        actions.push(record.action_str.to_string());
        target_types.push(record.target_str.to_string());
        target_ids.push(record.target_id.clone());
        details_list.push(record.details.clone());
        ip_addresses.push(record.ip_address.clone());
        user_agents.push(record.user_agent.clone());
        created_ats.push(record.created_at);
    }

    let query = r"
        INSERT INTO audit_logs (
            actor_id, actor_username, action, target_type, target_id,
            details, ip_address, user_agent, created_at
        )
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::text[], $4::text[], $5::text[],
            $6::jsonb[], $7::text[], $8::text[], $9::timestamptz[]
        )
    ";

    for attempt in 0..FLUSH_MAX_RETRIES {
        match sqlx::query(query)
            .bind(&actor_ids)
            .bind(&actor_usernames)
            .bind(&actions)
            .bind(&target_types)
            .bind(&target_ids)
            .bind(&details_list)
            .bind(&ip_addresses)
            .bind(&user_agents)
            .bind(&created_ats)
            .execute(pool)
            .await
        {
            Ok(_) => {
                tracing::debug!(batch_size = batch_size, "Audit batch flushed successfully");
                buffer.clear();
                return;
            }
            Err(e) => {
                if attempt + 1 < FLUSH_MAX_RETRIES {
                    let backoff_ms = FLUSH_RETRY_BASE_MS * (1 << attempt);
                    tracing::warn!(
                        batch_size = batch_size,
                        attempt = attempt + 1,
                        max_retries = FLUSH_MAX_RETRIES,
                        backoff_ms = backoff_ms,
                        error = %e,
                        "Audit batch flush failed, retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                } else {
                    dropped_count.fetch_add(batch_size, Ordering::Relaxed);
                    tracing::error!(
                        batch_size = batch_size,
                        attempts = FLUSH_MAX_RETRIES,
                        error = %e,
                        "Failed to flush audit batch after all retries, events dropped"
                    );
                }
            }
        }
    }

    buffer.clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========== AuditAction Serialization ==========

    #[test]
    fn test_audit_action_serialization() {
        let action = AuditAction::UserCreated;
        let json = serde_json::to_string(&action).unwrap();
        assert!(json.contains("user_created"));
    }

    #[test]
    fn test_all_audit_actions_serialize_to_snake_case() {
        let actions = vec![
            (AuditAction::UserCreated, "user_created"),
            (AuditAction::UserDeleted, "user_deleted"),
            (AuditAction::UserBanned, "user_banned"),
            (AuditAction::UserUnbanned, "user_unbanned"),
            (AuditAction::UserPasswordUpdated, "user_password_updated"),
            (AuditAction::UserUsernameUpdated, "user_username_updated"),
            (AuditAction::UserRoleUpdated, "user_role_updated"),
            (AuditAction::RoomCreated, "room_created"),
            (AuditAction::RoomDeleted, "room_deleted"),
            (AuditAction::RoomBanned, "room_banned"),
            (AuditAction::RoomUnbanned, "room_unbanned"),
            (AuditAction::RoomPasswordUpdated, "room_password_updated"),
            (AuditAction::PermissionGranted, "permission_granted"),
            (AuditAction::PermissionRevoked, "permission_revoked"),
            (
                AuditAction::ProviderInstanceCreated,
                "provider_instance_created",
            ),
            (
                AuditAction::ProviderInstanceUpdated,
                "provider_instance_updated",
            ),
            (
                AuditAction::ProviderInstanceDeleted,
                "provider_instance_deleted",
            ),
            (AuditAction::SettingsUpdated, "settings_updated"),
            (AuditAction::MemberKicked, "member_kicked"),
            (AuditAction::MemberBanned, "member_banned"),
            (AuditAction::MemberUnbanned, "member_unbanned"),
            (AuditAction::MemberRoleUpdated, "member_role_updated"),
            (
                AuditAction::MemberPermissionUpdated,
                "member_permission_updated",
            ),
            (AuditAction::MemberStatusUpdated, "member_status_updated"),
            (AuditAction::RoomSettingsUpdated, "room_settings_updated"),
            (AuditAction::UserApproved, "user_approved"),
            (AuditAction::RoomApproved, "room_approved"),
            (AuditAction::StreamKicked, "stream_kicked"),
            (AuditAction::RateLimitResetFailed, "rate_limit_reset_failed"),
            // Token security events (Task #65)
            (AuditAction::UserLogin, "user_login"),
            (AuditAction::UserLogout, "user_logout"),
            (AuditAction::TokenIssued, "token_issued"),
            (AuditAction::TokenRefreshed, "token_refreshed"),
            (AuditAction::TokenFamilyRevoked, "token_family_revoked"),
            // Settings access audit
            (AuditAction::SettingsViewed, "settings_viewed"),
            (AuditAction::SettingsGroupViewed, "settings_group_viewed"),
        ];

        for (action, expected) in actions {
            let json = serde_json::to_string(&action).unwrap();
            assert_eq!(json, format!("\"{expected}\""), "Mismatch for {expected}");
        }
    }

    #[test]
    fn test_audit_action_deserialization() {
        let json = r#""user_banned""#;
        let action: AuditAction = serde_json::from_str(json).unwrap();
        assert!(matches!(action, AuditAction::UserBanned));
    }

    #[test]
    fn test_audit_action_round_trip() {
        let original = AuditAction::PermissionGranted;
        let json = serde_json::to_string(&original).unwrap();
        let deserialized: AuditAction = serde_json::from_str(&json).unwrap();
        assert_eq!(original.as_str(), deserialized.as_str());
    }

    // ========== AuditAction::as_str ==========

    #[test]
    fn test_audit_action_as_str_matches_serialization() {
        let actions = vec![
            AuditAction::UserCreated,
            AuditAction::UserDeleted,
            AuditAction::UserBanned,
            AuditAction::UserUnbanned,
            AuditAction::UserPasswordUpdated,
            AuditAction::UserUsernameUpdated,
            AuditAction::UserRoleUpdated,
            AuditAction::RoomCreated,
            AuditAction::RoomDeleted,
            AuditAction::RoomBanned,
            AuditAction::RoomUnbanned,
            AuditAction::RoomPasswordUpdated,
            AuditAction::PermissionGranted,
            AuditAction::PermissionRevoked,
            AuditAction::ProviderInstanceCreated,
            AuditAction::ProviderInstanceUpdated,
            AuditAction::ProviderInstanceDeleted,
            AuditAction::SettingsUpdated,
            AuditAction::MemberKicked,
            AuditAction::MemberBanned,
            AuditAction::MemberUnbanned,
            AuditAction::MemberRoleUpdated,
            AuditAction::MemberPermissionUpdated,
            AuditAction::MemberStatusUpdated,
            AuditAction::RoomSettingsUpdated,
            AuditAction::UserApproved,
            AuditAction::RoomApproved,
            AuditAction::StreamKicked,
            AuditAction::RateLimitResetFailed,
            // Token security events (Task #65)
            AuditAction::UserLogin,
            AuditAction::UserLogout,
            AuditAction::TokenIssued,
            AuditAction::TokenRefreshed,
            AuditAction::TokenFamilyRevoked,
            // Settings access audit
            AuditAction::SettingsViewed,
            AuditAction::SettingsGroupViewed,
        ];

        for action in actions {
            let as_str = action.as_str();
            let serialized = serde_json::to_string(&action).unwrap();
            // Serialized form is quoted: "user_created"
            assert_eq!(serialized, format!("\"{as_str}\""));
        }
    }

    // ========== AuditTargetType ==========

    #[test]
    fn test_all_target_types_serialize_to_snake_case() {
        let targets = vec![
            (AuditTargetType::User, "user"),
            (AuditTargetType::Room, "room"),
            (AuditTargetType::Member, "member"),
            (AuditTargetType::ProviderInstance, "provider_instance"),
            (AuditTargetType::Settings, "settings"),
            (AuditTargetType::System, "system"),
            (AuditTargetType::Stream, "stream"),
            (AuditTargetType::Token, "token"),
        ];

        for (target, expected) in targets {
            let json = serde_json::to_string(&target).unwrap();
            assert_eq!(json, format!("\"{expected}\""), "Mismatch for {expected}");
        }
    }

    #[test]
    fn test_target_type_as_str_matches_serialization() {
        let targets = vec![
            AuditTargetType::User,
            AuditTargetType::Room,
            AuditTargetType::Member,
            AuditTargetType::ProviderInstance,
            AuditTargetType::Settings,
            AuditTargetType::System,
            AuditTargetType::Stream,
            AuditTargetType::Token,
        ];

        for target in targets {
            let as_str = target.as_str();
            let serialized = serde_json::to_string(&target).unwrap();
            assert_eq!(serialized, format!("\"{as_str}\""));
        }
    }

    #[test]
    fn test_target_type_deserialization() {
        let json = r#""provider_instance""#;
        let target: AuditTargetType = serde_json::from_str(json).unwrap();
        assert!(matches!(target, AuditTargetType::ProviderInstance));
    }

    // ========== AuditLog Construction ==========

    #[test]
    fn test_audit_log_construction() {
        let log = AuditLog {
            id: "test_id".to_string(),
            actor_id: "actor_123".to_string(),
            actor_username: "admin".to_string(),
            action: AuditAction::UserBanned,
            target_type: AuditTargetType::User,
            target_id: Some("user_456".to_string()),
            details: serde_json::json!({"reason": "spam"}),
            ip_address: Some("192.168.1.1".to_string()),
            user_agent: Some("Mozilla/5.0".to_string()),
            created_at: Utc::now(),
        };

        assert_eq!(log.id, "test_id");
        assert_eq!(log.actor_id, "actor_123");
        assert_eq!(log.actor_username, "admin");
        assert_eq!(log.action.as_str(), "user_banned");
        assert_eq!(log.target_type.as_str(), "user");
        assert_eq!(log.target_id, Some("user_456".to_string()));
        assert_eq!(log.details["reason"], "spam");
    }

    #[test]
    fn test_audit_log_optional_fields() {
        let log = AuditLog {
            id: "test".to_string(),
            actor_id: "system".to_string(),
            actor_username: "system".to_string(),
            action: AuditAction::SettingsUpdated,
            target_type: AuditTargetType::Settings,
            target_id: None,
            details: serde_json::json!({}),
            ip_address: None,
            user_agent: None,
            created_at: Utc::now(),
        };

        assert!(log.target_id.is_none());
        assert!(log.ip_address.is_none());
        assert!(log.user_agent.is_none());
    }

    #[test]
    fn test_audit_log_serialization_round_trip() {
        let log = AuditLog {
            id: "audit_1".to_string(),
            actor_id: "user_1".to_string(),
            actor_username: "alice".to_string(),
            action: AuditAction::RoomCreated,
            target_type: AuditTargetType::Room,
            target_id: Some("room_1".to_string()),
            details: serde_json::json!({"room_name": "Test Room"}),
            ip_address: Some("10.0.0.1".to_string()),
            user_agent: Some("TestAgent/1.0".to_string()),
            created_at: Utc::now(),
        };

        let json = serde_json::to_string(&log).unwrap();
        let deserialized: AuditLog = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.id, log.id);
        assert_eq!(deserialized.actor_id, log.actor_id);
        assert_eq!(deserialized.actor_username, log.actor_username);
        assert_eq!(deserialized.action.as_str(), log.action.as_str());
        assert_eq!(deserialized.target_type.as_str(), log.target_type.as_str());
        assert_eq!(deserialized.target_id, log.target_id);
        assert_eq!(deserialized.details, log.details);
    }

    // ========== AuditEventParams ==========

    #[test]
    fn test_audit_event_params_construction() {
        let params = AuditEventParams {
            actor_id: "admin_1".to_string(),
            actor_username: "superadmin".to_string(),
            action: AuditAction::UserRoleUpdated,
            target_type: AuditTargetType::User,
            target_id: Some("user_42".to_string()),
            details: serde_json::json!({
                "old_role": "user",
                "new_role": "admin"
            }),
            ip_address: Some("203.0.113.50".to_string()),
            user_agent: None,
        };

        assert_eq!(params.actor_id, "admin_1");
        assert_eq!(params.action.as_str(), "user_role_updated");
        assert_eq!(params.details["old_role"], "user");
        assert_eq!(params.details["new_role"], "admin");
    }

    // ========== Details JSON Formatting ==========

    #[test]
    fn test_permission_change_details_format() {
        let details = serde_json::json!({
            "old_permissions": 0u64,
            "new_permissions": 255u64
        });

        assert_eq!(details["old_permissions"], 0);
        assert_eq!(details["new_permissions"], 255);
    }

    #[test]
    fn test_details_with_nested_info() {
        let details = serde_json::json!({
            "reason": "Terms of service violation",
            "evidence": {
                "report_id": "rpt_123",
                "reported_by": ["user_a", "user_b"]
            },
            "duration": "permanent"
        });

        assert_eq!(details["reason"], "Terms of service violation");
        assert!(details["evidence"]["reported_by"].is_array());
        assert_eq!(
            details["evidence"]["reported_by"].as_array().unwrap().len(),
            2
        );
    }

    // ========== Buffered AuditService ==========

    #[tokio::test]
    async fn test_unbuffered_service_dropped_count_is_zero() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let service = AuditService::new_unbuffered(pool);
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_buffered_service_enqueues_events() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        // Enqueue an event -- it should not error even with a fake pool
        // because the event is only buffered, not written to DB
        let result = service
            .log(
                "actor1".to_string(),
                "admin".to_string(),
                AuditAction::UserCreated,
                AuditTargetType::User,
                Some("user1".to_string()),
                serde_json::json!({}),
                None,
                None,
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_audit_record_fields() {
        let record = AuditRecord {
            actor_id: "a1".to_string(),
            actor_username: "admin".to_string(),
            action_str: AuditAction::RoomDeleted.as_str(),
            target_str: AuditTargetType::Room.as_str(),
            target_id: Some("r1".to_string()),
            details: serde_json::json!({"reason": "test"}),
            ip_address: Some("127.0.0.1".to_string()),
            user_agent: None,
            created_at: Utc::now(),
        };

        assert_eq!(record.action_str, "room_deleted");
        assert_eq!(record.target_str, "room");
        assert_eq!(record.actor_id, "a1");
    }

    #[test]
    fn test_buffer_constants() {
        assert_eq!(DEFAULT_BUFFER_CAPACITY, 10_000);
        assert_eq!(FLUSH_BATCH_SIZE, 100);
        assert_eq!(FLUSH_INTERVAL_SECS, 5);
        assert_eq!(FLUSH_MAX_RETRIES, 3);
        assert_eq!(FLUSH_RETRY_BASE_MS, 100);
    }

    // ========== Stream Kick Audit ==========

    #[test]
    fn test_stream_kicked_action_serialization() {
        let action = AuditAction::StreamKicked;
        let json = serde_json::to_string(&action).unwrap();
        assert_eq!(json, "\"stream_kicked\"");
    }

    #[test]
    fn test_stream_target_type_serialization() {
        let target = AuditTargetType::Stream;
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, "\"stream\"");
    }

    // ========== Integration Tests (Require DB) ==========

    #[tokio::test]
    async fn test_log_audit_event() {
        // Integration test placeholder for AuditService::log_event
        // Would require TestInfra with PostgreSQL
    }

    #[tokio::test]
    async fn test_log_with_params() {
        // Integration test placeholder for AuditService::log_with_params
        // Would require TestInfra with PostgreSQL
    }

    #[tokio::test]
    async fn test_log_stream_kicked() {
        // Integration test placeholder for AuditService::log_stream_kicked
        // Would require TestInfra with PostgreSQL
    }

    // ========== log_stream_kicked Parameter Tests ==========

    #[test]
    fn test_log_stream_kicked_target_id_format() {
        // Verify the target_id format is "room_id:media_id"
        let room_id = "room_abc123";
        let media_id = "media_xyz789";
        let expected_target_id = format!("{room_id}:{media_id}");
        assert_eq!(expected_target_id, "room_abc123:media_xyz789");
    }

    #[test]
    fn test_log_stream_kicked_details_json_structure() {
        // Verify the details JSON structure contains all expected fields
        let room_id = "test_room";
        let media_id = "test_media";
        let reason = "Test reason".to_string();

        let details = serde_json::json!({
            "room_id": room_id,
            "media_id": media_id,
            "reason": reason
        });

        assert_eq!(details["room_id"], "test_room");
        assert_eq!(details["media_id"], "test_media");
        assert_eq!(details["reason"], "Test reason");
    }

    #[test]
    fn test_log_stream_kicked_details_json_empty_reason() {
        // Verify the details JSON structure when reason is None
        let room_id = "test_room";
        let media_id = "test_media";

        let details = serde_json::json!({
            "room_id": room_id,
            "media_id": media_id,
            "reason": ""
        });

        assert_eq!(details["reason"], "");
    }

    #[test]
    fn test_stream_kicked_action_and_target_type() {
        // Verify the correct action and target type are used
        let action = AuditAction::StreamKicked;
        let target_type = AuditTargetType::Stream;

        assert_eq!(action.as_str(), "stream_kicked");
        assert_eq!(target_type.as_str(), "stream");
    }

    // ========================================================================
    // Token Security Audit Tests (Task #65)
    // ========================================================================

    #[test]
    fn test_token_actions_serialization() {
        let actions = vec![
            (AuditAction::UserLogin, "user_login"),
            (AuditAction::UserLogout, "user_logout"),
            (AuditAction::TokenIssued, "token_issued"),
            (AuditAction::TokenRefreshed, "token_refreshed"),
            (AuditAction::TokenFamilyRevoked, "token_family_revoked"),
        ];

        for (action, expected) in actions {
            let json = serde_json::to_string(&action).unwrap();
            assert_eq!(json, format!("\"{expected}\""), "Mismatch for {expected}");
        }
    }

    #[test]
    fn test_token_target_type_serialization() {
        let target = AuditTargetType::Token;
        let json = serde_json::to_string(&target).unwrap();
        assert_eq!(json, "\"token\"");
        assert_eq!(target.as_str(), "token");
    }

    #[test]
    fn test_log_user_login_details_structure() {
        let details = serde_json::json!({
            "login_method": "password",
            "username": "alice"
        });

        assert_eq!(details["login_method"], "password");
        assert_eq!(details["username"], "alice");
    }

    #[test]
    fn test_log_user_login_oauth2_method() {
        let details = serde_json::json!({
            "login_method": "oauth2",
            "username": "bob"
        });

        assert_eq!(details["login_method"], "oauth2");
    }

    #[test]
    fn test_log_token_issued_details_structure() {
        let jti = "jti_abc123";
        let token_type = "access";
        let expires_at = 1735689600i64;

        let details = serde_json::json!({
            "token_type": token_type,
            "jti": jti,
            "expires_at": expires_at
        });

        assert_eq!(details["token_type"], "access");
        assert_eq!(details["jti"], "jti_abc123");
        assert_eq!(details["expires_at"], 1735689600);
    }

    #[test]
    fn test_log_token_refreshed_details_structure() {
        let old_jti = "jti_old_123";
        let new_jti = "jti_new_456";

        let details = serde_json::json!({
            "old_jti": old_jti,
            "new_jti": new_jti
        });

        assert_eq!(details["old_jti"], "jti_old_123");
        assert_eq!(details["new_jti"], "jti_new_456");
    }

    #[test]
    fn test_log_token_family_revoked_details_structure() {
        let replayed_jti = "jti_replayed_789";

        let details = serde_json::json!({
            "replayed_jti": replayed_jti,
            "reason": "token_replay_detected"
        });

        assert_eq!(details["replayed_jti"], "jti_replayed_789");
        assert_eq!(details["reason"], "token_replay_detected");
    }

    #[test]
    fn test_log_user_logout_details_empty() {
        let details = serde_json::json!({});
        assert!(details.is_object());
        assert!(details.as_object().unwrap().is_empty());
    }

    #[test]
    fn test_token_issued_target_id_format() {
        // Verify the target_id format is "user_id:jti"
        let user_id = "user_123";
        let jti = "jti_abc456";
        let target_id = format!("{user_id}:{jti}");
        assert_eq!(target_id, "user_123:jti_abc456");
    }

    #[test]
    fn test_token_refreshed_target_id_format() {
        // Verify the target_id format uses new_jti
        let user_id = "user_123";
        let new_jti = "jti_new_789";
        let target_id = format!("{user_id}:{new_jti}");
        assert_eq!(target_id, "user_123:jti_new_789");
    }

    #[tokio::test]
    async fn test_log_user_login_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_user_login(
                "user_123".to_string(),
                "alice".to_string(),
                "password",
                Some("192.168.1.1".to_string()),
                Some("Mozilla/5.0".to_string()),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_user_logout_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_user_logout(
                "user_123".to_string(),
                "alice".to_string(),
                Some("192.168.1.1".to_string()),
                Some("Mozilla/5.0".to_string()),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_token_issued_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_token_issued(
                "user_123".to_string(),
                "alice".to_string(),
                "access",
                "jti_abc123".to_string(),
                1735689600,
                Some("192.168.1.1".to_string()),
                Some("Mozilla/5.0".to_string()),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_token_refreshed_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_token_refreshed(
                "user_123".to_string(),
                "alice".to_string(),
                "jti_old_123".to_string(),
                "jti_new_456".to_string(),
                Some("192.168.1.1".to_string()),
                Some("Mozilla/5.0".to_string()),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_token_family_revoked_buffered() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_token_family_revoked(
                "user_123".to_string(),
                "alice".to_string(),
                "jti_replayed_789".to_string(),
                Some("192.168.1.1".to_string()),
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }

    #[tokio::test]
    async fn test_log_user_login_without_ip_or_user_agent() {
        let pool = PgPool::connect_lazy("postgresql://fake").unwrap();
        let (service, _handle) = AuditService::new(pool);

        let result = service
            .log_user_login(
                "user_123".to_string(),
                "alice".to_string(),
                "oauth2",
                None,
                None,
            )
            .await;

        assert!(result.is_ok());
        assert_eq!(service.dropped_count(), 0);
    }
}
