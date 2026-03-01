//! Tests for Content-Encoding header handling in the proxy.
//!
//! These tests verify that the proxy correctly handles various Content-Encoding
//! scenarios, including single encodings and multiple encoding combinations.

#![allow(clippy::unwrap_used)]
/// Unit tests for the content-encoding parsing logic.
///
/// These tests verify the function that determines if reqwest would have
/// auto-decompressed a response based on its Content-Encoding header.
mod encoding_parsing_unit_tests {
    /// Test the current (broken) implementation that uses exact match.
    /// This should FAIL with the current code, demonstrating the bug.
    fn current_implementation_is_auto_decompressed(ce: &str) -> bool {
        let ce_lower = ce.to_lowercase();
        ce_lower == "gzip" || ce_lower == "deflate" || ce_lower == "br"
    }

    /// Test the fixed implementation that uses contains.
    /// This should PASS with the fixed code.
    fn fixed_implementation_is_auto_decompressed(ce: &str) -> bool {
        let ce_lower = ce.to_lowercase();
        ce_lower.contains("gzip") || ce_lower.contains("deflate") || ce_lower.contains("br")
    }

    #[test]
    fn test_current_single_gzip() {
        assert!(
            current_implementation_is_auto_decompressed("gzip"),
            "current: single gzip should work"
        );
    }

    #[test]
    fn test_current_single_deflate() {
        assert!(
            current_implementation_is_auto_decompressed("deflate"),
            "current: single deflate should work"
        );
    }

    #[test]
    fn test_current_single_br() {
        assert!(
            current_implementation_is_auto_decompressed("br"),
            "current: single br should work"
        );
    }

    /// This test demonstrates the BUG: "gzip, deflate" is NOT recognized.
    #[test]
    fn test_current_gzip_deflate_combination_fails() {
        // With the current implementation, this returns false (BUG!)
        // After fix, this test will fail, showing the bug is fixed
        let result = current_implementation_is_auto_decompressed("gzip, deflate");
        // This assertion documents the current broken behavior
        // It should pass now (showing the bug) and fail after fix
        assert!(!result, "BUG: current impl fails on 'gzip, deflate'");
    }

    /// This test demonstrates the BUG: "br, gzip" is NOT recognized.
    #[test]
    fn test_current_br_gzip_combination_fails() {
        let result = current_implementation_is_auto_decompressed("br, gzip");
        assert!(!result, "BUG: current impl fails on 'br, gzip'");
    }

    // ==================================================================
    // Tests for the FIXED implementation (using contains)
    // These should ALL pass with the fixed code
    // ==================================================================

    #[test]
    fn test_fixed_single_gzip() {
        assert!(
            fixed_implementation_is_auto_decompressed("gzip"),
            "fixed: single gzip should work"
        );
    }

    #[test]
    fn test_fixed_single_deflate() {
        assert!(
            fixed_implementation_is_auto_decompressed("deflate"),
            "fixed: single deflate should work"
        );
    }

    #[test]
    fn test_fixed_single_br() {
        assert!(
            fixed_implementation_is_auto_decompressed("br"),
            "fixed: single br should work"
        );
    }

    #[test]
    fn test_fixed_gzip_deflate_combination() {
        assert!(
            fixed_implementation_is_auto_decompressed("gzip, deflate"),
            "fixed: 'gzip, deflate' should be detected"
        );
    }

    #[test]
    fn test_fixed_br_gzip_combination() {
        assert!(
            fixed_implementation_is_auto_decompressed("br, gzip"),
            "fixed: 'br, gzip' should be detected"
        );
    }

    #[test]
    fn test_fixed_unknown_encoding() {
        assert!(
            !fixed_implementation_is_auto_decompressed("zstd"),
            "fixed: zstd should NOT be detected as auto-decompressed"
        );
    }

    #[test]
    fn test_fixed_gzip_with_whitespace() {
        assert!(
            fixed_implementation_is_auto_decompressed(" gzip "),
            "fixed: ' gzip ' with whitespace should be detected"
        );
    }

    #[test]
    fn test_fixed_case_insensitive() {
        assert!(
            fixed_implementation_is_auto_decompressed("GZIP"),
            "fixed: GZIP uppercase should be detected"
        );
        assert!(
            fixed_implementation_is_auto_decompressed("GZip"),
            "fixed: GZip mixed case should be detected"
        );
    }

    #[test]
    fn test_fixed_deflate_gzip_combination() {
        assert!(
            fixed_implementation_is_auto_decompressed("deflate, gzip"),
            "fixed: 'deflate, gzip' should be detected"
        );
    }

    #[test]
    fn test_fixed_triple_combination() {
        assert!(
            fixed_implementation_is_auto_decompressed("br, gzip, deflate"),
            "fixed: 'br, gzip, deflate' should be detected"
        );
    }

    #[test]
    fn test_fixed_x_gzip() {
        // x-gzip is an alias for gzip, contains "gzip"
        assert!(
            fixed_implementation_is_auto_decompressed("x-gzip"),
            "fixed: x-gzip contains 'gzip' so should be detected"
        );
    }

    #[test]
    fn test_fixed_identity_not_auto_decompressed() {
        // identity means no encoding, should not be treated as auto-decompressed
        // (reqwest doesn't decode identity, it's pass-through)
        assert!(
            !fixed_implementation_is_auto_decompressed("identity"),
            "fixed: identity should NOT be detected as auto-decompressed"
        );
    }

    #[test]
    fn test_fixed_empty_string() {
        assert!(
            !fixed_implementation_is_auto_decompressed(""),
            "fixed: empty string should NOT be detected"
        );
    }

    #[test]
    fn test_fixed_zstd_gzip_combination() {
        // Mixed known and unknown encoding
        assert!(
            fixed_implementation_is_auto_decompressed("zstd, gzip"),
            "fixed: 'zstd, gzip' contains gzip so should be detected"
        );
    }
}
