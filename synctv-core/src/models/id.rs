use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use std::sync::atomic::{AtomicI64, Ordering};

pub const INVITE_CODE_LENGTH: usize = 24;

static EPHEMERAL_ID_COUNTER: AtomicI64 = AtomicI64::new(1_000_000_000);
static USER_ID_COUNTER: AtomicI64 = AtomicI64::new(1);
static ROOM_ID_COUNTER: AtomicI64 = AtomicI64::new(1);
static MEDIA_ID_COUNTER: AtomicI64 = AtomicI64::new(1);
static PLAYLIST_ID_COUNTER: AtomicI64 = AtomicI64::new(1);
static REVIEW_REQUEST_ID_COUNTER: AtomicI64 = AtomicI64::new(1);
static BAN_RECORD_ID_COUNTER: AtomicI64 = AtomicI64::new(1);

/// Generate a process-local positive numeric ID for tests and in-memory values.
///
/// Persistent database rows must use database identity columns instead.
pub fn generate_id() -> i64 {
    EPHEMERAL_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

pub fn generate_invite_code() -> String {
    synctv_common::snanoid!(INVITE_CODE_LENGTH)
}

fn validate_id(id: i64, type_name: &str) -> Result<(), String> {
    if id > 0 {
        Ok(())
    } else {
        Err(format!("invalid {type_name}: expected a positive integer"))
    }
}

pub trait TypedId:
    Copy
    + TryFrom<i64, Error = String>
    + Into<i64>
    + fmt::Display
    + FromStr<Err = String>
    + Send
    + Sync
    + 'static
{
    const TYPE_NAME: &'static str;

    fn get(self) -> i64;
}

macro_rules! numeric_id_type {
    ($name:ident, $label:literal, $counter:ident) => {
        #[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(i64);

        impl $name {
            pub const MAX: Self = Self(i64::MAX);

            #[must_use]
            pub fn new() -> Self {
                Self($counter.fetch_add(1, Ordering::Relaxed))
            }

            #[must_use]
            pub fn get(self) -> i64 {
                self.0
            }

            #[must_use]
            pub fn as_i64(&self) -> i64 {
                self.0
            }

            #[track_caller]
            #[must_use]
            pub fn expect_positive(id: i64) -> Self {
                Self::try_from(id).expect("typed id must be positive")
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl TryFrom<u64> for $name {
            type Error = String;

            fn try_from(id: u64) -> Result<Self, Self::Error> {
                let id =
                    i64::try_from(id).map_err(|_| format!("invalid {}: exceeds i64", $label))?;
                validate_id(id, $label)?;
                Ok(Self(id))
            }
        }

        impl TryFrom<i64> for $name {
            type Error = String;

            fn try_from(id: i64) -> Result<Self, Self::Error> {
                validate_id(id, $label)?;
                Ok(Self(id))
            }
        }

        impl From<$name> for i64 {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        impl TypedId for $name {
            const TYPE_NAME: &'static str = $label;

            fn get(self) -> i64 {
                self.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = String;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let id = value
                    .trim()
                    .parse::<i64>()
                    .map_err(|error| format!("invalid {}: {error}", $label))?;
                validate_id(id, $label)?;
                Ok(Self(id))
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                let id = i64::deserialize(deserializer)?;
                validate_id(id, $label).map_err(serde::de::Error::custom)?;
                Ok(Self(id))
            }
        }

        impl sqlx::Type<sqlx::Postgres> for $name {
            fn type_info() -> sqlx::postgres::PgTypeInfo {
                <i64 as sqlx::Type<sqlx::Postgres>>::type_info()
            }
        }

        impl sqlx::Encode<'_, sqlx::Postgres> for $name {
            fn encode_by_ref(
                &self,
                buf: &mut sqlx::postgres::PgArgumentBuffer,
            ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
                <i64 as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
            }
        }

        impl<'r> sqlx::Decode<'r, sqlx::Postgres> for $name {
            fn decode(
                value: sqlx::postgres::PgValueRef<'r>,
            ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
                let id = <i64 as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
                validate_id(id, $label)?;
                Ok(Self(id))
            }
        }
    };
}

numeric_id_type!(UserId, "UserId", USER_ID_COUNTER);
numeric_id_type!(RoomId, "RoomId", ROOM_ID_COUNTER);
numeric_id_type!(MediaId, "MediaId", MEDIA_ID_COUNTER);
numeric_id_type!(PlaylistId, "PlaylistId", PLAYLIST_ID_COUNTER);
numeric_id_type!(
    ReviewRequestId,
    "ReviewRequestId",
    REVIEW_REQUEST_ID_COUNTER
);
numeric_id_type!(BanRecordId, "BanRecordId", BAN_RECORD_ID_COUNTER);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_are_positive_and_unique() {
        let id1 = UserId::new();
        let id2 = UserId::new();
        assert!(id1.as_i64() > 0);
        assert_ne!(id1, id2);
    }

    #[test]
    fn parsed_ids_accept_positive_integers() {
        assert_eq!("1".parse::<UserId>().unwrap().as_i64(), 1);
        assert_eq!("2".parse::<RoomId>().unwrap().as_i64(), 2);
        assert_eq!("3".parse::<MediaId>().unwrap().as_i64(), 3);
        assert_eq!("4".parse::<PlaylistId>().unwrap().as_i64(), 4);
        assert_eq!("5".parse::<ReviewRequestId>().unwrap().as_i64(), 5);
        assert_eq!("6".parse::<BanRecordId>().unwrap().as_i64(), 6);
    }

    #[test]
    fn parsed_ids_reject_non_positive_values() {
        assert!("0".parse::<UserId>().is_err());
        assert!("-1".parse::<RoomId>().is_err());
    }

    #[test]
    fn try_from_i64_rejects_non_positive_values() {
        assert_eq!(UserId::try_from(1_i64).unwrap().as_i64(), 1);
        assert!(UserId::try_from(0_i64).is_err());
        assert!(UserId::try_from(-1_i64).is_err());
    }
}
