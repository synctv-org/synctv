use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Workflow status for human/manual review records.
///
/// Review states are stored only on request tables such as
/// `user_registration_requests`, `room_creation_requests`, and
/// `room_join_requests`. They are intentionally separate from entity lifecycle
/// and moderation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
#[repr(i16)]
pub enum ReviewStatus {
    #[default]
    Pending = 1,
    Approved = 2,
    Rejected = 3,
}

impl ReviewStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
        }
    }
}

impl FromStr for ReviewStatus {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "pending" => Ok(Self::Pending),
            "approved" => Ok(Self::Approved),
            "rejected" => Ok(Self::Rejected),
            other => Err(format!("Unknown review status: {other}")),
        }
    }
}

impl std::fmt::Display for ReviewStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

sqlx_i16_enum!(ReviewStatus, "Invalid ReviewStatus value", {
    Pending = 1,
    Approved = 2,
    Rejected = 3,
});
