//! Room ID validation helpers
//!
//! This module provides utilities for validating and parsing room IDs
//! that can be used by both HTTP and gRPC endpoints.

use synctv_core::models::RoomId;
use crate::http::validation::{ValidationError, validate_room_id as http_validate_room_id};

/// Validate and parse a room ID string
///
/// This function validates the room ID format according to the rules:
/// - Must not be empty
/// - Must not exceed `ID_MAX` length
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
/// assert!(parse_room_id("room123").is_ok());
/// assert!(parse_room_id("room_123").is_ok());
/// assert!(parse_room_id("room-123").is_ok());
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
mod tests {
    use super::*;

    #[test]
    fn test_parse_room_id_valid_formats() {
        assert!(parse_room_id("room123").is_ok());
        assert!(parse_room_id("room_123").is_ok());
        assert!(parse_room_id("room-123").is_ok());
        assert!(parse_room_id("Room123").is_ok());  // Case sensitive
        assert!(parse_room_id("ROOM123").is_ok());  // All caps
        assert!(parse_room_id("a").is_ok());  // Single char
        assert!(parse_room_id("123").is_ok());  // Numbers only
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
        assert!(parse_room_id("room 123").is_err());  // Space
        assert!(parse_room_id("room.123").is_err());  // Dot
        assert!(parse_room_id("room/123").is_err());  // Slash
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
        // Valid length (1-64 chars)
        assert!(parse_room_id("a").is_ok());
        assert!(parse_room_id(&"a".repeat(64)).is_ok());

        // Too long (>64 chars)
        let too_long = "a".repeat(65);
        assert!(parse_room_id(&too_long).is_err());
    }

    #[test]
    fn test_parse_room_id_returns_valid_roomid() {
        let room_id = parse_room_id("test-room_123").unwrap();
        assert_eq!(room_id.as_str(), "test-room_123");
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
        let too_long = "a".repeat(65);
        let err = parse_room_id(&too_long).unwrap_err();
        assert!(matches!(err, ValidationError::TooLong { .. }));
    }
}
