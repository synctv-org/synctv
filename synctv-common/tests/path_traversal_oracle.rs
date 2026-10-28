//! Equivalence oracle for `validate_path_for_traversal`.
//!
//! Contains a verbatim snapshot of the pre-optimization implementation
//! (frozen at the baseline commit) and compares it against the live
//! implementation. The original panics on inputs where a `%` byte is
//! followed by a multi-byte UTF-8 sequence straddling a char boundary
//! (e.g. `"%Aé"`); for those inputs the oracle only requires the new
//! implementation not to panic.

#![allow(clippy::unwrap_used)]

mod reference {
    use std::fmt;

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
                                    reason: "must not contain double-encoded dot character"
                                        .to_string(),
                                });
                            }
                            // Triple encoding: %25 decodes to %, check next layer
                            if inner_val == 0x25 && i + 6 < bytes.len() {
                                let nested_hex = &path[i + 5..i + 7];
                                if let Ok(nested_val) = u8::from_str_radix(nested_hex, 16) {
                                    if nested_val == 0x2E {
                                        return Err(PathTraversalError {
                                            reason:
                                                "must not contain triple-encoded dot character"
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
}

fn outcome(result: Result<(), impl std::fmt::Display>) -> String {
    match result {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("err: {e}"),
    }
}

fn run_reference(path: &str) -> Option<String> {
    // The original panics on some multi-byte inputs; treat a panic as
    // "no reference outcome" and only require the new code not to panic.
    std::panic::catch_unwind(|| outcome(reference::validate_path_for_traversal(path))).ok()
}

#[test]
fn oracle_path_traversal() {
    let cases: Vec<String> = vec![
        String::new(),
        "/".into(),
        "media/movies".into(),
        "/absolute/path".into(),
        "folder with spaces/file.txt".into(),
        "file-with-dashes.txt".into(),
        "file_with_underscores.txt".into(),
        "single.dot".into(),
        "file.tar.gz".into(),
        "/path/with.dots/in/middle".into(),
        "unicode-file/résumé.txt".into(),
        "中文/目录/文件.mp4".into(),
        "path//to//file".into(),
        "path/to/file/".into(),
        "/leading/slash".into(),
        "../../../etc/passwd".into(),
        "../secret".into(),
        "test/../etc".into(),
        "/safe/../../etc".into(),
        "%2e%2e/etc/passwd".into(),
        "%2E%2E/secret".into(),
        "test/%2e%2e/config".into(),
        "/%2e%2e/../etc".into(),
        "..%2fetc/passwd".into(),
        "..%2Fetc/passwd".into(),
        "test/..%5cwindows".into(),
        "..\\..\\windows".into(),
        "test\\..\\config".into(),
        "..\\secret".into(),
        "\\..\\windows".into(),
        "./../etc".into(),
        ".././secret".into(),
        "././../config".into(),
        "./.././etc".into(),
        "test\0../etc".into(),
        "/etc/\0passwd".into(),
        "test\0file".into(),
        "/movies/video%00.mp4".into(),
        "%252e%252e/secret".into(),
        "%252E%252E/secret".into(),
        "%252e%252e".into(),
        "%25252e%25252e/secret".into(),
        "%25252E%25252E/secret".into(),
        "%2e%2E/secret".into(),
        "%2E%2e/secret".into(),
        "%2E%2E/secret".into(),
        ".%2e/secret".into(),
        "%2e./secret".into(),
        ".%2E/secret".into(),
        "file%2eext".into(),
        "%2ext".into(),
        "t%2ext".into(),
        "file%41name/dir%42/".repeat(26),
        "a".repeat(1024),
        "%2".into(),
        "%".into(),
        "%2e".into(),
        "x%2e".into(),
        "xx%2e".into(),
        "%25".into(),
        "%252".into(),
        "%252e".into(),
        "a%25%32%65b".into(),
        "%25252".into(),
        "%25252e".into(),
        "pct%zz%2ex".into(),
        "%2g2e".into(),
        "A".repeat(1023) + "%",
    ];

    for case in &cases {
        let reference_outcome = run_reference(case);
        let live_outcome =
            std::panic::catch_unwind(|| outcome(synctv_common::validation::validate_path_for_traversal(case)))
                .expect("new implementation must not panic");
        if let Some(expected) = reference_outcome {
            assert_eq!(
                live_outcome, expected,
                "behavior divergence for path {case:?}"
            );
        }
    }
}

// The original implementation panics on these inputs (byte slicing through
// a multi-byte char). Ignored until the optimized implementation lands; the
// optimization round removes the ignore.
#[test]
#[ignore = "known panic bug in the baseline implementation; enable after optimization"]
fn no_panic_on_multibyte_after_percent() {
    // The original implementation panics on these (byte slicing through a
    // multi-byte char); the optimized implementation must return normally.
    for path in ["%Aé", "%é2", "a%C3%A9%2e", "%\u{4e00}2e"] {
        let _ = synctv_common::validation::validate_path_for_traversal(path);
    }
}
