//! Path traversal validation.
//!
//! Provides [`validate_path_for_traversal`] to detect directory-traversal
//! attacks in user-supplied paths.  Catches literal `..`, URL-encoded dots,
//! double-encoded dots, backslash variants, null bytes, and mixed-dot
//! sequences.
#![allow(clippy::missing_errors_doc)]

use std::fmt;

/// Error returned when a path fails traversal validation.
#[derive(Debug, Clone)]
pub struct PathTraversalError {
    pub reason: String,
}

impl fmt::Display for PathTraversalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "path traversal rejected: {}", self.reason)
    }
}

impl std::error::Error for PathTraversalError {}

/// Validate a path for directory-traversal attacks.
///
/// Rejects paths containing:
/// - Literal `..` (covers backslash variants like `..\\` and `..` followed by
///   an encoded separator, since both contain a literal `..`)
/// - Null bytes (literal or `%00`)
/// - Mixed dot sequences (`./.`)
/// - URL-encoded dot (`%2e` / `%2E`)
/// - Double-encoded dot (`%252e` / `%252E`)
///
/// # Examples
///
/// ```
/// use synctv_common::validation::validate_path_for_traversal;
///
/// assert!(validate_path_for_traversal("media/movies").is_ok());
/// assert!(validate_path_for_traversal("/absolute/path").is_ok());
/// assert!(validate_path_for_traversal("../../../etc/passwd").is_err());
/// assert!(validate_path_for_traversal("%2e%2e/secret").is_err());
/// ```
pub fn validate_path_for_traversal(path: &str) -> Result<(), PathTraversalError> {
    // Check 1: Literal ..
    if path.contains("..") {
        return Err(PathTraversalError {
            reason: "must not contain '..' for path traversal".to_string(),
        });
    }

    // Check 2: Null bytes (literal or URL-encoded)
    if path.contains('\0') || path.contains("%00") {
        return Err(PathTraversalError {
            reason: "must not contain null bytes".to_string(),
        });
    }

    // Backslash traversal (`..\`, `\..`) and `..` followed by an encoded
    // separator are intentionally not checked here: any path containing a
    // literal `..` is already rejected by Check 1 above.

    // Check 3: Mixed traversal (e.g., "./../")
    if path.contains("./.") {
        return Err(PathTraversalError {
            reason: "must not contain mixed dot sequences".to_string(),
        });
    }

    // Check 4: URL-encoded variants and complex attacks
    let bytes = path.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &path[i + 1..i + 3];
            if let Ok(byte_val) = u8::from_str_radix(hex, 16) {
                // Reject URL-encoded dot (0x2E)
                if byte_val == 0x2E {
                    return Err(PathTraversalError {
                        reason: "must not contain URL-encoded dot character".to_string(),
                    });
                }

                // Reject multi-layer encoded dots:
                // %252e  -> %2e  -> .  (double-encoded)
                // %25252e -> %252e -> %2e -> .  (triple-encoded)
                if byte_val == 0x25 && i + 4 < bytes.len() {
                    let inner_hex = &path[i + 3..i + 5];
                    if let Ok(inner_val) = u8::from_str_radix(inner_hex, 16) {
                        if inner_val == 0x2E {
                            return Err(PathTraversalError {
                                reason: "must not contain double-encoded dot character".to_string(),
                            });
                        }
                        // Triple encoding: %25 decodes to %, check next layer
                        if inner_val == 0x25 && i + 6 < bytes.len() {
                            let nested_hex = &path[i + 5..i + 7];
                            if let Ok(nested_val) = u8::from_str_radix(nested_hex, 16) {
                                if nested_val == 0x2E {
                                    return Err(PathTraversalError {
                                        reason: "must not contain triple-encoded dot character"
                                            .to_string(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        i += 1;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rejects_literal_double_dot() {
        assert!(validate_path_for_traversal("../../../etc/passwd").is_err());
        assert!(validate_path_for_traversal("../secret").is_err());
        assert!(validate_path_for_traversal("test/../etc").is_err());
        assert!(validate_path_for_traversal("/safe/../../etc").is_err());
    }

    #[test]
    fn test_rejects_url_encoded_dot() {
        assert!(validate_path_for_traversal("%2e%2e/etc/passwd").is_err());
        assert!(validate_path_for_traversal("%2E%2E/secret").is_err());
        assert!(validate_path_for_traversal("test/%2e%2e/config").is_err());
        assert!(validate_path_for_traversal("/%2e%2e/../etc").is_err());
    }

    #[test]
    fn test_rejects_mixed_encoding() {
        assert!(validate_path_for_traversal("..%2fetc/passwd").is_err());
        assert!(validate_path_for_traversal("..%2Fetc/passwd").is_err());
        assert!(validate_path_for_traversal("%2e%2e/secret").is_err());
        assert!(validate_path_for_traversal("test/..%5cwindows").is_err());
    }

    #[test]
    fn test_rejects_backslash_traversal() {
        assert!(validate_path_for_traversal("..\\..\\windows").is_err());
        assert!(validate_path_for_traversal("test\\..\\config").is_err());
        assert!(validate_path_for_traversal("..\\secret").is_err());
        assert!(validate_path_for_traversal("\\..\\windows").is_err());
    }

    #[test]
    fn test_rejects_mixed_dot_sequences() {
        assert!(validate_path_for_traversal("./../etc").is_err());
        assert!(validate_path_for_traversal(".././secret").is_err());
        assert!(validate_path_for_traversal("././../config").is_err());
        assert!(validate_path_for_traversal("./.././etc").is_err());
    }

    #[test]
    fn test_rejects_null_bytes() {
        assert!(validate_path_for_traversal("test\0../etc").is_err());
        assert!(validate_path_for_traversal("/etc/\0passwd").is_err());
        assert!(validate_path_for_traversal("test\0file").is_err());
        // URL-encoded null byte
        assert!(validate_path_for_traversal("/movies/video%00.mp4").is_err());
    }

    #[test]
    fn test_allows_valid_paths() {
        assert!(validate_path_for_traversal("media/movies").is_ok());
        assert!(validate_path_for_traversal("/absolute/path").is_ok());
        assert!(validate_path_for_traversal("folder with spaces/file.txt").is_ok());
        assert!(validate_path_for_traversal("file-with-dashes.txt").is_ok());
        assert!(validate_path_for_traversal("file_with_underscores.txt").is_ok());
        assert!(validate_path_for_traversal("single.dot").is_ok());
        assert!(validate_path_for_traversal("file.tar.gz").is_ok());
        assert!(validate_path_for_traversal("/path/with.dots/in/middle").is_ok());
        assert!(validate_path_for_traversal("unicode-file/résumé.txt").is_ok());
    }

    #[test]
    fn test_edge_cases() {
        assert!(validate_path_for_traversal("").is_ok());
        assert!(validate_path_for_traversal("/").is_ok());
        assert!(validate_path_for_traversal("path//to//file").is_ok());
        assert!(validate_path_for_traversal("path/to/file/").is_ok());
        assert!(validate_path_for_traversal("/leading/slash").is_ok());
    }

    #[test]
    fn test_double_url_encoding() {
        assert!(validate_path_for_traversal("%252e%252e/secret").is_err());
        assert!(validate_path_for_traversal("%252E%252E/secret").is_err());
        assert!(validate_path_for_traversal("%252e%252e").is_err());
    }

    #[test]
    fn test_triple_url_encoding() {
        // %25252e -> %252e -> %2e -> .
        assert!(validate_path_for_traversal("%25252e%25252e/secret").is_err());
        assert!(validate_path_for_traversal("%25252E%25252E/secret").is_err());
    }

    #[test]
    fn test_mixed_case_encoding() {
        assert!(validate_path_for_traversal("%2e%2E/secret").is_err());
        assert!(validate_path_for_traversal("%2E%2e/secret").is_err());
        assert!(validate_path_for_traversal("%2E%2E/secret").is_err());
    }

    #[test]
    fn test_partial_encoding() {
        assert!(validate_path_for_traversal(".%2e/secret").is_err());
        assert!(validate_path_for_traversal("%2e./secret").is_err());
        assert!(validate_path_for_traversal(".%2E/secret").is_err());
    }

    #[test]
    fn test_any_url_encoded_dot() {
        assert!(validate_path_for_traversal("file%2eext").is_err());
        assert!(validate_path_for_traversal("%2ext").is_err());
        assert!(validate_path_for_traversal("t%2ext").is_err());
    }

    #[test]
    fn test_error_display() {
        let err = validate_path_for_traversal("../etc")
            .expect_err("parent-directory traversal must be rejected");
        assert!(err.to_string().contains("path traversal rejected"));
    }
}
