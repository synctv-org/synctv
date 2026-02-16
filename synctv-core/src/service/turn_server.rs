//! Built-in STUN server powered by turn-rs.
//!
//! Starts turn-rs with STUN-only configuration (no auth configured,
//! so TURN allocations are rejected while STUN Binding requests work).
//! This is multi-replica safe since STUN is stateless.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// STUN server configuration
#[derive(Debug, Clone)]
pub struct StunServerConfig {
    /// Bind address (e.g., "0.0.0.0:3478")
    pub bind_addr: String,
    /// External address for reflexive candidates (public IP:port)
    pub external_addr: String,
}

impl Default for StunServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:3478".to_string(),
            external_addr: "0.0.0.0:3478".to_string(),
        }
    }
}

/// Returns `true` if the IP is unspecified (0.0.0.0 / ::) or a private/loopback address.
fn is_unusable_external_ip(ip: &IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(_) => false, // IPv6 ULA detection not critical here
    }
}

/// Attempt to resolve a publicly routable external IP address.
///
/// Tries sources in order:
/// 1. `STUN_EXTERNAL_IP` env var (explicit override)
/// 2. `POD_IP` env var (Kubernetes downward API)
/// 3. AWS EC2 instance metadata (IMDSv2)
/// 4. GCP instance metadata
/// 5. Azure instance metadata (IMDS)
///
/// Returns `None` if no routable address could be determined.
pub async fn resolve_external_ip() -> Option<IpAddr> {
    // 1. Explicit env var override
    if let Some(ip) = ip_from_env("STUN_EXTERNAL_IP") {
        tracing::info!(ip = %ip, "Resolved external IP from STUN_EXTERNAL_IP env var");
        return Some(ip);
    }

    // 2. Kubernetes downward API
    if let Some(ip) = ip_from_env("POD_IP") {
        // POD_IP may be a cluster-internal IP, but in K8s with hostNetwork
        // or a LoadBalancer it could be routable. Accept it as a candidate.
        tracing::info!(ip = %ip, "Resolved external IP from POD_IP env var");
        return Some(ip);
    }

    // 3. Cloud metadata services (best-effort, with short timeouts)
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_secs(1))
        .build()
    {
        Ok(c) => c,
        Err(_) => return None,
    };

    // AWS IMDSv2
    if let Some(ip) = try_aws_metadata(&client).await {
        tracing::info!(ip = %ip, "Resolved external IP from AWS EC2 metadata");
        return Some(ip);
    }

    // GCP
    if let Some(ip) = try_gcp_metadata(&client).await {
        tracing::info!(ip = %ip, "Resolved external IP from GCP metadata");
        return Some(ip);
    }

    // Azure
    if let Some(ip) = try_azure_metadata(&client).await {
        tracing::info!(ip = %ip, "Resolved external IP from Azure IMDS");
        return Some(ip);
    }

    None
}

fn ip_from_env(var: &str) -> Option<IpAddr> {
    std::env::var(var)
        .ok()
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_unspecified())
}

async fn try_aws_metadata(client: &reqwest::Client) -> Option<IpAddr> {
    // IMDSv2: get token first
    let token = client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "30")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    let ip_str = client
        .get("http://169.254.169.254/latest/meta-data/public-ipv4")
        .header("X-aws-ec2-metadata-token", token.trim())
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    ip_str.trim().parse::<IpAddr>().ok()
}

async fn try_gcp_metadata(client: &reqwest::Client) -> Option<IpAddr> {
    let ip_str = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/network-interfaces/0/access-configs/0/external-ip")
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    ip_str.trim().parse::<IpAddr>().ok()
}

async fn try_azure_metadata(client: &reqwest::Client) -> Option<IpAddr> {
    let resp = client
        .get("http://169.254.169.254/metadata/instance/network/interface/0/ipv4/ipAddress/0/publicIpAddress?api-version=2021-02-01&format=text")
        .header("Metadata", "true")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    resp.trim().parse::<IpAddr>().ok()
}

/// Validate that a resolved external address is usable for STUN.
///
/// Returns an error message if the address is not routable.
pub fn validate_external_addr(addr: &str) -> Result<(), String> {
    let sock: SocketAddr = addr
        .parse()
        .map_err(|e| format!("Invalid STUN external address '{addr}': {e}"))?;

    if is_unusable_external_ip(&sock.ip()) {
        return Err(format!(
            "STUN external address '{}' is not routable (unspecified, loopback, or private). \
             Set SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR to a public IP, or set STUN_EXTERNAL_IP / POD_IP env var. \
             In Kubernetes, use the pod's host IP or a LoadBalancer IP.",
            addr
        ));
    }

    Ok(())
}

/// Built-in STUN server backed by turn-rs.
///
/// Runs turn-rs with no authentication configured, so:
/// - STUN Binding requests work (stateless, no auth needed)
/// - TURN Allocate requests are rejected (auth required but none configured)
pub struct StunServer {
    task: JoinHandle<()>,
    local_addr: SocketAddr,
}

impl StunServer {
    /// Start the STUN server.
    ///
    /// Configures turn-rs with a single UDP interface and no authentication,
    /// so only STUN Binding requests succeed.
    pub async fn start(config: StunServerConfig) -> anyhow::Result<Arc<Self>> {
        let listen: SocketAddr = config
            .bind_addr
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid bind_addr '{}': {e}", config.bind_addr))?;
        let external: SocketAddr = config
            .external_addr
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid external_addr '{}': {e}", config.external_addr))?;

        let turn_config = turn_server::config::Config {
            server: turn_server::config::Server {
                realm: "synctv".to_string(),
                interfaces: vec![turn_server::config::Interface::Udp {
                    listen,
                    external,
                    idle_timeout: 60,
                    mtu: 1500,
                }],
                ..Default::default()
            },
            ..Default::default()
        };

        let local_addr = listen;

        let task = crate::spawn::spawn_monitored("stun_server", async move {
            if let Err(e) = turn_server::start_server(turn_config).await {
                tracing::error!(error = %e, "STUN server (turn-rs) exited with error");
            }
        });

        tracing::info!(
            bind_addr = %listen,
            external_addr = %external,
            "STUN server started (powered by turn-rs)"
        );

        Ok(Arc::new(Self { task, local_addr }))
    }

    /// Get the local bind address.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Shut down the STUN server by aborting the background task.
    pub async fn shutdown(&self) {
        self.task.abort();
    }
}

/// TURN credential generated using HMAC-SHA1 (coturn `use-auth-secret` compatible).
///
/// The credential format follows the coturn ephemeral credential mechanism:
/// - `username` = `<expiry_unix_timestamp>:<user_id>`
/// - `password` = Base64(HMAC-SHA1(`shared_secret`, `username`))
///
/// Clients present these credentials to the TURN server, which validates them
/// using the same shared secret. Credentials expire after `ttl_seconds`.
#[derive(Debug, Clone)]
pub struct TurnCredential {
    /// The username in `timestamp:userid` format
    pub username: String,
    /// The Base64-encoded HMAC-SHA1 password
    pub password: String,
    /// Unix timestamp when this credential expires
    pub expiry_timestamp: u64,
}

/// Generate time-limited TURN credentials using HMAC-SHA1.
///
/// Compatible with coturn's `use-auth-secret` / `static-auth-secret` mode
/// and other TURN servers that implement the REST API for ephemeral credentials
/// (draft-uberti-behave-turn-rest).
///
/// # Arguments
/// * `shared_secret` - The secret shared between this server and the TURN server
/// * `user_id` - An identifier for the user (included in the username for auditing)
/// * `ttl_seconds` - How long the credential should be valid (seconds from now)
///
/// # Returns
/// A `TurnCredential` with the generated username, password, and expiry time.
pub fn generate_turn_credentials(
    shared_secret: &str,
    user_id: &str,
    ttl_seconds: u64,
) -> TurnCredential {
    use base64::Engine as _;
    use hmac::{Hmac, Mac};
    use sha1::Sha1;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let expiry_timestamp = now.saturating_add(ttl_seconds);

    // Username format: <expiry_timestamp>:<user_id>
    let username = format!("{expiry_timestamp}:{user_id}");

    // Password: Base64(HMAC-SHA1(secret, username))
    type HmacSha1 = Hmac<Sha1>;
    let mut mac = HmacSha1::new_from_slice(shared_secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(username.as_bytes());
    let result = mac.finalize();
    let password = base64::engine::general_purpose::STANDARD.encode(result.into_bytes());

    TurnCredential {
        username,
        password,
        expiry_timestamp,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stun_server_config_default() {
        let config = StunServerConfig::default();
        assert_eq!(config.bind_addr, "0.0.0.0:3478");
        assert_eq!(config.external_addr, "0.0.0.0:3478");
    }

    #[test]
    fn test_validate_external_addr_public_ip() {
        assert!(validate_external_addr("203.0.113.1:3478").is_ok());
    }

    #[test]
    fn test_validate_external_addr_unspecified() {
        let err = validate_external_addr("0.0.0.0:3478").unwrap_err();
        assert!(err.contains("not routable"));
    }

    #[test]
    fn test_validate_external_addr_loopback() {
        let err = validate_external_addr("127.0.0.1:3478").unwrap_err();
        assert!(err.contains("not routable"));
    }

    #[test]
    fn test_validate_external_addr_private_rfc1918() {
        assert!(validate_external_addr("10.0.0.1:3478").unwrap_err().contains("not routable"));
        assert!(validate_external_addr("172.16.0.1:3478").unwrap_err().contains("not routable"));
        assert!(validate_external_addr("192.168.1.1:3478").unwrap_err().contains("not routable"));
    }

    #[test]
    fn test_validate_external_addr_link_local() {
        let err = validate_external_addr("169.254.1.1:3478").unwrap_err();
        assert!(err.contains("not routable"));
    }

    #[test]
    fn test_validate_external_addr_invalid_format() {
        let err = validate_external_addr("not-an-address").unwrap_err();
        assert!(err.contains("Invalid STUN external address"));
    }

    #[test]
    fn test_is_unusable_external_ip() {
        assert!(is_unusable_external_ip(&"0.0.0.0".parse().expect("valid")));
        assert!(is_unusable_external_ip(&"127.0.0.1".parse().expect("valid")));
        assert!(is_unusable_external_ip(&"10.0.0.1".parse().expect("valid")));
        assert!(is_unusable_external_ip(&"172.16.0.1".parse().expect("valid")));
        assert!(is_unusable_external_ip(&"192.168.0.1".parse().expect("valid")));
        assert!(is_unusable_external_ip(&"169.254.0.1".parse().expect("valid")));
        assert!(!is_unusable_external_ip(&"8.8.8.8".parse().expect("valid")));
        assert!(!is_unusable_external_ip(&"203.0.113.1".parse().expect("valid")));
    }

    #[test]
    fn test_generate_turn_credentials_format() {
        let cred = generate_turn_credentials("my-secret", "user123", 86400);

        // Username should be in "timestamp:userid" format
        assert!(cred.username.contains(':'));
        let parts: Vec<&str> = cred.username.splitn(2, ':').collect();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[1], "user123");

        // Timestamp part should be a valid number
        let ts: u64 = parts[0].parse().expect("timestamp should be a number");
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // Expiry should be roughly now + 86400 (within 5 seconds tolerance)
        assert!(ts >= now + 86400 - 5);
        assert!(ts <= now + 86400 + 5);

        // Password should be valid Base64
        assert!(!cred.password.is_empty());
        {
            use base64::Engine;
            base64::engine::general_purpose::STANDARD
                .decode(&cred.password)
                .expect("password should be valid Base64");
        }

        // Expiry timestamp should match
        assert_eq!(cred.expiry_timestamp, ts);
    }

    #[test]
    fn test_generate_turn_credentials_deterministic() {
        // Same inputs at the same time should produce the same output
        let cred1 = generate_turn_credentials("secret", "user1", 3600);
        let cred2 = generate_turn_credentials("secret", "user1", 3600);

        // Timestamps should be within 1 second of each other
        assert!(cred1.expiry_timestamp.abs_diff(cred2.expiry_timestamp) <= 1);

        // If timestamps match, passwords should match
        if cred1.username == cred2.username {
            assert_eq!(cred1.password, cred2.password);
        }
    }

    #[test]
    fn test_generate_turn_credentials_different_secrets() {
        let cred1 = generate_turn_credentials("secret-a", "user1", 3600);
        let cred2 = generate_turn_credentials("secret-b", "user1", 3600);

        // Different secrets should produce different passwords
        // (unless by astronomically unlikely collision)
        if cred1.username == cred2.username {
            assert_ne!(cred1.password, cred2.password);
        }
    }

    #[test]
    fn test_generate_turn_credentials_different_users() {
        let cred1 = generate_turn_credentials("secret", "alice", 3600);
        let cred2 = generate_turn_credentials("secret", "bob", 3600);

        // Different user IDs should produce different usernames
        assert_ne!(cred1.username, cred2.username);
    }

    #[test]
    fn test_generate_turn_credentials_zero_ttl() {
        let cred = generate_turn_credentials("secret", "user1", 0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // With TTL=0, expiry should be approximately now
        assert!(cred.expiry_timestamp.abs_diff(now) <= 2);
    }
}
