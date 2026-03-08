//! Room ID validation helpers
//!
//! This module provides utilities for validating and parsing room IDs
//! that can be used by both HTTP and gRPC endpoints.

use crate::http::validation::{validate_room_id as http_validate_room_id, ValidationError};
use synctv_core::models::RoomId;

/// Validate and parse a room ID string
///
/// This function validates the room ID format according to the rules:
/// - Must not be empty
/// - Must be exactly 12 characters long
/// - Must contain only alphanumeric characters, underscores, and hyphens
///
/// # Arguments
/// * `id` - The room ID string to validate
///
/// # Returns
/// * `Ok(RoomId)` - If the ID is valid
/// * `Err(ValidationError)` - If the ID is invalid
///
/// # Example
/// ```rust
/// use synctv_api::room_id_validation::parse_room_id;
///
/// // Valid room IDs
/// assert!(parse_room_id("room1234_abx").is_ok());
/// assert!(parse_room_id("room_123-xyz").is_ok());
/// assert!(parse_room_id("ROOM-123_abC").is_ok());
///
/// // Invalid room IDs
/// assert!(parse_room_id("room@123").is_err());
/// assert!(parse_room_id("").is_err());
/// ```
pub fn parse_room_id(id: &str) -> Result<RoomId, ValidationError> {
    let validated = http_validate_room_id(id)?;
    Ok(RoomId::from_string(validated))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_room_id_valid_formats() {
        assert!(parse_room_id("room1234_abx").is_ok());
        assert!(parse_room_id("room_123-xyz").is_ok());
        assert!(parse_room_id("Room1234_Abc").is_ok());
        assert!(parse_room_id("ROOM1234-XYZ").is_ok());
        assert!(parse_room_id("123456789012").is_ok());
    }

    #[test]
    fn test_parse_room_id_invalid_formats() {
        // Invalid characters
        assert!(parse_room_id("room@123").is_err());
        assert!(parse_room_id("room#123").is_err());
        assert!(parse_room_id("room$123").is_err());
        assert!(parse_room_id("room%123").is_err());
        assert!(parse_room_id("room&123").is_err());
        assert!(parse_room_id("room*123").is_err());
        assert!(parse_room_id("room 123").is_err()); // Space
        assert!(parse_room_id("room.123").is_err()); // Dot
        assert!(parse_room_id("room/123").is_err()); // Slash
        assert!(parse_room_id("room\\123").is_err()); // Backslash

        // Empty
        assert!(parse_room_id("").is_err());

        // Control characters (should be sanitized)
        assert!(parse_room_id("room\n123").is_err());
        assert!(parse_room_id("room\t123").is_err());

        // Unicode characters (ROOM_ID regex is ASCII-only)
        assert!(parse_room_id("房间123").is_err());
        assert!(parse_room_id("roomééé").is_err());
    }

    #[test]
    fn test_parse_room_id_length_limits() {
        assert!(parse_room_id(&"a".repeat(12)).is_ok());

        let too_short = "a".repeat(11);
        assert!(parse_room_id(&too_short).is_err());

        let too_long = "a".repeat(13);
        assert!(parse_room_id(&too_long).is_err());
    }

    #[test]
    fn test_parse_room_id_returns_valid_roomid() {
        let room_id = parse_room_id("test-room_12").unwrap();
        assert_eq!(room_id.as_str(), "test-room_12");
    }

    #[test]
    fn test_parse_room_id_error_messages() {
        // Empty room ID
        let err = parse_room_id("").unwrap_err();
        assert!(matches!(err, ValidationError::Required(_)));

        // Invalid format
        let err = parse_room_id("room@123").unwrap_err();
        assert!(matches!(err, ValidationError::InvalidFormat { .. }));

        // Too long
        let too_long = "a".repeat(13);
        let err = parse_room_id(&too_long).unwrap_err();
        assert!(matches!(err, ValidationError::TooLong { .. }));
    }
}
