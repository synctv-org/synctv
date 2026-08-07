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

impl From<ReviewStatus> for i32 {
    fn from(value: ReviewStatus) -> Self {
        match value {
            ReviewStatus::Pending => 1,
            ReviewStatus::Approved => 2,
            ReviewStatus::Rejected => 3,
        }
    }
}

impl TryFrom<i32> for ReviewStatus {
    type Error = String;

    fn try_from(value: i32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Pending),
            2 => Ok(Self::Approved),
            3 => Ok(Self::Rejected),
            _ => Err(format!("Unknown review status: {value}")),
        }
    }
}

i16_enum!(ReviewStatus, "Invalid ReviewStatus value", {
    Pending = 1,
    Approved = 2,
    Rejected = 3,
});

#[cfg(test)]
mod tests {
    use super::ReviewStatus;

    #[test]
    fn review_status_i32_conversions_reject_unknown_input() {
        assert_eq!(i32::from(ReviewStatus::Pending), 1);
        assert_eq!(ReviewStatus::try_from(2), Ok(ReviewStatus::Approved));
        assert!(ReviewStatus::try_from(0).is_err());
        assert!(ReviewStatus::try_from(4).is_err());
    }
}
