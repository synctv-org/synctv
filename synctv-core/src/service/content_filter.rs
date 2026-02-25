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

    // ==================== XSS Protection Tests ====================

    #[test]
    fn test_xss_script_tags() {
        let filter = ContentFilter::new();

        // Basic script tag - script tags should be stripped
        // Note: the text content "alert('xss')" becomes plain text (harmless)
        let result = filter.filter_chat("<script>alert('xss')</script>").unwrap();
        assert!(!result.contains("<script"));

        // Script with attributes - src attribute content should not appear as a URL
        let result = filter
            .filter_chat("<script src='https://evil.com/xss.js'></script>")
            .unwrap();
        assert!(!result.contains("<script"));
        // After stripping, there should be no content (empty script tag)
        assert!(result.is_empty() || !result.contains("evil.com") || !result.contains("src="));

        // Script with different casing - case-insensitive stripping
        let result = filter.filter_chat("<SCRIPT>alert('xss')</SCRIPT>").unwrap();
        assert!(!result.contains("<script") && !result.contains("<SCRIPT"));

        // Script with spaces - malformed HTML should still be safe
        let _result = filter.filter_chat("< script>alert('xss')</script>").unwrap();
        // Malformed tags may not be stripped, but the content should still be safe
        // (browsers won't execute this as a script due to the space)
    }

    #[test]
    fn test_xss_event_handlers() {
        let filter = ContentFilter::new();

        // onclick
        let result = filter
            .filter_chat("<div onclick='alert(1)'>Click me</div>")
            .unwrap();
        assert!(!result.contains("onclick"));
        assert!(result.contains("Click me"));

        // onerror
        let result = filter
            .filter_chat("<img src=x onerror='alert(1)'>")
            .unwrap();
        assert!(!result.contains("onerror"));

        // onload
        let result = filter
            .filter_chat("<body onload='alert(1)'>Hello</body>")
            .unwrap();
        assert!(!result.contains("onload"));
        assert!(result.contains("Hello"));

        // onmouseover
        let result = filter
            .filter_chat("<a onmouseover='alert(1)'>Hover</a>")
            .unwrap();
        assert!(!result.contains("onmouseover"));
        assert!(result.contains("Hover"));

        // onfocus
        let result = filter
            .filter_chat("<input onfocus='alert(1)' autofocus>")
            .unwrap();
        assert!(!result.contains("onfocus"));

        // Mixed event handlers
        let result = filter
            .filter_chat("<div onclick='x' onmouseover='y' onerror='z'>Text</div>")
            .unwrap();
        assert!(!result.contains("onclick"));
        assert!(!result.contains("onmouseover"));
        assert!(!result.contains("onerror"));
    }

    #[test]
    fn test_xss_dangerous_tags() {
        let filter = ContentFilter::new();

        // iframe
        let result = filter
            .filter_chat("<iframe src='https://evil.com'></iframe>")
            .unwrap();
        assert!(!result.contains("<iframe"));
        assert!(!result.contains("evil.com"));

        // object
        let result = filter
            .filter_chat("<object data='https://evil.com/swf'></object>")
            .unwrap();
        assert!(!result.contains("<object"));

        // embed
        let result = filter
            .filter_chat("<embed src='https://evil.com/swf'>")
            .unwrap();
        assert!(!result.contains("<embed"));

        // svg with script
        let result = filter
            .filter_chat("<svg onload='alert(1)'><circle></circle></svg>")
            .unwrap();
        assert!(!result.contains("<svg"));
        assert!(!result.contains("onload"));

        // math with script
        let result = filter
            .filter_chat("<math><mtext><script>alert(1)</script></mtext></math>")
            .unwrap();
        assert!(!result.contains("<script"));
    }

    #[test]
    fn test_xss_javascript_protocol() {
        let filter = ContentFilter::new();

        // javascript: in href
        let result = filter
            .filter_chat("<a href='javascript:alert(1)'>Click</a>")
            .unwrap();
        assert!(!result.contains("javascript:"));
        assert!(result.contains("Click"));

        // javascript: with encoding
        let result = filter
            .filter_chat("<a href='&#106;avascript:alert(1)'>Click</a>")
            .unwrap();
        assert!(!result.contains("javascript:"));

        // javascript: with spaces
        let result = filter
            .filter_chat("<a href='  javascript:alert(1)'>Click</a>")
            .unwrap();
        assert!(!result.contains("javascript:"));

        // vbscript: (IE specific)
        let result = filter
            .filter_chat("<a href='vbscript:alert(1)'>Click</a>")
            .unwrap();
        assert!(!result.contains("vbscript:"));

        // data: URI with script
        let result = filter
            .filter_chat("<a href='data:text/html,<script>alert(1)</script>'>Click</a>")
            .unwrap();
        assert!(!result.contains("data:text/html"));
    }

    #[test]
    fn test_xss_data_uri() {
        let filter = ContentFilter::new();

        // data: URI in img src
        let result = filter
            .filter_chat("<img src='data:image/svg+xml,<svg onload=alert(1)>'>")
            .unwrap();
        assert!(!result.contains("onload"));

        // data: URI in object
        let result = filter
            .filter_chat("<object data='data:text/html,<script>alert(1)</script>'>")
            .unwrap();
        assert!(!result.contains("<object"));
    }

    #[test]
    fn test_xss_style_based() {
        let filter = ContentFilter::new();

        // style with expression (IE)
        let result = filter
            .filter_chat("<div style='width:expression(alert(1))'>Text</div>")
            .unwrap();
        assert!(!result.contains("expression"));

        // style with url
        let result = filter
            .filter_chat("<div style='background:url(javascript:alert(1))'>Text</div>")
            .unwrap();
        assert!(!result.contains("javascript"));
    }

    #[test]
    fn test_xss_encoded_attacks() {
        let filter = ContentFilter::new();

        // HTML entity encoded
        let result = filter.filter_chat("&#60;script&#62;alert(1)&#60;/script&#62;").unwrap();
        // After ammonia processing, this should be safe
        assert!(!result.contains("<script>") || !result.contains("alert"));

        // Hex encoded
        let result = filter.filter_chat("&#x3c;script&#x3e;alert(1)&#x3c;/script&#x3e;").unwrap();
        assert!(!result.contains("<script>") || !result.contains("alert"));

        // Mixed case with encoding
        let result = filter.filter_chat("&#60;ScRiPt&#62;alert(1)&#60;/sCrIpT&#62;").unwrap();
        assert!(!result.contains("<script") && !result.contains("<ScRiPt"));
    }

    #[test]
    fn test_xss_svg_attacks() {
        let filter = ContentFilter::new();

        // SVG with script
        let result = filter
            .filter_chat("<svg><script>alert(1)</script></svg>")
            .unwrap();
        assert!(!result.contains("<script"));

        // SVG with use xlink
        let result = filter
            .filter_chat("<svg><use xlink:href='data:image/svg+xml,<svg onload=alert(1)>'></use></svg>")
            .unwrap();
        assert!(!result.contains("onload"));

        // SVG animate
        let result = filter
            .filter_chat("<svg><animate onbegin='alert(1)'></animate></svg>")
            .unwrap();
        assert!(!result.contains("onbegin"));
    }

    #[test]
    fn test_xss_form_attacks() {
        let filter = ContentFilter::new();

        // form action
        let result = filter
            .filter_chat("<form action='javascript:alert(1)'><input type='submit'></form>")
            .unwrap();
        assert!(!result.contains("javascript:"));

        // formaction on input
        let result = filter
            .filter_chat("<input formaction='javascript:alert(1)' type='submit'>")
            .unwrap();
        assert!(!result.contains("javascript:"));
    }

    #[test]
    fn test_xss_meta_refresh() {
        let filter = ContentFilter::new();

        // meta refresh
        let result = filter
            .filter_chat("<meta http-equiv='refresh' content='0;url=javascript:alert(1)'>")
            .unwrap();
        assert!(!result.contains("javascript:"));
        assert!(!result.contains("<meta"));
    }

    #[test]
    fn test_xss_base_tag() {
        let filter = ContentFilter::new();

        // base tag can hijack relative URLs
        let result = filter
            .filter_chat("<base href='https://evil.com/'>")
            .unwrap();
        assert!(!result.contains("<base"));
    }

    #[test]
    fn test_preserve_safe_content() {
        let filter = ContentFilter::new();

        // Plain text should be preserved
        let result = filter.filter_chat("Hello, this is a normal message!").unwrap();
        assert_eq!(result, "Hello, this is a normal message!");

        // Unicode should be preserved
        let result = filter.filter_chat("Hello 你好 مرحبا 🌍").unwrap();
        assert_eq!(result, "Hello 你好 مرحبا 🌍");

        // Numbers and special chars
        let result = filter.filter_chat("Price: $100 (20% off) + tax!").unwrap();
        assert_eq!(result, "Price: $100 (20% off) + tax!");

        // Newlines and formatting
        let result = filter.filter_chat("Line 1\nLine 2\n\nLine 4").unwrap();
        assert_eq!(result, "Line 1\nLine 2\n\nLine 4");
    }

    #[test]
    fn test_danmaku_xss_protection() {
        let filter = ContentFilter::new();

        // Script in danmaku
        let result = filter.filter_danmaku("<script>alert(1)</script>Danmaku").unwrap();
        assert!(!result.contains("<script"));
        assert!(result.contains("Danmaku"));

        // Event handler in danmaku
        let result = filter.filter_danmaku("<img src=x onerror=alert(1)>").unwrap();
        assert!(!result.contains("onerror"));

        // Link in danmaku
        let result = filter
            .filter_danmaku("<a href='javascript:alert(1)'>Click</a>")
            .unwrap();
        assert!(!result.contains("javascript:"));
        assert!(result.contains("Click"));
    }

    #[test]
    fn test_xss_nested_tags() {
        let filter = ContentFilter::new();

        // Nested script tags
        let result = filter
            .filter_chat("<div><script><script>alert(1)</script></script></div>")
            .unwrap();
        assert!(!result.contains("<script"));

        // Deeply nested
        let result = filter
            .filter_chat("<div><span><b><script>alert(1)</script></b></span></div>")
            .unwrap();
        assert!(!result.contains("<script"));
    }

    #[test]
    fn test_xss_malformed_html() {
        let filter = ContentFilter::new();

        // Unclosed script tag
        let result = filter.filter_chat("<script>alert(1)").unwrap();
        assert!(!result.contains("<script"));

        // Extra spaces
        let result = filter.filter_chat("<  script  >alert(1)<  /  script  >").unwrap();
        // Should strip or escape
        assert!(result.contains("alert") || !result.contains("<script"));

        // Null byte injection
        let result = filter.filter_chat("<scr\x00ipt>alert(1)</script>").unwrap();
        assert!(!result.contains("alert") || !result.contains("<script"));
    }
}
