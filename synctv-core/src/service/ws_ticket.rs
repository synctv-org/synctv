//! WebSocket Ticket Service
//!
//! Provides short-lived, one-time-use tickets for WebSocket authentication.
//! This is more secure than passing JWT tokens directly in WebSocket URLs,
//! as tickets:
//! - Are short-lived (default 30 seconds)
//! - Can only be used once
//! - Don't expose the actual JWT token in URLs/logs
//!
//! ## Storage Backends
//!
//! - **Redis** (recommended for multi-replica): Tickets are stored in Redis with TTL,
//!   ensuring they work across all replicas.
//! - **Memory** (single-replica only): Tickets are stored in memory. This is suitable
//!   for single-instance deployments but will not work correctly with multiple replicas.
//!
//! ## Security: TOCTOU Prevention
//!
//! The `validate_and_consume_checked` method accepts a user validator callback
//! so callers can reject banned/deleted users without burning an otherwise
//! valid one-time ticket on a retriable authorization failure.

use async_trait::async_trait;
use base64::Engine;
use rand::RngExt;
use std::future::Future;
use std::sync::Arc;
use synctv_common::ExecutionControl;
use tracing::debug;

use crate::models::{RoomId, UserId};
use crate::{Error, RedisConnectionRuntime, Result, SharedStateMode, SharedStateProfile};

mod memory_store;
mod redis_store;
mod store;
mod types;

pub use memory_store::InMemoryTicketStore;
#[cfg(test)]
use redis_store::run_ws_ticket_redis_op;
pub use redis_store::RedisTicketStore;
pub use store::TicketStore;
pub use types::{
    CreateGuestTicketRequest, PendingValidatedTicket, UserValidationResult, ValidatedGuestTicket,
    ValidatedTicket, WsTicketData,
};

const INVALID_OR_EXPIRED_TICKET_MESSAGE: &str = "Invalid or expired ticket";
const AUTHENTICATION_FAILED_MESSAGE: &str = "Authentication failed";

/// Trait for validating user status during ticket consumption.
///
/// Implemented by the caller (typically `UserService`) to check user status
/// atomically with ticket validation, preventing TOCTOU race conditions.
#[async_trait]
pub trait UserValidator: Send + Sync {
    /// Validate user for ticket-based authentication.
    ///
    /// Returns `Ok(UserValidationResult)` if the user is valid (active status,
    /// not deleted) or `Err` if the user should be rejected (banned, pending,
    /// deleted, or not found).
    async fn validate_for_ticket(&self, user_id: &UserId) -> Result<UserValidationResult>;
}

/// Default ticket TTL in seconds
const DEFAULT_TICKET_TTL_SECS: u64 = 30;
/// Ticket length in bytes (256 bits of entropy)
const TICKET_LENGTH: usize = 32;

fn now_unix_seconds() -> u64 {
    u64::try_from(crate::SystemClock.now().timestamp()).unwrap_or(0)
}

fn normalize_ticket_ttl_secs(ticket_ttl_secs: Option<u64>) -> u64 {
    match ticket_ttl_secs {
        Some(0) => {
            tracing::warn!(
                default_ttl_secs = DEFAULT_TICKET_TTL_SECS,
                "WebSocket ticket TTL must be positive; using default TTL"
            );
            DEFAULT_TICKET_TTL_SECS
        }
        Some(ttl) => ttl,
        None => DEFAULT_TICKET_TTL_SECS,
    }
}

/// Service for creating and validating WebSocket tickets.
///
/// Uses a pluggable `TicketStore` backend (Redis or in-memory).
#[derive(Clone)]
pub struct WsTicketService {
    store: Arc<dyn TicketStore>,
    /// Ticket TTL in seconds
    ticket_ttl_secs: u64,
}

#[async_trait]
pub trait WebSocketTicketService: Send + Sync {
    fn ticket_ttl_secs(&self) -> u64;

    fn supports_cluster_runtime(&self) -> bool;

    async fn create_ticket(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        password_version: i32,
    ) -> Result<String>;

    async fn create_ticket_with_control(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        password_version: i32,
        control: Option<&ExecutionControl>,
    ) -> Result<String>;

    async fn create_guest_ticket_with_control(
        &self,
        request: CreateGuestTicketRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<String>;

    async fn validate_and_consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<ValidatedTicket>;

    async fn validate_and_consume_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<ValidatedTicket>;

    async fn validate_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<PendingValidatedTicket>;

    async fn validate_checked_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
        control: Option<&ExecutionControl>,
    ) -> Result<PendingValidatedTicket>;

    async fn consume_prevalidated(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        pending: &PendingValidatedTicket,
    ) -> Result<ValidatedTicket>;

    async fn consume_prevalidated_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        pending: &PendingValidatedTicket,
        control: Option<&ExecutionControl>,
    ) -> Result<ValidatedTicket>;
}

impl std::fmt::Debug for WsTicketService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsTicketService")
            .field("cross_node_capable", &self.store.supports_cluster_runtime())
            .field("ticket_ttl_secs", &self.ticket_ttl_secs)
            .finish()
    }
}

impl WsTicketService {
    async fn run_with_control<T, F>(control: Option<&ExecutionControl>, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        match control {
            Some(control) => control.run(future).await.map_err(Error::from)?,
            None => future.await,
        }
    }

    fn ensure_ticket_room_matches(
        ticket_data: &WsTicketData,
        expected_room_id: &RoomId,
        cross_node_capable: bool,
    ) -> Result<()> {
        if ticket_data.room_id != expected_room_id.to_string() {
            debug!(
                ticket_room = %ticket_data.room_id,
                expected_room = %expected_room_id,
                cross_node_capable,
                "WebSocket ticket rejected: room mismatch"
            );
            return Err(Error::Authorization(
                "Ticket not valid for this room".to_string(),
            ));
        }

        Ok(())
    }

    /// Create a new WebSocket ticket service with a custom ticket store backend.
    pub fn from_store(store: Arc<dyn TicketStore>, ticket_ttl_secs: Option<u64>) -> Self {
        Self {
            store,
            ticket_ttl_secs: normalize_ticket_ttl_secs(ticket_ttl_secs),
        }
    }

    fn with_redis_runtime(
        redis_runtime: Arc<dyn RedisConnectionRuntime>,
        key_prefix: impl Into<String>,
        ticket_ttl_secs: Option<u64>,
    ) -> Self {
        Self::from_store(
            Arc::new(RedisTicketStore::from_runtime(redis_runtime, key_prefix)),
            ticket_ttl_secs,
        )
    }

    fn with_memory(ticket_ttl_secs: Option<u64>) -> Self {
        Self::from_store(Arc::new(InMemoryTicketStore::new()), ticket_ttl_secs)
    }

    #[must_use]
    pub fn local_only(ticket_ttl_secs: Option<u64>) -> Self {
        Self::with_memory(ticket_ttl_secs)
    }

    pub(crate) fn from_shared_state_profile(
        profile: &SharedStateProfile,
        ticket_ttl_secs: Option<u64>,
    ) -> Result<Self> {
        match profile.state_mode() {
            SharedStateMode::SharedRequired => Ok(Self::with_redis_runtime(
                profile.require_shared_runtime("WebSocket ticket storage")?,
                profile.key_prefix(),
                ticket_ttl_secs,
            )),
            SharedStateMode::SharedBestEffort => Ok(Self::with_redis_runtime(
                profile.best_effort_shared_runtime("WebSocket ticket storage")?,
                profile.key_prefix(),
                ticket_ttl_secs,
            )),
            SharedStateMode::LocalOnly => Ok(Self::with_memory(ticket_ttl_secs)),
        }
    }

    /// Get the configured ticket TTL in seconds
    #[must_use]
    pub const fn ticket_ttl_secs(&self) -> u64 {
        self.ticket_ttl_secs
    }

    /// Whether the configured store is safe to use when cluster runtime is enabled.
    #[must_use]
    pub fn supports_cluster_runtime(&self) -> bool {
        self.store.supports_cluster_runtime()
    }

    /// Create a new ticket for a user bound to a specific room.
    ///
    /// Returns a ticket string that can be used once for WebSocket authentication.
    /// The ticket expires after `ticket_ttl_secs` seconds and is only valid for
    /// the supplied `room_id`.
    ///
    /// The `password_version` is stored in the ticket and validated during consumption
    /// to ensure tickets are invalidated when the user changes their password.
    pub async fn create_ticket(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        password_version: i32,
    ) -> Result<String> {
        self.create_ticket_with_control(user_id, room_id, password_version, None)
            .await
    }

    pub async fn create_ticket_with_control(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        password_version: i32,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        let ticket = Self::generate_ticket();

        let ticket_data = WsTicketData::user(user_id, room_id, password_version);

        Self::run_with_control(
            control,
            self.store
                .store(&ticket, &ticket_data, self.ticket_ttl_secs),
        )
        .await?;

        debug!(
            user_id = %user_id,
            ttl_secs = self.ticket_ttl_secs,
            cross_node_capable = self.store.supports_cluster_runtime(),
            "WebSocket ticket created"
        );

        Ok(ticket)
    }

    pub async fn create_guest_ticket_with_control(
        &self,
        request: CreateGuestTicketRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        let ticket = Self::generate_ticket();
        let ticket_data = WsTicketData::guest(
            &request.room_id,
            request.guest_id,
            request.display_name,
            request.session_id,
            request.token_jti,
            request.room_guest_version,
            request.permissions,
        );

        Self::run_with_control(
            control,
            self.store
                .store(&ticket, &ticket_data, self.ticket_ttl_secs),
        )
        .await?;

        debug!(
            principal = %ticket_data.principal.user_id_for_log(),
            ttl_secs = self.ticket_ttl_secs,
            cross_node_capable = self.store.supports_cluster_runtime(),
            "Guest WebSocket ticket created"
        );

        Ok(ticket)
    }

    /// Validate and consume a ticket.
    ///
    /// Returns [`ValidatedTicket`] containing the user ID and password version if valid
    /// and the ticket's `room_id` matches the expected `room_id`. The ticket is deleted
    /// after use (one-time use). Passing a ticket for a different room returns an error
    /// so that tickets cannot be replayed across rooms.
    ///
    /// The caller is responsible for checking that the `password_version` in the returned
    /// [`ValidatedTicket`] matches the current user's password version to ensure the ticket
    /// is invalidated if the user changed their password after the ticket was issued.
    pub async fn validate_and_consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<ValidatedTicket> {
        self.validate_and_consume_with_control(ticket, expected_room_id, None)
            .await
    }

    pub async fn validate_and_consume_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        control: Option<&ExecutionControl>,
    ) -> Result<ValidatedTicket> {
        let cross_node_capable = self.store.supports_cluster_runtime();

        let Some(ticket_data) = Self::run_with_control(control, self.store.load(ticket)).await?
        else {
            debug!(
                ticket = %ticket,
                cross_node_capable,
                "WebSocket ticket not found or expired"
            );
            return Err(Error::Authentication(
                INVALID_OR_EXPIRED_TICKET_MESSAGE.to_string(),
            ));
        };

        Self::ensure_ticket_room_matches(&ticket_data, expected_room_id, cross_node_capable)?;

        if !Self::run_with_control(control, self.store.claim(ticket, &ticket_data)).await? {
            debug!(
                ticket = %ticket,
                cross_node_capable,
                "WebSocket ticket already consumed during validation"
            );
            return Err(Error::Authentication(
                INVALID_OR_EXPIRED_TICKET_MESSAGE.to_string(),
            ));
        }

        debug!(
            principal = %ticket_data.principal.user_id_for_log(),
            room_id = %ticket_data.room_id,
            cross_node_capable,
            "WebSocket ticket validated and consumed"
        );

        ticket_data.into_validated()
    }

    /// Validate and consume a ticket with user status check.
    ///
    /// This is the recommended method for WebSocket ticket validation. It:
    /// 1. Validates room-scoped ticket presence
    /// 2. Calls the `user_validator` to check user status and password version
    /// 3. Consumes the ticket only after all validation succeeds
    ///
    /// Returns [`ValidatedTicket`] if all checks pass.
    pub async fn validate_and_consume_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<ValidatedTicket> {
        self.validate_and_consume_checked_with_control(
            ticket,
            expected_room_id,
            user_validator,
            None,
        )
        .await
    }

    pub async fn validate_and_consume_checked_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
        control: Option<&ExecutionControl>,
    ) -> Result<ValidatedTicket> {
        let cross_node_capable = self.store.supports_cluster_runtime();

        let pending = self
            .validate_checked_with_control(ticket, expected_room_id, user_validator, control)
            .await?;

        if !Self::run_with_control(control, self.store.claim(ticket, pending.ticket_data())).await?
        {
            debug!(
                ticket = %ticket,
                cross_node_capable,
                "WebSocket ticket already consumed during checked validation"
            );
            return Err(Error::Authentication(
                INVALID_OR_EXPIRED_TICKET_MESSAGE.to_string(),
            ));
        }

        debug!(
            principal = %pending.principal_for_log(),
            room_id = %pending.ticket_data().room_id,
            cross_node_capable,
            "WebSocket ticket validated and consumed with principal check"
        );

        Ok(pending.to_validated())
    }

    /// Validate a ticket and user state without consuming the ticket yet.
    ///
    /// This is intended for handshake flows that still have additional checks
    /// before the connection is definitively established. The caller must later
    /// call [`Self::consume_prevalidated`] to preserve one-time-use semantics.
    pub async fn validate_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<PendingValidatedTicket> {
        self.validate_checked_with_control(ticket, expected_room_id, user_validator, None)
            .await
    }

    pub async fn validate_checked_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
        control: Option<&ExecutionControl>,
    ) -> Result<PendingValidatedTicket> {
        let cross_node_capable = self.store.supports_cluster_runtime();

        let Some(ticket_data) = Self::run_with_control(control, self.store.load(ticket)).await?
        else {
            debug!(
                ticket = %ticket,
                cross_node_capable,
                "WebSocket ticket not found or expired"
            );
            return Err(Error::Authentication(
                INVALID_OR_EXPIRED_TICKET_MESSAGE.to_string(),
            ));
        };

        Self::ensure_ticket_room_matches(&ticket_data, expected_room_id, cross_node_capable)?;

        let Ok((user_id, ticket_password_version)) = ticket_data.user_principal() else {
            debug!(
                principal = %ticket_data.principal.user_id_for_log(),
                room_id = %ticket_data.room_id,
                cross_node_capable,
                "Guest WebSocket ticket prevalidated without user password check"
            );
            let guest = ticket_data.principal.clone().into_validated_guest()?;
            return Ok(PendingValidatedTicket::Guest { guest, ticket_data });
        };

        let user_validation =
            Self::run_with_control(control, user_validator.validate_for_ticket(&user_id))
                .await
                .map_err(|e| {
                    debug!(
                        user_id = %user_id,
                        error = %e,
                        cross_node_capable,
                        "WebSocket ticket rejected: user validation failed"
                    );
                    match crate::service::SecurityPipeline::classify_auth_error(&e) {
                        crate::service::AuthErrorCategory::Authentication
                        | crate::service::AuthErrorCategory::Authorization => {
                            Error::Authentication(AUTHENTICATION_FAILED_MESSAGE.to_string())
                        }
                        crate::service::AuthErrorCategory::Unavailable
                        | crate::service::AuthErrorCategory::Internal => e,
                    }
                })?;

        // Check password version after loading the current user state.
        if ticket_password_version < user_validation.password_version {
            debug!(
                user_id = %user_id,
                ticket_pv = ticket_password_version,
                current_pv = user_validation.password_version,
                cross_node_capable,
                "WebSocket ticket rejected: password changed after ticket issued"
            );
            return Err(Error::Authentication(
                AUTHENTICATION_FAILED_MESSAGE.to_string(),
            ));
        }

        debug!(
            user_id = %user_id,
            room_id = %ticket_data.room_id,
            cross_node_capable,
            "WebSocket ticket prevalidated with user check"
        );

        Ok(PendingValidatedTicket::User {
            user_id,
            password_version: ticket_password_version,
            ticket_data,
        })
    }

    /// Consume a previously prevalidated ticket.
    pub async fn consume_prevalidated(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        pending: &PendingValidatedTicket,
    ) -> Result<ValidatedTicket> {
        self.consume_prevalidated_with_control(ticket, expected_room_id, pending, None)
            .await
    }

    pub async fn consume_prevalidated_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        pending: &PendingValidatedTicket,
        control: Option<&ExecutionControl>,
    ) -> Result<ValidatedTicket> {
        let cross_node_capable = self.store.supports_cluster_runtime();

        Self::ensure_ticket_room_matches(
            pending.ticket_data(),
            expected_room_id,
            cross_node_capable,
        )?;

        if !Self::run_with_control(control, self.store.claim(ticket, pending.ticket_data())).await?
        {
            debug!(
                ticket = %ticket,
                cross_node_capable,
                "WebSocket ticket already consumed before final handshake commit"
            );
            return Err(Error::Authentication(
                INVALID_OR_EXPIRED_TICKET_MESSAGE.to_string(),
            ));
        }

        debug!(
            principal = %pending.principal_for_log(),
            room_id = %pending.ticket_data().room_id,
            cross_node_capable,
            "WebSocket ticket consumed after prevalidated handshake succeeded"
        );

        Ok(pending.to_validated())
    }

    /// Generate a secure random ticket string
    fn generate_ticket() -> String {
        let mut rng = rand::rng();
        let mut bytes = [0u8; TICKET_LENGTH];
        rng.fill(&mut bytes);
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }
}

#[async_trait]
impl WebSocketTicketService for WsTicketService {
    fn ticket_ttl_secs(&self) -> u64 {
        self.ticket_ttl_secs()
    }

    fn supports_cluster_runtime(&self) -> bool {
        self.supports_cluster_runtime()
    }

    async fn create_ticket(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        password_version: i32,
    ) -> Result<String> {
        WsTicketService::create_ticket(self, user_id, room_id, password_version).await
    }

    async fn create_ticket_with_control(
        &self,
        user_id: &UserId,
        room_id: &RoomId,
        password_version: i32,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        WsTicketService::create_ticket_with_control(
            self,
            user_id,
            room_id,
            password_version,
            control,
        )
        .await
    }

    async fn create_guest_ticket_with_control(
        &self,
        request: CreateGuestTicketRequest,
        control: Option<&ExecutionControl>,
    ) -> Result<String> {
        WsTicketService::create_guest_ticket_with_control(self, request, control).await
    }

    async fn validate_and_consume(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
    ) -> Result<ValidatedTicket> {
        WsTicketService::validate_and_consume(self, ticket, expected_room_id).await
    }

    async fn validate_and_consume_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<ValidatedTicket> {
        WsTicketService::validate_and_consume_checked(
            self,
            ticket,
            expected_room_id,
            user_validator,
        )
        .await
    }

    async fn validate_checked(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
    ) -> Result<PendingValidatedTicket> {
        WsTicketService::validate_checked(self, ticket, expected_room_id, user_validator).await
    }

    async fn validate_checked_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        user_validator: &dyn UserValidator,
        control: Option<&ExecutionControl>,
    ) -> Result<PendingValidatedTicket> {
        WsTicketService::validate_checked_with_control(
            self,
            ticket,
            expected_room_id,
            user_validator,
            control,
        )
        .await
    }

    async fn consume_prevalidated(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        pending: &PendingValidatedTicket,
    ) -> Result<ValidatedTicket> {
        WsTicketService::consume_prevalidated(self, ticket, expected_room_id, pending).await
    }

    async fn consume_prevalidated_with_control(
        &self,
        ticket: &str,
        expected_room_id: &RoomId,
        pending: &PendingValidatedTicket,
        control: Option<&ExecutionControl>,
    ) -> Result<ValidatedTicket> {
        WsTicketService::consume_prevalidated_with_control(
            self,
            ticket,
            expected_room_id,
            pending,
            control,
        )
        .await
    }
}

#[cfg(test)]
mod tests;
