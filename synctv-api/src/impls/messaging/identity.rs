use synctv_core::models::{RoomId, RoomPermissionSet, UserId};

use crate::impls::client::{GuestRoomAccess, RoomActor};

pub(crate) const GUEST_INTERNAL_USER_ID_BASE: i64 = 8_000_000_000_000_000_000;
pub(crate) const GUEST_INTERNAL_USER_ID_SPAN: u64 = 500_000_000_000_000_000;

#[derive(Debug, Clone)]
pub struct GuestRealtimeIdentity {
    pub guest_id: String,
    pub display_name: String,
    pub session_id: String,
    pub token_jti: String,
    pub room_guest_version: i64,
    pub permissions: RoomPermissionSet,
}

#[derive(Debug, Clone)]
pub enum RealtimePrincipal {
    User {
        user_id: UserId,
        username: String,
    },
    Guest {
        internal_user_id: UserId,
        identity: GuestRealtimeIdentity,
    },
}

impl RealtimePrincipal {
    #[must_use]
    pub fn user(user_id: UserId, username: String) -> Self {
        Self::User { user_id, username }
    }

    pub fn guest(
        room_id: RoomId,
        identity: GuestRealtimeIdentity,
    ) -> Result<Self, RealtimeJoinError> {
        Ok(Self::Guest {
            internal_user_id: internal_guest_user_id(room_id, &identity.session_id)?,
            identity,
        })
    }

    #[must_use]
    pub fn connection_user_id(&self) -> UserId {
        match self {
            Self::User { user_id, .. } => *user_id,
            Self::Guest {
                internal_user_id, ..
            } => *internal_user_id,
        }
    }

    #[must_use]
    pub fn username(&self) -> &str {
        match self {
            Self::User { username, .. } => username,
            Self::Guest { identity, .. } => &identity.display_name,
        }
    }

    pub(super) fn public_actor_id(
        &self,
        public_id_codec: &crate::public_id::PublicIdCodec,
    ) -> Result<String, String> {
        match self {
            Self::User { user_id, .. } => public_id_codec
                .encode_user_id(*user_id)
                .map_err(|error| format!("Failed to encode user public id: {error}")),
            Self::Guest { identity, .. } => Ok(identity.guest_id.clone()),
        }
    }

    #[must_use]
    pub(super) fn room_actor(&self, room_id: RoomId) -> RoomActor {
        match self {
            Self::User { user_id, .. } => RoomActor::User {
                room_id,
                user_id: *user_id,
            },
            Self::Guest { identity, .. } => RoomActor::Guest(GuestRoomAccess {
                room_id,
                guest_id: identity.guest_id.clone(),
                display_name: identity.display_name.clone(),
                session_id: identity.session_id.clone(),
                token_jti: identity.token_jti.clone(),
                permissions: identity.permissions,
                room_guest_version: identity.room_guest_version,
            }),
        }
    }

    #[must_use]
    pub(super) fn is_guest(&self) -> bool {
        matches!(self, Self::Guest { .. })
    }

    #[must_use]
    pub(super) fn guest_identity(&self) -> Option<&GuestRealtimeIdentity> {
        match self {
            Self::Guest { identity, .. } => Some(identity),
            Self::User { .. } => None,
        }
    }
}

pub(crate) fn internal_guest_user_id(
    room_id: RoomId,
    session_id: &str,
) -> Result<UserId, RealtimeJoinError> {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    "synctv:guest:v1".hash(&mut hasher);
    room_id.hash(&mut hasher);
    session_id.hash(&mut hasher);
    let offset = hasher.finish() % GUEST_INTERNAL_USER_ID_SPAN;
    let offset = i64::try_from(offset).map_err(|_| {
        RealtimeJoinError::Internal("Guest internal user id span exceeds i64".to_string())
    })?;
    UserId::try_from(GUEST_INTERNAL_USER_ID_BASE + offset).map_err(|error| {
        RealtimeJoinError::Internal(format!("Guest internal user id is invalid: {error}"))
    })
}

#[must_use]
pub fn guest_public_id(session_id: &str) -> String {
    format!("gst_{session_id}")
}

#[must_use]
pub fn guest_display_name(session_id: &str) -> String {
    let short = session_id.chars().take(6).collect::<String>();
    format!("Guest {short}")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealtimeJoinError {
    InvalidInput(String),
    PermissionDenied(String),
    RateLimited(String),
    ServiceUnavailable(String),
    Internal(String),
}

impl RealtimeJoinError {
    #[must_use]
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidInput(message)
            | Self::PermissionDenied(message)
            | Self::RateLimited(message)
            | Self::ServiceUnavailable(message)
            | Self::Internal(message) => message,
        }
    }

    pub fn log_if_internal(&self, context: &'static str) {
        if let Self::Internal(message) = self {
            tracing::error!(context, error = %message, "Unexpected realtime join failure");
        }
    }
}

impl From<String> for RealtimeJoinError {
    fn from(message: String) -> Self {
        classify_realtime_join_error_message(message)
    }
}

impl From<crate::runtime::RealtimeAdmissionError> for RealtimeJoinError {
    fn from(error: crate::runtime::RealtimeAdmissionError) -> Self {
        match error {
            crate::runtime::RealtimeAdmissionError::Capacity(message) => Self::RateLimited(message),
            crate::runtime::RealtimeAdmissionError::ClusterUnavailable(message) => {
                Self::ServiceUnavailable(message)
            }
            crate::runtime::RealtimeAdmissionError::Internal(message) => Self::Internal(message),
        }
    }
}

impl From<crate::impls::ApiError> for RealtimeJoinError {
    fn from(error: crate::impls::ApiError) -> Self {
        let message = error.message().to_string();
        match error.classify() {
            crate::impls::ErrorKind::InvalidArgument => Self::InvalidInput(message),
            crate::impls::ErrorKind::RateLimited => Self::RateLimited(message),
            crate::impls::ErrorKind::ServiceUnavailable | crate::impls::ErrorKind::Timeout => {
                Self::ServiceUnavailable(message)
            }
            crate::impls::ErrorKind::PermissionDenied
            | crate::impls::ErrorKind::Unauthenticated => Self::PermissionDenied(message),
            _ => Self::Internal(message),
        }
    }
}

impl From<RealtimeJoinError> for crate::impls::ApiError {
    fn from(error: RealtimeJoinError) -> Self {
        match error {
            RealtimeJoinError::InvalidInput(message) => Self::InvalidInput(message),
            RealtimeJoinError::PermissionDenied(message) => Self::Authorization(message),
            RealtimeJoinError::RateLimited(message) => Self::RateLimited(message),
            RealtimeJoinError::ServiceUnavailable(message) => Self::ServiceUnavailable(message),
            RealtimeJoinError::Internal(message) => Self::Internal(message),
        }
    }
}

impl From<RealtimeJoinError> for String {
    fn from(error: RealtimeJoinError) -> Self {
        error.to_string()
    }
}

impl std::fmt::Display for RealtimeJoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for RealtimeJoinError {}

pub(crate) fn classify_realtime_join_error_message(message: String) -> RealtimeJoinError {
    match crate::impls::classify_error(&message) {
        crate::impls::ErrorKind::InvalidArgument => RealtimeJoinError::InvalidInput(message),
        crate::impls::ErrorKind::RateLimited => RealtimeJoinError::RateLimited(message),
        crate::impls::ErrorKind::ServiceUnavailable => {
            RealtimeJoinError::ServiceUnavailable(message)
        }
        crate::impls::ErrorKind::PermissionDenied => RealtimeJoinError::PermissionDenied(message),
        _ => RealtimeJoinError::Internal(message),
    }
}
