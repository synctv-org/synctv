use serde::{Deserialize, Serialize};

use super::UserId;

/// Identity attached to a realtime connection.
///
/// User and guest identities carry different data so guests cannot be
/// represented as synthetic users.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum RealtimeActor {
    User { user_id: UserId, public_id: String },
    Guest { guest_id: String },
}

impl RealtimeActor {
    #[must_use]
    pub fn user(user_id: UserId, public_id: impl Into<String>) -> Self {
        Self::User {
            user_id,
            public_id: public_id.into(),
        }
    }

    #[must_use]
    pub fn guest(guest_id: impl Into<String>) -> Self {
        Self::Guest {
            guest_id: guest_id.into(),
        }
    }

    #[must_use]
    pub const fn user_id(&self) -> Option<UserId> {
        match self {
            Self::User { user_id, .. } => Some(*user_id),
            Self::Guest { .. } => None,
        }
    }

    #[must_use]
    pub fn public_id(&self) -> &str {
        match self {
            Self::User { public_id, .. } => public_id,
            Self::Guest { guest_id } => guest_id,
        }
    }

    #[must_use]
    pub const fn is_guest(&self) -> bool {
        matches!(self, Self::Guest { .. })
    }

    #[must_use]
    pub fn connection_key(&self) -> String {
        match self {
            Self::User { user_id, .. } => format!("user:{user_id}"),
            Self::Guest { guest_id } => format!("guest:{guest_id}"),
        }
    }
}

impl std::fmt::Display for RealtimeActor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.public_id())
    }
}

#[cfg(test)]
mod tests {
    use super::{RealtimeActor, UserId};

    #[test]
    fn guest_has_no_user_id() {
        let guest = RealtimeActor::guest("gst_session");

        assert_eq!(guest.user_id(), None);
        assert_eq!(guest.public_id(), "gst_session");
        assert_eq!(guest.connection_key(), "guest:gst_session");
    }

    #[test]
    fn user_keeps_internal_and_public_identity_separate() {
        let user_id = UserId::expect_positive(42);
        let user = RealtimeActor::user(user_id, "usr_public");

        assert_eq!(user.user_id(), Some(user_id));
        assert_eq!(user.public_id(), "usr_public");
        assert_eq!(user.connection_key(), "user:42");
    }
}
