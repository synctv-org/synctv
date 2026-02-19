use ammonia::clean;
use std::collections::HashSet;
use thiserror::Error;

/// Content filtering error
#[derive(Error, Debug)]
pub enum ContentFilterError {
    #[error("Message exceeds maximum length of {max_length} characters")]
    MessageTooLong { max_length: usize },

    #[error("Message is empty")]
    EmptyMessage,

    #[error("Message contains prohibited content: {reason}")]
    ProhibitedContent { reason: String },
}

/// Content filter for sanitizing and validating user-generated content
#[derive(Clone)]
pub struct ContentFilter {
    /// Maximum chat message length
    pub max_chat_length: usize,

    /// Maximum danmaku length
    pub max_danmaku_length: usize,

    /// Sensitive words to filter (optional)
    sensitive_words: Option<HashSet<String>>,

    /// Whether to strip all HTML tags
    strip_html: bool,
}

impl ContentFilter {
    /// Create a new `ContentFilter` with default settings
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_chat_length: 2000,
            max_danmaku_length: 100,
            sensitive_words: None,
            strip_html: true,
        }
    }

    /// Create with custom settings
    #[must_use] 
    pub fn with_config(
        max_chat_length: usize,
        max_danmaku_length: usize,
        sensitive_words: Option<Vec<String>>,
        strip_html: bool,
    ) -> Self {
        let sensitive_words = sensitive_words.map(|words| {
            words.into_iter().map(|w| w.to_lowercase()).collect()
        });

        Self {
            max_chat_length,
            max_danmaku_length,
            sensitive_words,
            strip_html,
        }
    }

    /// Filter and sanitize a chat message
    ///
    /// Returns the sanitized message or an error if invalid
    pub fn filter_chat(&self, message: &str) -> Result<String, ContentFilterError> {
        // Check if empty
        let trimmed = message.trim();
        if trimmed.is_empty() {
            return Err(ContentFilterError::EmptyMessage);
        }

        // Check length (character count, not byte length, for Unicode support)
        if trimmed.chars().count() > self.max_chat_length {
            return Err(ContentFilterError::MessageTooLong {
                max_length: self.max_chat_length,
            });
        }

        // Sanitize HTML/XSS
        let sanitized = if self.strip_html {
            // Strip all HTML tags for maximum safety
            self.strip_all_html(trimmed)
        } else {
            // Allow safe HTML subset (links, bold, italic)
            clean(trimmed)
        };

        // Check for sensitive words
        if let Some(ref words) = self.sensitive_words {
            let lower = sanitized.to_lowercase();
            for word in words {
                if lower.contains(word) {
                    return Err(ContentFilterError::ProhibitedContent {
                        reason: "Contains prohibited word".to_string(),
                    });
                }
            }
        }

        Ok(sanitized)
    }

    /// Filter and sanitize a danmaku message
    ///
    /// Danmaku has stricter rules (shorter, plain text only)
    pub fn filter_danmaku(&self, message: &str) -> Result<String, ContentFilterError> {
        // Check if empty
        let trimmed = message.trim();
        if trimmed.is_empty() {
            return Err(ContentFilterError::EmptyMessage);
        }

        // Check length (character count for Unicode support, danmaku is shorter)
        if trimmed.chars().count() > self.max_danmaku_length {
            return Err(ContentFilterError::MessageTooLong {
                max_length: self.max_danmaku_length,
            });
        }

        // Validate danmaku doesn't contain control characters (check before sanitization)
        if trimmed.chars().any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r') {
            return Err(ContentFilterError::ProhibitedContent {
                reason: "Contains control characters".to_string(),
            });
        }

        // Always strip HTML for danmaku (security + readability)
        let sanitized = self.strip_all_html(trimmed);

        // Check for sensitive words
        if let Some(ref words) = self.sensitive_words {
            let lower = sanitized.to_lowercase();
            for word in words {
                if lower.contains(word) {
                    return Err(ContentFilterError::ProhibitedContent {
                        reason: "Contains prohibited word".to_string(),
                    });
                }
            }
        }

        Ok(sanitized)
    }

    /// Strip all HTML tags from text
    ///
    /// This is more aggressive than ammonia's cleaning - removes ALL HTML
    fn strip_all_html(&self, text: &str) -> String {
        // Use ammonia to decode entities first, then strip tags
        let cleaned = clean(text);

        // Simple state machine to strip HTML tags
        let mut result = String::with_capacity(cleaned.len());
        let mut in_tag = false;

        for ch in cleaned.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(ch),
                _ => {}
            }
        }

        result.trim().to_string()
    }

    /// Validate username
    pub fn validate_username(&self, username: &str) -> Result<String, ContentFilterError> {
        let trimmed = username.trim();

        if trimmed.is_empty() {
            return Err(ContentFilterError::EmptyMessage);
        }

        if trimmed.chars().count() > 50 {
            return Err(ContentFilterError::MessageTooLong { max_length: 50 });
        }

        // Check for special characters first (allow alphanumeric, underscore, dash, whitespace only)
        if !trimmed
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c.is_whitespace())
        {
            return Err(ContentFilterError::ProhibitedContent {
                reason: "Username contains invalid characters".to_string(),
            });
        }

        // Strip HTML (just in case, though validation above should catch it)
        let sanitized = self.strip_all_html(trimmed);

        Ok(sanitized)
    }
}

impl Default for ContentFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_chat_normal() {
        let filter = ContentFilter::new();
        let result = filter.filter_chat("Hello, world!").unwrap();
        assert_eq!(result, "Hello, world!");
    }

    #[test]
    fn test_filter_chat_empty() {
        let filter = ContentFilter::new();
        let result = filter.filter_chat("   ");
        assert!(matches!(result, Err(ContentFilterError::EmptyMessage)));
    }

    #[test]
    fn test_filter_chat_too_long() {
        let filter = ContentFilter::new();
        let long_message = "a".repeat(2001);
        let result = filter.filter_chat(&long_message);
        assert!(matches!(
            result,
            Err(ContentFilterError::MessageTooLong { .. })
        ));
    }

    #[test]
    fn test_filter_chat_xss() {
        let filter = ContentFilter::new();

        // Script tag should be stripped
        let result = filter
            .filter_chat("<script>alert('xss')</script>Hello")
            .unwrap();
        assert!(!result.contains("<script>"));
        assert!(result.contains("Hello"));

        // Image with onerror
        let result = filter
            .filter_chat("<img src=x onerror=alert(1)>")
            .unwrap();
        assert!(!result.contains("onerror"));
    }

    #[test]
    fn test_filter_chat_html_stripping() {
        let filter = ContentFilter::new();

        let result = filter.filter_chat("<b>Bold</b> text").unwrap();
        assert_eq!(result, "Bold text");

        let result = filter.filter_chat("<a href='evil.com'>Link</a>").unwrap();
        assert_eq!(result, "Link");
    }

    #[test]
    fn test_filter_danmaku_normal() {
        let filter = ContentFilter::new();
        let result = filter.filter_danmaku("666").unwrap();
        assert_eq!(result, "666");
    }

    #[test]
    fn test_filter_danmaku_too_long() {
        let filter = ContentFilter::new();
        let long_message = "a".repeat(101);
        let result = filter.filter_danmaku(&long_message);
        assert!(matches!(
            result,
            Err(ContentFilterError::MessageTooLong { max_length: 100 })
        ));
    }

    #[test]
    fn test_filter_danmaku_html() {
        let filter = ContentFilter::new();
        let result = filter.filter_danmaku("<script>alert(1)</script>Danmaku").unwrap();
        assert!(!result.contains("<script>"));
        assert!(result.contains("Danmaku"));
    }

    #[test]
    fn test_sensitive_words() {
        let filter = ContentFilter::with_config(
            1000,
            100,
            Some(vec!["badword".to_string(), "spam".to_string()]),
            true,
        );

        // Should be blocked
        let result = filter.filter_chat("This contains badword!");
        assert!(matches!(
            result,
            Err(ContentFilterError::ProhibitedContent { .. })
        ));

        // Case insensitive
        let result = filter.filter_chat("This contains BADWORD!");
        assert!(matches!(
            result,
            Err(ContentFilterError::ProhibitedContent { .. })
        ));

        // Should pass
        let result = filter.filter_chat("This is clean").unwrap();
        assert_eq!(result, "This is clean");
    }

    #[test]
    fn test_validate_username() {
        let filter = ContentFilter::new();

        // Valid usernames
        assert!(filter.validate_username("john_doe").is_ok());
        assert!(filter.validate_username("user-123").is_ok());
        assert!(filter.validate_username("пользователь").is_ok());

        // Invalid: empty
        assert!(filter.validate_username("").is_err());

        // Invalid: too long
        let long_name = "a".repeat(51);
        assert!(filter.validate_username(&long_name).is_err());

        // Invalid: special characters
        assert!(filter.validate_username("user@email.com").is_err());
        assert!(filter.validate_username("user<script>").is_err());
    }

    #[test]
    fn test_unicode_support() {
        let filter = ContentFilter::new();

        // Should support Unicode (Cyrillic)
        let result = filter.filter_chat("Привет мир 🌍").unwrap();
        assert_eq!(result, "Привет мир 🌍");

        // Should support Unicode (Japanese)
        let result = filter.filter_danmaku("ダンマクテスト").unwrap();
        assert_eq!(result, "ダンマクテスト");

        // Should support Unicode (Arabic)
        let result = filter.filter_chat("مرحبا بالعالم").unwrap();
        assert_eq!(result, "مرحبا بالعالم");

        // Should support Unicode (Emoji)
        let result = filter.filter_chat("Hello 👋 World 🌍").unwrap();
        assert_eq!(result, "Hello 👋 World 🌍");
    }

    #[test]
    fn test_html_entity_decoding() {
        let filter = ContentFilter::new();

        // HTML entities in text should be preserved or decoded safely
        let result = filter.filter_chat("&lt;script&gt;Hello&lt;/script&gt;").unwrap();
        // After stripping HTML, we should have safe text
        assert!(!result.contains("<script>"));
        // The text "Hello" should still be present
        assert!(result.contains("Hello") || result.contains("script")); // Either decoded or stripped
    }

    #[test]
    fn test_control_characters_in_danmaku() {
        let filter = ContentFilter::new();

        // Control characters should be rejected in danmaku
        let result = filter.filter_danmaku("Hello\x00World");
        assert!(matches!(
            result,
            Err(ContentFilterError::ProhibitedContent { .. })
        ));

        // Newlines and tabs are allowed
        let result = filter.filter_danmaku("Line1\nLine2");
        assert!(result.is_ok());
    }
}
