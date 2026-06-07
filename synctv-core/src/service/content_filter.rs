use ammonia::{Builder, UrlRelative};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use thiserror::Error;

static SAFE_HTML_CLEANER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut attributes = HashMap::new();
    attributes.insert("a", HashSet::from(["href", "title"]));

    let mut cleaner = Builder::default();
    cleaner
        .tags(HashSet::from(["a", "b", "strong", "i", "em", "code", "br"]))
        .tag_attributes(attributes)
        .generic_attributes(HashSet::new())
        .url_relative(UrlRelative::Deny);
    cleaner
});

static PLAIN_TEXT_CLEANER: LazyLock<Builder<'static>> = LazyLock::new(|| {
    let mut cleaner = Builder::default();
    cleaner
        .tags(HashSet::new())
        .tag_attributes(HashMap::new())
        .generic_attributes(HashSet::new())
        .url_relative(UrlRelative::Deny);
    cleaner
});

#[derive(Error, Debug, PartialEq, Eq)]
pub enum ContentFilterError {
    #[error("Message exceeds maximum length of {max_length} characters")]
    MessageTooLong { max_length: usize },

    #[error("Message is empty")]
    EmptyMessage,

    #[error("Message contains prohibited content: {reason}")]
    ProhibitedContent { reason: String },
}

#[derive(Clone)]
pub struct ContentFilter {
    pub max_chat_length: usize,
    sensitive_words: Option<HashSet<String>>,
    strip_html: bool,
}

impl ContentFilter {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            max_chat_length: 2000,
            sensitive_words: None,
            strip_html: true,
        }
    }

    #[must_use]
    pub fn new_with_config(
        max_chat_length: usize,
        sensitive_words: Option<Vec<String>>,
        strip_html: bool,
    ) -> Self {
        let sensitive_words = sensitive_words.and_then(|words| {
            let words = words
                .into_iter()
                .map(|word| word.trim().to_lowercase())
                .filter(|word| !word.is_empty())
                .collect::<HashSet<_>>();
            (!words.is_empty()).then_some(words)
        });

        Self {
            max_chat_length,
            sensitive_words,
            strip_html,
        }
    }

    pub fn filter_chat(&self, message: &str) -> Result<String, ContentFilterError> {
        let trimmed = message.trim();
        if trimmed.is_empty() {
            return Err(ContentFilterError::EmptyMessage);
        }

        if trimmed.chars().count() > self.max_chat_length {
            return Err(ContentFilterError::MessageTooLong {
                max_length: self.max_chat_length,
            });
        }

        let sanitized = if self.strip_html {
            Self::strip_all_html(trimmed)
        } else {
            SAFE_HTML_CLEANER.clean(trimmed).to_string()
        };

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

    fn strip_all_html(text: &str) -> String {
        PLAIN_TEXT_CLEANER
            .clean(text)
            .to_string()
            .trim()
            .to_string()
    }

    pub fn validate_username(&self, username: &str) -> Result<String, ContentFilterError> {
        let trimmed = username.trim();

        if trimmed.is_empty() {
            return Err(ContentFilterError::EmptyMessage);
        }

        if trimmed.chars().count() > 50 {
            return Err(ContentFilterError::MessageTooLong { max_length: 50 });
        }

        if !trimmed
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-' || c == ' ')
        {
            return Err(ContentFilterError::ProhibitedContent {
                reason: "Username contains invalid characters".to_string(),
            });
        }

        Ok(trimmed.to_string())
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
    fn filter_chat_accepts_plain_unicode_text() {
        let filter = ContentFilter::new();

        assert_eq!(
            filter.filter_chat("Hello bonjour مرحبا 🌍").unwrap(),
            "Hello bonjour مرحبا 🌍"
        );
        assert_eq!(
            filter.filter_chat("Line 1\nLine 2").unwrap(),
            "Line 1\nLine 2"
        );
    }

    #[test]
    fn filter_chat_rejects_empty_and_overlong_messages() {
        let filter = ContentFilter::new();

        assert_eq!(
            filter.filter_chat("   "),
            Err(ContentFilterError::EmptyMessage)
        );
        assert!(matches!(
            filter.filter_chat(&"a".repeat(2001)),
            Err(ContentFilterError::MessageTooLong { max_length: 2000 })
        ));
        assert!(filter.filter_chat(&"🎉".repeat(2000)).is_ok());
    }

    #[test]
    fn filter_chat_strips_html_and_common_xss_vectors() {
        let filter = ContentFilter::new();

        let result = filter
            .filter_chat("<script>alert(1)</script><b>Hello</b>")
            .unwrap();
        assert!(!result.contains("<script"));
        assert_eq!(result, "Hello");

        for input in [
            "<img src=x onerror=alert(1)>",
            "<a href='javascript:alert(1)'>Click</a>",
            "<iframe src='https://evil.example'></iframe>",
            "<svg onload='alert(1)'><circle></circle></svg>",
        ] {
            let result = filter.filter_chat(input).unwrap();
            assert!(!result.contains("javascript:"));
            assert!(!result.contains("onerror"));
            assert!(!result.contains("onload"));
            assert!(!result.contains("<iframe"));
            assert!(!result.contains("<svg"));
        }
    }

    #[test]
    fn filter_chat_can_allow_small_safe_html_subset() {
        let filter = ContentFilter::new_with_config(2000, None, false);

        let result = filter
            .filter_chat("<b>Bold</b><em>Em</em><script>alert(1)</script>")
            .unwrap();
        assert!(result.contains("<b>Bold</b>"));
        assert!(result.contains("<em>Em</em>"));
        assert!(!result.contains("<script"));

        let result = filter
            .filter_chat("<img src='x'><a href='https://example.com'>Link</a>")
            .unwrap();
        assert!(!result.contains("<img"));
        assert!(result.contains("<a href=\"https://example.com\""));
    }

    #[test]
    fn sensitive_words_are_case_insensitive_and_ignore_empty_config_entries() {
        let filter = ContentFilter::new_with_config(
            1000,
            Some(vec!["badword".to_string(), " ".to_string()]),
            true,
        );

        assert!(matches!(
            filter.filter_chat("This contains BADWORD"),
            Err(ContentFilterError::ProhibitedContent { .. })
        ));
        assert_eq!(filter.filter_chat("clean").unwrap(), "clean");

        let empty_filter = ContentFilter::new_with_config(1000, Some(vec![" ".to_string()]), true);
        assert_eq!(empty_filter.filter_chat("clean").unwrap(), "clean");
    }

    #[test]
    fn validate_username_accepts_public_name_chars() {
        let filter = ContentFilter::new();

        assert_eq!(
            filter.validate_username("  user name-123_测试  ").unwrap(),
            "user name-123_测试"
        );
        assert_eq!(
            filter.validate_username("пользователь").unwrap(),
            "пользователь"
        );
    }

    #[test]
    fn validate_username_rejects_empty_overlong_and_control_or_symbol_chars() {
        let filter = ContentFilter::new();

        assert_eq!(
            filter.validate_username(""),
            Err(ContentFilterError::EmptyMessage)
        );
        assert!(matches!(
            filter.validate_username(&"a".repeat(51)),
            Err(ContentFilterError::MessageTooLong { max_length: 50 })
        ));
        for username in ["user@email.com", "user<script>", "user\nname", "user\tname"] {
            assert!(matches!(
                filter.validate_username(username),
                Err(ContentFilterError::ProhibitedContent { .. })
            ));
        }
    }
}
