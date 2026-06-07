use serde::{Deserialize, Serialize};

use crate::models::{RoomId, RoomPermissionSet, UserId};
use crate::{Error, Result};

use super::{now_unix_seconds, AUTHENTICATION_FAILED_MESSAGE};

/// User validation result returned by `UserValidator` callback
#[derive(Debug, Clone)]
pub struct UserValidationResult {
    /// Current password version of the user
    pub password_version: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsTicketPrincipal {
    User {
        user_id: String,
        password_version: i32,
    },
    Guest {
        guest_id: String,
        display_name: String,
        session_id: String,
        token_jti: String,
        room_guest_version: i64,
        permissions: u64,
    },
}

/// WebSocket ticket data
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WsTicketData {
    /// Principal associated with this ticket.
    ///
    /// User tickets are invalidated by password-version checks during
    /// consumption. Guest tickets are room-bound and carry the validated guest
    /// realtime identity so clients do not need to send long-lived guest tokens
    /// during the WebSocket handshake.
    pub principal: WsTicketPrincipal,
    /// Room ID the ticket is bound to.
    ///
    /// Tickets are room-scoped: a ticket created for room A cannot be used to
    /// authenticate a WebSocket connection to room B.
    pub room_id: String,
    /// When the ticket was created (Unix timestamp)
    pub created_at: u64,
}

/// Outcome of a successful ticket validation.
#[derive(Debug, Clone)]
pub enum ValidatedTicket {
    User {
        user_id: UserId,
        password_version: i32,
    },
    Guest(ValidatedGuestTicket),
}

#[derive(Debug, Clone)]
pub struct ValidatedGuestTicket {
    pub guest_id: String,
    pub display_name: String,
    pub session_id: String,
    pub token_jti: String,
    pub room_guest_version: i64,
    pub permissions: RoomPermissionSet,
}

pub struct CreateGuestTicketRequest {
    pub room_id: RoomId,
    pub guest_id: String,
    pub display_name: String,
    pub session_id: String,
    pub token_jti: String,
    pub room_guest_version: i64,
    pub permissions: RoomPermissionSet,
}

/// Outcome of a successful pre-validation before the ticket is finally consumed.
#[derive(Debug, Clone)]
pub enum PendingValidatedTicket {
    User {
        user_id: UserId,
        password_version: i32,
        ticket_data: WsTicketData,
    },
    Guest {
        guest: ValidatedGuestTicket,
        ticket_data: WsTicketData,
    },
}

impl PendingValidatedTicket {
    pub fn ticket_data(&self) -> &WsTicketData {
        match self {
            Self::User { ticket_data, .. } | Self::Guest { ticket_data, .. } => ticket_data,
        }
    }

    pub fn principal_for_log(&self) -> &str {
        self.ticket_data().principal.user_id_for_log()
    }

    pub(super) fn to_validated(&self) -> ValidatedTicket {
        match self {
            Self::User {
                user_id,
                password_version,
                ..
            } => ValidatedTicket::User {
                user_id: *user_id,
                password_version: *password_version,
            },
            Self::Guest { guest, .. } => ValidatedTicket::Guest(guest.clone()),
        }
    }
}

impl ValidatedTicket {
    pub fn user_id(&self) -> Option<UserId> {
        match self {
            Self::User { user_id, .. } => Some(*user_id),
            Self::Guest(_) => None,
        }
    }

    pub fn password_version(&self) -> Option<i32> {
        match self {
            Self::User {
                password_version, ..
            } => Some(*password_version),
            Self::Guest(_) => None,
        }
    }
}

impl WsTicketPrincipal {
    pub(super) fn user_id_for_log(&self) -> &str {
        match self {
            Self::User { user_id, .. } => user_id,
            Self::Guest { guest_id, .. } => guest_id,
        }
    }

    pub(super) fn into_validated_guest(self) -> Result<ValidatedGuestTicket> {
        match self {
            Self::Guest {
                guest_id,
                display_name,
                session_id,
                token_jti,
                room_guest_version,
                permissions,
            } => Ok(ValidatedGuestTicket {
                guest_id,
                display_name,
                session_id,
                token_jti,
                room_guest_version,
                permissions: RoomPermissionSet(permissions),
            }),
            Self::User { .. } => Err(Error::Authentication(
                AUTHENTICATION_FAILED_MESSAGE.to_string(),
            )),
        }
    }
}

impl WsTicketData {
    pub(super) fn user(user_id: &UserId, room_id: &RoomId, password_version: i32) -> Self {
        Self {
            principal: WsTicketPrincipal::User {
                user_id: user_id.to_string(),
                password_version,
            },
            room_id: room_id.to_string(),
            created_at: now_unix_seconds(),
        }
    }

    pub fn guest(
        room_id: &RoomId,
        guest_id: impl Into<String>,
        display_name: impl Into<String>,
        session_id: impl Into<String>,
        token_jti: impl Into<String>,
        room_guest_version: i64,
        permissions: RoomPermissionSet,
    ) -> Self {
        Self {
            principal: WsTicketPrincipal::Guest {
                guest_id: guest_id.into(),
                display_name: display_name.into(),
                session_id: session_id.into(),
                token_jti: token_jti.into(),
                room_guest_version,
                permissions: permissions.0,
            },
            room_id: room_id.to_string(),
            created_at: now_unix_seconds(),
        }
    }

    pub(super) fn user_principal(&self) -> Result<(UserId, i32)> {
        match &self.principal {
            WsTicketPrincipal::User {
                user_id,
                password_version,
            } => Ok((user_id.parse().map_err(Error::Internal)?, *password_version)),
            WsTicketPrincipal::Guest { .. } => Err(Error::Authentication(
                AUTHENTICATION_FAILED_MESSAGE.to_string(),
            )),
        }
    }

    pub(super) fn into_validated(self) -> Result<ValidatedTicket> {
        match self.principal {
            WsTicketPrincipal::User {
                user_id,
                password_version,
            } => Ok(ValidatedTicket::User {
                user_id: user_id.parse().map_err(Error::Internal)?,
                password_version,
            }),
            principal @ WsTicketPrincipal::Guest { .. } => {
                Ok(ValidatedTicket::Guest(principal.into_validated_guest()?))
            }
        }
    }
}
