use serde::{Deserialize, Serialize};

/// Business-level reason that a recoverable resource became unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[repr(i16)]
pub enum DeletionSource {
    Account = 1,
    Admin = 2,
    System = 3,
    Room = 4,
    User = 5,
}

impl DeletionSource {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Account => "account",
            Self::Admin => "admin",
            Self::System => "system",
            Self::Room => "room",
            Self::User => "user",
        }
    }
}

impl std::fmt::Display for DeletionSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

i16_enum!(DeletionSource, "Invalid deletion source", {
    Account = 1,
    Admin = 2,
    System = 3,
    Room = 4,
    User = 5,
});

#[cfg(test)]
mod tests {
    use super::DeletionSource;

    #[test]
    fn deletion_sources_round_trip_storage_codes() {
        for source in [
            DeletionSource::Account,
            DeletionSource::Admin,
            DeletionSource::System,
            DeletionSource::Room,
            DeletionSource::User,
        ] {
            let code = i16::from(source);
            assert_eq!(DeletionSource::try_from(code), Ok(source));
        }
        assert!(DeletionSource::try_from(i16::MAX).is_err());
    }
}
