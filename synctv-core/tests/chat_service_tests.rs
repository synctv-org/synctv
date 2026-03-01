//! Chat service validation tests
//!
//! Tests chat message and danmaku validation logic that can be tested
//! without full service dependencies.
//!
//! Run with: cargo test --test `chat_service_tests`
#![allow(clippy::unwrap_used)]

/// Maximum chat message characters (mirrors the constant in `ChatService`)
const MAX_CHAT_MESSAGE_CHARS: usize = 500;

/// Maximum danmaku characters (mirrors the service validation)
const MAX_DANMAKU_CHARS: usize = 100;

// ============================================================================
// Chat message validation (extracted from ChatService::send_message logic)
// ============================================================================

/// Validate chat message content (mirrors `ChatService::send_message` validation)
fn validate_chat_content(content: &str) -> Result<(), &'static str> {
    if content.is_empty() {
        return Err("Message content cannot be empty");
    }
    if content.chars().count() > MAX_CHAT_MESSAGE_CHARS {
        return Err("Message content must be at most 500 characters");
    }
    Ok(())
}

/// Validate danmaku content (mirrors `ChatService::send_danmaku` validation)
fn validate_danmaku_content(content: &str) -> Result<(), &'static str> {
    if content.is_empty() {
        return Err("Danmaku content cannot be empty");
    }
    if content.chars().count() > MAX_DANMAKU_CHARS {
        return Err("Danmaku content must be at most 100 characters");
    }
    Ok(())
}

/// Validate danmaku color (mirrors `ChatService::send_danmaku` validation).
///
/// Uses `chars().count()` instead of `.len()` to correctly reject
/// multi-byte UTF-8 strings that happen to have a byte-length of 7.
fn validate_danmaku_color(color: &str) -> Result<(), &'static str> {
    if !color.starts_with('#') || color.chars().count() != 7 {
        return Err("Invalid color format");
    }
    if !color[1..].chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("Invalid color format: must be hex digits");
    }
    Ok(())
}

// ============================================================================
// Chat message tests
// ============================================================================

#[test]
fn test_chat_message_empty_rejected() {
    let result = validate_chat_content("");
    assert!(result.is_err(), "Empty message should be rejected");
    assert!(result.unwrap_err().contains("empty"));
}

#[test]
fn test_chat_message_over_500_chars_rejected() {
    let content: String = "a".repeat(501);
    let result = validate_chat_content(&content);
    assert!(result.is_err(), "Message over 500 chars should be rejected");
}

#[test]
fn test_chat_message_exactly_500_accepted() {
    let content: String = "a".repeat(500);
    let result = validate_chat_content(&content);
    assert!(
        result.is_ok(),
        "Message of exactly 500 chars should be accepted"
    );
}

#[test]
fn test_chat_message_499_chars_accepted() {
    let content: String = "a".repeat(499);
    assert!(validate_chat_content(&content).is_ok());
}

#[test]
fn test_chat_message_single_char_accepted() {
    assert!(validate_chat_content("x").is_ok());
}

#[test]
fn test_chat_message_unicode_counted_by_chars_not_bytes() {
    // Each emoji is 1 char but multiple bytes
    let content: String = "\u{1F600}".repeat(500);
    assert_eq!(content.chars().count(), 500);
    assert!(validate_chat_content(&content).is_ok());

    let too_long: String = "\u{1F600}".repeat(501);
    assert!(validate_chat_content(&too_long).is_err());
}

// ============================================================================
// Danmaku content tests
// ============================================================================

#[test]
fn test_danmaku_over_100_chars_rejected() {
    let content: String = "a".repeat(101);
    let result = validate_danmaku_content(&content);
    assert!(result.is_err(), "Danmaku over 100 chars should be rejected");
}

#[test]
fn test_danmaku_exactly_100_accepted() {
    let content: String = "a".repeat(100);
    assert!(validate_danmaku_content(&content).is_ok());
}

#[test]
fn test_danmaku_empty_rejected() {
    assert!(validate_danmaku_content("").is_err());
}

// ============================================================================
// Danmaku color tests
// ============================================================================

#[test]
fn test_danmaku_color_valid_format() {
    assert!(validate_danmaku_color("#000000").is_ok());
    assert!(validate_danmaku_color("#FFFFFF").is_ok());
    assert!(validate_danmaku_color("#ff00ff").is_ok());
    assert!(validate_danmaku_color("#aAbBcC").is_ok());
    assert!(validate_danmaku_color("#123456").is_ok());
    assert!(validate_danmaku_color("#7890ab").is_ok());
}

#[test]
fn test_danmaku_color_missing_hash_rejected() {
    assert!(validate_danmaku_color("000000").is_err());
    assert!(validate_danmaku_color("FFFFFF").is_err());
    assert!(validate_danmaku_color("ff00ff").is_err());
}

#[test]
fn test_danmaku_color_wrong_length_rejected() {
    assert!(validate_danmaku_color("#FFF").is_err());
    assert!(validate_danmaku_color("#FFFFFFFF").is_err());
    assert!(validate_danmaku_color("#").is_err());
    assert!(validate_danmaku_color("").is_err());
    assert!(validate_danmaku_color("#FF").is_err());
    assert!(validate_danmaku_color("#FFFFF").is_err());
}

#[test]
fn test_danmaku_color_non_hex_chars_rejected() {
    assert!(validate_danmaku_color("#<scrip").is_err());
    assert!(validate_danmaku_color("#ghijkl").is_err());
    assert!(validate_danmaku_color("#ZZZZZZ").is_err());
    assert!(validate_danmaku_color("#12345g").is_err());
    assert!(validate_danmaku_color("#00000!").is_err());
    assert!(validate_danmaku_color("# space").is_err());
}

#[test]
fn test_danmaku_color_xss_payloads_rejected() {
    assert!(validate_danmaku_color("#<>\"'&;").is_err());
    assert!(validate_danmaku_color("#script").is_err());
}

/// Test that multi-byte UTF-8 strings with a byte-length of 7 are correctly rejected.
///
/// Before the fix, `color.len() != 7` used byte-length, which could pass for a
/// multi-byte string that happens to be 7 bytes. With `chars().count() != 7`,
/// we correctly count Unicode characters.
#[test]
fn test_danmaku_color_multibyte_utf8_rejected() {
    // "#" (1 byte) + two 3-byte characters = 7 bytes total, but only 3 chars
    let multibyte = format!("#{}", "\u{00E9}\u{00E9}"); // "#ee" with accented e's
    assert_eq!(multibyte.len(), 5); // 1 + 2 + 2 = 5 bytes
    assert!(
        validate_danmaku_color(&multibyte).is_err(),
        "Multi-byte UTF-8 string should be rejected"
    );

    // A string that is exactly 7 bytes but fewer than 7 chars:
    // "#" (1 byte) + 2 x 3-byte chars = 7 bytes, 3 chars
    let tricky = "#\u{4E16}\u{754C}"; // "#" + 2 CJK chars, each 3 bytes
    assert_eq!(tricky.len(), 7); // 1 + 3 + 3 = 7 bytes
    assert_eq!(tricky.chars().count(), 3); // only 3 chars
    assert!(
        validate_danmaku_color(tricky).is_err(),
        "String with 7 bytes but 3 chars should be rejected by chars().count()"
    );
}

/// Test that the ChatMessage.user_id field is correctly Optional in the model
#[test]
fn test_chat_message_user_id_is_optional() {
    use synctv_core::models::{ChatMessage, RoomId, UserId};

    // ChatMessage::new wraps user_id in Some
    let msg = ChatMessage::new(
        RoomId::from_string("room1".to_string()),
        UserId::from_string("user1".to_string()),
        "hello".to_string(),
    );
    assert_eq!(
        msg.user_id,
        Some(UserId::from_string("user1".to_string())),
        "ChatMessage::new should wrap user_id in Some"
    );

    // Can construct a ChatMessage with None user_id (simulating deleted user)
    let orphaned = ChatMessage {
        id: "test_id_1234".to_string(),
        room_id: RoomId::from_string("room1".to_string()),
        user_id: None,
        content: "orphaned message".to_string(),
        message_type: 1,
        created_at: chrono::Utc::now(),
    };
    assert!(
        orphaned.user_id.is_none(),
        "Orphaned message should have None user_id"
    );
}

/// Test DanmakuPosition::as_str returns correct string representations
#[test]
fn test_danmaku_position_as_str() {
    use synctv_core::models::DanmakuPosition;

    assert_eq!(DanmakuPosition::Top.as_str(), "top");
    assert_eq!(DanmakuPosition::Bottom.as_str(), "bottom");
    assert_eq!(DanmakuPosition::Scroll.as_str(), "scroll");
}

/// Test that content at the boundary (exactly 500 chars) is accepted
/// and content at 501 chars is rejected
#[test]
fn test_chat_message_boundary_500_chars() {
    let at_limit: String = "a".repeat(500);
    assert!(validate_chat_content(&at_limit).is_ok());

    let over_limit: String = "a".repeat(501);
    assert!(validate_chat_content(&over_limit).is_err());
}

/// Test that danmaku content at the boundary (exactly 100 chars) is accepted
/// and content at 101 chars is rejected
#[test]
fn test_danmaku_content_boundary_100_chars() {
    let at_limit: String = "b".repeat(100);
    assert!(validate_danmaku_content(&at_limit).is_ok());

    let over_limit: String = "b".repeat(101);
    assert!(validate_danmaku_content(&over_limit).is_err());
}
