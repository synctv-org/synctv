use serde::{Deserialize, Serialize};

/// Expected length of all entity IDs (shared base62 IDs)
pub const ID_LENGTH: usize = 12;

/// Generate a 12-character shared base62 ID for entity IDs
pub fn generate_id() -> String {
    synctv_common::snanoid!(ID_LENGTH)
}

/// Validate that an externally-supplied ID string uses the expected base62
/// alphabet and length.
///
/// Returns `Err` with a descriptive message when validation fails.
fn validate_id(id: &str, type_name: &str) -> Result<(), String> {
    if id.len() != ID_LENGTH {
        return Err(format!(
            "invalid {type_name}: expected {ID_LENGTH} characters, got {}",
            id.len()
        ));
    }

    if synctv_common::id::is_valid_with_len(id, ID_LENGTH) {
        Ok(())
    } else {
        Err(format!(
            "invalid {type_name}: expected only ASCII alphanumeric characters"
        ))
    }
}

/// User ID type (CHAR(12) shared base62 ID)
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

    /// Create a `UserId` from an externally-supplied string, validating the
    /// expected base62 format. Use this in API / gRPC handlers that receive
    /// IDs from untrusted input.
    pub fn from_string_validated(id: String) -> Result<Self, String> {
        validate_id(&id, "UserId")?;
        Ok(Self(id))
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
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for UserId {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

/// Room ID type (CHAR(12) shared base62 ID)
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

    /// Create a `RoomId` from an externally-supplied string, validating the
    /// expected base62 format. Use this in API / gRPC handlers that receive
    /// IDs from untrusted input.
    pub fn from_string_validated(id: String) -> Result<Self, String> {
        validate_id(&id, "RoomId")?;
        Ok(Self(id))
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
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for RoomId {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

/// Media ID type (CHAR(12) shared base62 ID)
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

    /// Create a `MediaId` from an externally-supplied string, validating the
    /// expected base62 format. Use this in API / gRPC handlers that receive
    /// IDs from untrusted input.
    pub fn from_string_validated(id: String) -> Result<Self, String> {
        validate_id(&id, "MediaId")?;
        Ok(Self(id))
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
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for MediaId {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let s = <String as sqlx::Decode<sqlx::Postgres>>::decode(value)?;
        Ok(Self(s))
    }
}

/// Playlist ID type (CHAR(12) shared base62 ID)
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

    /// Create a `PlaylistId` from an externally-supplied string, validating the
    /// expected base62 format. Use this in API / gRPC handlers that receive
    /// IDs from untrusted input.
    pub fn from_string_validated(id: String) -> Result<Self, String> {
        validate_id(&id, "PlaylistId")?;
        Ok(Self(id))
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
    fn encode_by_ref(
        &self,
        buf: &mut sqlx::postgres::PgArgumentBuffer,
    ) -> Result<sqlx::encode::IsNull, Box<dyn std::error::Error + Send + Sync>> {
        <String as sqlx::Encode<sqlx::Postgres>>::encode_by_ref(&self.0, buf)
    }
}

impl<'r> sqlx::Decode<'r, sqlx::Postgres> for PlaylistId {
    fn decode(
        value: sqlx::postgres::PgValueRef<'r>,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
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
        assert!(synctv_common::id::is_valid(&id));
        assert!(!id.contains('-'));
        assert!(!id.contains('_'));
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

    #[test]
    fn test_from_string_validated_accepts_valid_base62_id() {
        let valid = "abcdefghijkl".to_string(); // 12 chars
        assert!(UserId::from_string_validated(valid.clone()).is_ok());
        assert!(RoomId::from_string_validated(valid.clone()).is_ok());
        assert!(MediaId::from_string_validated(valid.clone()).is_ok());
        assert!(PlaylistId::from_string_validated(valid).is_ok());
    }

    #[test]
    fn test_from_string_validated_rejects_wrong_length() {
        let too_short = "abc".to_string();
        let too_long = "abcdefghijklmnop".to_string();

        assert!(UserId::from_string_validated(too_short.clone()).is_err());
        assert!(UserId::from_string_validated(too_long.clone()).is_err());
        assert!(RoomId::from_string_validated(too_short.clone()).is_err());
        assert!(MediaId::from_string_validated(too_short.clone()).is_err());
        assert!(PlaylistId::from_string_validated(too_short).is_err());
        assert!(PlaylistId::from_string_validated(too_long).is_err());
    }

    #[test]
    fn test_from_string_validated_rejects_empty() {
        assert!(UserId::from_string_validated(String::new()).is_err());
    }

    #[test]
    fn test_from_string_validated_rejects_non_base62_characters() {
        for invalid in ["abc-defghijk", "abc_defghijk", "abc.defghijk"] {
            assert!(UserId::from_string_validated(invalid.to_string()).is_err());
            assert!(RoomId::from_string_validated(invalid.to_string()).is_err());
            assert!(MediaId::from_string_validated(invalid.to_string()).is_err());
            assert!(PlaylistId::from_string_validated(invalid.to_string()).is_err());
        }
    }
}
