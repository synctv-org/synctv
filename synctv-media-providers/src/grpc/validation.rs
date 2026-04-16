//! Shared validation utilities for gRPC server layers
//!
//! Provides common field validators for gRPC request/response handling.
//! Runtime SSRF protection is enforced at the HTTP client layer during
//! connection establishment; the gRPC layer only performs structural request
//! validation and must remain compatible with self-hosted/private deployments.

use tonic::Status;

/// Validate that a host string is a non-empty, parseable URL.
///
/// Checks:
/// - URL is parseable
/// - Scheme is http or https only
/// - URL contains a host component
///
/// SSRF protection (private IP blocking, DNS rebinding) is handled by
/// the SSRF-safe DNS resolver at HTTP connection time.
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; boxing would break gRPC API
pub fn validate_host(host: &str) -> Result<(), Status> {
    if host.is_empty() {
        return Err(Status::invalid_argument("host URL must not be empty"));
    }

    let parsed = url::Url::parse(host)
        .map_err(|e| Status::invalid_argument(format!("invalid host URL: {e}")))?;

    match parsed.scheme() {
        "http" | "https" => {}
        scheme => {
            return Err(Status::invalid_argument(format!(
                "unsupported scheme '{scheme}': only http and https are allowed"
            )))
        }
    }

    if parsed.host().is_none() {
        return Err(Status::invalid_argument(
            "host URL must contain a valid host component",
        ));
    }

    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(Status::invalid_argument(
            "host URL must not include userinfo credentials",
        ));
    }

    if parsed.query().is_some() {
        return Err(Status::invalid_argument(
            "host URL must not include query parameters",
        ));
    }

    if parsed.fragment().is_some() {
        return Err(Status::invalid_argument(
            "host URL must not include fragments",
        ));
    }

    Ok(())
}

/// Validate a provider host URL for gRPC request input.
///
/// This intentionally preserves the same URL semantics as [`validate_host`].
/// Self-hosted/private endpoints are supported and rely on transport-time SSRF
/// protection instead of static gRPC-layer blocking.
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; boxing would break gRPC API
pub fn validate_provider_grpc_host(host: &str) -> Result<(), Status> {
    validate_host(host)
}

/// Validate that a required string field is non-empty.
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; boxing would break gRPC API
pub fn validate_required(field_name: &str, value: &str) -> Result<(), Status> {
    if value.trim().is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must not be empty"
        )));
    }
    Ok(())
}

/// Maximum length for provider names
pub const PROVIDER_NAME_MAX: usize = 64;

/// Minimum length for provider names
pub const PROVIDER_NAME_MIN: usize = 1;

/// Validate a provider instance name.
///
/// Provider names must:
/// - Be non-empty
/// - Be at most 64 characters
/// - Contain only ASCII alphanumeric characters, underscores, or hyphens
///
/// This validation prevents injection attacks and ensures names are
/// safe for use in file paths, database queries, and logging.
#[allow(clippy::result_large_err)] // tonic::Status is inherently large; boxing would break gRPC API
pub fn validate_provider_name(name: &str) -> Result<String, Status> {
    let trimmed = name.trim();

    let len = trimmed.len();
    if len < PROVIDER_NAME_MIN {
        return Err(Status::invalid_argument("provider name must not be empty"));
    }
    if len > PROVIDER_NAME_MAX {
        return Err(Status::invalid_argument(format!(
            "provider name must be at most {PROVIDER_NAME_MAX} characters (got {len})"
        )));
    }

    // Only allow ASCII alphanumeric, underscores, and hyphens to prevent injection
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(Status::invalid_argument(
            "provider name must contain only ASCII alphanumeric characters, underscores, or hyphens",
        ));
    }

    Ok(trimmed.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_hosts() {
        assert!(validate_host("https://example.com").is_ok());
        assert!(validate_host("http://my-alist.example.com:5244").is_ok());
        assert!(validate_host("https://emby.myserver.org/emby").is_ok());
        assert!(validate_host("http://192.168.1.100:5244").is_ok());
        assert!(validate_host("http://10.0.0.1:5244").is_ok());
        assert!(validate_host("https://jellyfin.home.local:8096").is_ok());
    }

    #[test]
    fn test_private_and_local_hosts_are_allowed_at_validation_layer() {
        for host in [
            "http://127.0.0.1:5244",
            "http://192.168.1.100:5244",
            "http://169.254.169.254",
            "http://localhost:8096",
            "http://metadata.google.internal",
            "http://[::1]:5244",
            "http://[::ffff:127.0.0.1]",
        ] {
            assert!(
                validate_host(host).is_ok(),
                "transport-level SSRF controls should handle host blocking: {host}"
            );
        }
    }

    #[test]
    fn test_validate_provider_grpc_host_preserves_self_hosted_targets() {
        for host in [
            "http://127.0.0.1:5244",
            "http://192.168.1.100:5244",
            "http://10.0.0.1:5244",
            "http://169.254.169.254",
            "http://localhost:8096",
            "http://metadata.google.internal",
            "http://[::1]:5244",
            "http://[::ffff:127.0.0.1]",
        ] {
            assert!(
                validate_provider_grpc_host(host).is_ok(),
                "gRPC validation should stay compatible with self-hosted/private targets: {host}"
            );
        }
    }

    #[test]
    fn test_validate_provider_grpc_host_allows_public_targets() {
        for host in [
            "https://example.com",
            "https://emby.myserver.org/emby",
            "http://93.184.216.34:5244",
        ] {
            assert!(
                validate_provider_grpc_host(host).is_ok(),
                "expected public host to pass gRPC validation: {host}"
            );
        }
    }

    #[test]
    fn test_blocked_schemes() {
        assert!(validate_host("ftp://example.com").is_err());
        assert!(validate_host("file:///etc/passwd").is_err());
        assert!(validate_host("gopher://evil.com").is_err());
    }

    #[test]
    fn test_empty_host() {
        assert!(validate_host("").is_err());
    }

    #[test]
    fn test_invalid_url() {
        assert!(validate_host("not-a-url").is_err());
    }

    #[test]
    fn test_validate_host_missing_host_component() {
        // URLs with http/https scheme but no actual host should be rejected
        let result = validate_host("http://");
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("host"));
    }

    #[test]
    fn test_validate_required_empty() {
        let result = validate_required("username", "");
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("username"));
    }

    #[test]
    fn test_validate_required_non_empty() {
        assert!(validate_required("username", "alice").is_ok());
        assert!(validate_required("token", "abc123").is_ok());
    }

    #[test]
    fn test_validate_host_with_port() {
        assert!(validate_host("https://example.com:443").is_ok());
        assert!(validate_host("http://example.com:8080").is_ok());
    }

    #[test]
    fn test_validate_host_with_path() {
        assert!(validate_host("https://example.com/api/v1").is_ok());
        assert!(validate_host("https://example.com/emby").is_ok());
    }

    #[test]
    fn test_validate_host_rejects_userinfo() {
        let result = validate_host("https://user:pass@example.com");
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("userinfo"));
    }

    #[test]
    fn test_validate_host_rejects_query_and_fragment() {
        for host in [
            "https://example.com/emby?token=secret",
            "https://example.com/emby#fragment",
        ] {
            let result = validate_host(host);
            assert!(result.is_err(), "expected invalid host: {host}");
            let status = result.unwrap_err();
            assert_eq!(status.code(), tonic::Code::InvalidArgument);
            assert!(
                status.message().contains("query") || status.message().contains("fragment"),
                "unexpected message for {host}: {}",
                status.message()
            );
        }
    }

    #[test]
    fn test_validate_provider_name_valid() {
        assert!(validate_provider_name("provider1").is_ok());
        assert!(validate_provider_name("my-provider").is_ok());
        assert!(validate_provider_name("my_provider").is_ok());
        assert!(validate_provider_name("Provider123").is_ok());
        assert!(validate_provider_name("abc").is_ok());
        assert!(validate_provider_name("test_provider-123").is_ok());
    }

    #[test]
    fn test_validate_provider_name_empty() {
        let result = validate_provider_name("");
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("empty"));
    }

    #[test]
    fn test_validate_provider_name_whitespace_only() {
        let result = validate_provider_name("   ");
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("empty"));
    }

    #[test]
    fn test_validate_provider_name_too_long() {
        let long_name = "a".repeat(PROVIDER_NAME_MAX + 1);
        let result = validate_provider_name(&long_name);
        assert!(result.is_err());
        let status = result.unwrap_err();
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("64"));
    }

    #[test]
    fn test_validate_provider_name_max_length_valid() {
        let max_length_name = "a".repeat(PROVIDER_NAME_MAX);
        let result = validate_provider_name(&max_length_name);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), PROVIDER_NAME_MAX);
    }

    #[test]
    fn test_validate_provider_name_special_characters() {
        let invalid_names = vec![
            "test<script>",
            "test>alert",
            "test\"quote",
            "test'apostrophe",
            "test space",
            "test/slash",
            "test\\backslash",
            "test;drop",
            "test& amp",
            "test@email",
            "test!bang",
            "test#hash",
            "test$dollar",
            "test%percent",
            "test*star",
            "test(paren)",
            "test+plus",
            "test=equals",
            "test[bracket]",
            "test{brace}",
            "test|pipe",
            "test,comma",
            "test.period",
            "test:colon",
        ];
        for invalid_name in invalid_names {
            let result = validate_provider_name(invalid_name);
            assert!(result.is_err(), "Expected '{invalid_name}' to be invalid");
            let status = result.unwrap_err();
            assert_eq!(status.code(), tonic::Code::InvalidArgument);
        }
    }

    #[test]
    fn test_validate_provider_name_unicode() {
        let unicode_names = vec!["测试provider", "провайдер", "プロバイダー", "provider🎉"];
        for invalid_name in unicode_names {
            let result = validate_provider_name(invalid_name);
            assert!(result.is_err(), "Expected '{invalid_name}' to be invalid");
        }
    }

    #[test]
    fn test_validate_provider_name_trimmed() {
        let result = validate_provider_name("  valid_name  ");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "valid_name");
    }

    #[test]
    fn test_validate_provider_name_numeric_only() {
        assert!(validate_provider_name("123").is_ok());
        assert!(validate_provider_name("00123").is_ok());
    }

    #[test]
    fn test_validate_provider_name_single_char() {
        assert!(validate_provider_name("a").is_ok());
        assert!(validate_provider_name("Z").is_ok());
        assert!(validate_provider_name("1").is_ok());
        assert!(validate_provider_name("_").is_ok());
        assert!(validate_provider_name("-").is_ok());
    }
}
