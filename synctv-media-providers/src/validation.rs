//! Shared provider validation utilities.
//!
//! Runtime SSRF protection is enforced by the HTTP client during connection
//! establishment. These validators only check request structure and remain
//! compatible with self-hosted/private provider deployments.

use tonic::Status;

/// Validate that a host string is a non-empty, parseable URL.
#[allow(clippy::result_large_err)]
pub(crate) fn validate_host(host: &str) -> Result<(), Status> {
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
            )));
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

#[allow(clippy::result_large_err)]
pub(crate) fn validate_provider_grpc_host(host: &str) -> Result<(), Status> {
    validate_host(host)
}

#[allow(clippy::result_large_err)]
pub(crate) fn validate_required(field_name: &str, value: &str) -> Result<(), Status> {
    if value.trim().is_empty() {
        return Err(Status::invalid_argument(format!(
            "{field_name} must not be empty"
        )));
    }
    Ok(())
}

#[allow(clippy::result_large_err)]
pub(crate) fn validate_provider_auth(host: &str, token: &str) -> Result<(), Status> {
    validate_provider_grpc_host(host)?;
    validate_required("token", token)?;
    Ok(())
}

#[allow(clippy::result_large_err)]
pub(crate) fn validate_provider_user_auth(
    host: &str,
    token: &str,
    user_id: &str,
) -> Result<(), Status> {
    validate_provider_auth(host, token)?;
    validate_required("user_id", user_id)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_hosts() {
        assert!(validate_host("https://example.com").is_ok());
        assert!(validate_host("http://my-alist.example.com:5244").is_ok());
        assert!(validate_host("https://emby.myserver.org/emby").is_ok());
        assert!(validate_host("http://192.168.1.100:5244").is_ok());
        assert!(validate_host("http://10.0.0.1:5244").is_ok());
        assert!(validate_host("https://jellyfin.home.local:8096").is_ok());
    }

    #[test]
    fn private_and_local_hosts_are_allowed_at_validation_layer() {
        for host in [
            "http://127.0.0.1:5244",
            "http://192.168.1.100:5244",
            "http://169.254.169.254",
            "http://localhost:8096",
            "http://metadata.google.internal",
            "http://[::1]:5244",
            "http://[::ffff:127.0.0.1]",
        ] {
            assert!(validate_host(host).is_ok(), "expected valid host: {host}");
        }
    }

    #[test]
    fn provider_grpc_host_preserves_self_hosted_targets() {
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
                "expected valid provider host: {host}"
            );
        }
    }

    #[test]
    fn provider_grpc_host_allows_public_targets() {
        for host in [
            "https://example.com",
            "https://emby.myserver.org/emby",
            "http://93.184.216.34:5244",
        ] {
            assert!(
                validate_provider_grpc_host(host).is_ok(),
                "expected public host to pass: {host}"
            );
        }
    }

    #[test]
    fn blocked_schemes() {
        assert!(validate_host("ftp://example.com").is_err());
        assert!(validate_host("file:///etc/passwd").is_err());
        assert!(validate_host("gopher://evil.com").is_err());
    }

    #[test]
    fn empty_host() {
        assert!(validate_host("").is_err());
    }

    #[test]
    fn invalid_url() {
        assert!(validate_host("not-a-url").is_err());
    }

    #[test]
    fn host_missing_host_component() {
        let status = validate_host("http://").expect_err("validation should fail");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("host"));
    }

    #[test]
    fn required_empty() {
        let status = validate_required("username", "").expect_err("validation should fail");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("username"));
    }

    #[test]
    fn required_non_empty() {
        assert!(validate_required("username", "alice").is_ok());
        assert!(validate_required("token", "abc123").is_ok());
    }

    #[test]
    fn provider_auth_helpers_validate_common_fields() {
        assert!(validate_provider_auth("https://example.com", "token").is_ok());
        assert!(validate_provider_user_auth("https://example.com", "token", "user").is_ok());

        let status = validate_provider_auth("https://example.com", "").expect_err("token required");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("token"));

        let status = validate_provider_user_auth("https://example.com", "token", "")
            .expect_err("user_id required");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("user_id"));
    }

    #[test]
    fn host_with_port() {
        assert!(validate_host("https://example.com:443").is_ok());
        assert!(validate_host("http://example.com:8080").is_ok());
    }

    #[test]
    fn host_with_path() {
        assert!(validate_host("https://example.com/api/v1").is_ok());
        assert!(validate_host("https://example.com/emby").is_ok());
    }

    #[test]
    fn host_rejects_userinfo() {
        let status =
            validate_host("https://user:pass@example.com").expect_err("validation should fail");
        assert_eq!(status.code(), tonic::Code::InvalidArgument);
        assert!(status.message().contains("userinfo"));
    }

    #[test]
    fn host_rejects_query_and_fragment() {
        for host in [
            "https://example.com/emby?token=secret",
            "https://example.com/emby#fragment",
        ] {
            let status = validate_host(host).expect_err("validation should fail");
            assert_eq!(status.code(), tonic::Code::InvalidArgument);
            assert!(
                status.message().contains("query") || status.message().contains("fragment"),
                "unexpected message for {host}: {}",
                status.message()
            );
        }
    }
}
