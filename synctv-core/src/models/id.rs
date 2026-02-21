use nanoid::nanoid;
use serde::{Deserialize, Serialize};

/// Generate a 12-character nanoid for entity IDs
pub fn generate_id() -> String {
    nanoid!(12)
}

/// User ID type (CHAR(12) nanoid)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct UserId(pub String);

impl UserId {
    #[must_use] 
    pub fn new() -> Self {
        Self(generate_id())
    }

    #[must_use] 
    pub const fn from_string(id: String) -> Self {
        Self(id)
    }

    #[must_use] 
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for UserId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for UserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for UserId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// Database mapping: UserId <-> CHAR(12) (using bpchar for fixed-length strings)
impl sqlx::Type<sqlx::Postgres> for UserId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("bpchar")
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for UserId {
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for UserId {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

/// Room ID type (CHAR(12) nanoid)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomId(pub String);

impl RoomId {
    #[must_use] 
    pub fn new() -> Self {
        Self(generate_id())
    }

    #[must_use] 
    pub const fn from_string(id: String) -> Self {
        Self(id)
    }

    #[must_use] 
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for RoomId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for RoomId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// Database mapping: RoomId <-> CHAR(12) (using bpchar for fixed-length strings)
impl sqlx::Type<sqlx::Postgres> for RoomId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("bpchar")
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for RoomId {
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for RoomId {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

/// Media ID type (CHAR(12) nanoid)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MediaId(pub String);

impl MediaId {
    #[must_use] 
    pub fn new() -> Self {
        Self(generate_id())
    }

    #[must_use] 
    pub const fn from_string(id: String) -> Self {
        Self(id)
    }

    #[must_use] 
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for MediaId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MediaId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for MediaId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// Database mapping: MediaId <-> CHAR(12) (using bpchar for fixed-length strings)
impl sqlx::Type<sqlx::Postgres> for MediaId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("bpchar")
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for MediaId {
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for MediaId {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

/// Playlist ID type (CHAR(12) nanoid)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PlaylistId(pub String);

impl PlaylistId {
    #[must_use]
    pub fn new() -> Self {
        Self(generate_id())
    }

    #[must_use]
    pub const fn from_string(id: String) -> Self {
        Self(id)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PlaylistId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PlaylistId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for PlaylistId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// Database mapping: PlaylistId <-> CHAR(12) (using bpchar for fixed-length strings)
impl sqlx::Type<sqlx::Postgres> for PlaylistId {
    fn type_info() -> sqlx::postgres::PgTypeInfo {
        sqlx::postgres::PgTypeInfo::with_name("bpchar")
    }
}

impl sqlx::Encode<'_, sqlx::Postgres> for PlaylistId {
    fn encode_by_ref(&self, buf: &mut sqlx::postgres::PgArgumentBuffer) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for PlaylistId {
    fn decode(value: sqlx::postgres::PgValueRef<'r>) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_id() {
        let id = generate_id();
        assert_eq!(id.len(), 12);
    }

    #[test]
    fn test_user_id() {
        let id1 = UserId::new();
        let id2 = UserId::new();
        assert_ne!(id1, id2);
        assert_eq!(id1.as_str().len(), 12);
    }

    #[test]
    fn test_room_id() {
        let id1 = RoomId::new();
        let id2 = RoomId::new();
        assert_ne!(id1, id2);
        assert_eq!(id1.as_str().len(), 12);
    }

    #[test]
    fn test_media_id() {
        let id1 = MediaId::new();
        let id2 = MediaId::new();
        assert_ne!(id1, id2);
        assert_eq!(id1.as_str().len(), 12);
    }
}
