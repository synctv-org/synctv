/// Default timeout for network operations when detecting local IP.
const LOCAL_IP_TIMEOUT_SECS: u64 = 2;

/// Generate a unique node ID for this server instance.
/// Prefers the `POD_NAME` environment variable (set by Kubernetes downward API)
/// for predictable, consistent node IDs in K8s deployments.
/// Falls back to hostname + local IP + random suffix for non-K8s environments.
///
/// Network operations are wrapped with a timeout to prevent blocking
/// when the network is unavailable.
pub fn generate_node_id() -> String {
    // In Kubernetes, POD_NAME is injected via the downward API and provides
    // a stable, predictable identifier (e.g. "synctv-0", "synctv-abc123")
    if let Ok(pod_name) = std::env::var("POD_NAME") {
        if !pod_name.is_empty() {
            return pod_name;
        }
    }

    // Try to get hostname, fallback to "unknown"
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    // Get local IP address with timeout to prevent blocking
    let local_ip = get_local_ip_with_timeout(LOCAL_IP_TIMEOUT_SECS);

    // Add random suffix for uniqueness
    let suffix = nanoid::nanoid!(6);

    format!("{hostname}_{local_ip}-{suffix}")
}

/// Get local IP address with a timeout to prevent blocking when network is unavailable.
fn get_local_ip_with_timeout(timeout_secs: u64) -> String {
    use std::net::UdpSocket;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    // If timeout is zero, return fallback immediately
    if timeout_secs == 0 {
        return "0.0.0.0".to_string();
    }

    // Use a channel to receive the result from the spawned thread
    let (tx, rx) = mpsc::channel();

    thread::spawn(move || {
        let result = UdpSocket::bind("0.0.0.0:0")
            .and_then(|s| s.connect("8.8.8.8:80").map(|()| s))
            .and_then(|s| s.local_addr())
            .map(|addr| addr.ip().to_string());
        // Send result, ignore if receiver is gone (timeout)
        let _ = tx.send(result);
    });

    // Wait with timeout
    match rx.recv_timeout(Duration::from_secs(timeout_secs)) {
        Ok(Ok(ip)) => ip,
        Ok(Err(_)) | Err(_) => "0.0.0.0".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_node_id_format() {
        let node_id = generate_node_id();
        // Should contain hostname, IP (or 0.0.0.0), and a 6-char suffix
        // Format: {hostname}_{local_ip}-{suffix}
        assert!(
            node_id.contains('_'),
            "node_id should contain underscore: {node_id}"
        );
        assert!(
            node_id.contains('-'),
            "node_id should contain hyphen: {node_id}"
        );
    }

    #[test]
    fn test_generate_node_id_with_pod_name() {
        // Set POD_NAME environment variable
        std::env::set_var("POD_NAME", "test-pod-123");
        let node_id = generate_node_id();
        assert_eq!(node_id, "test-pod-123", "Should use POD_NAME when set");
        std::env::remove_var("POD_NAME");
    }

    #[test]
    fn test_generate_node_id_empty_pod_name() {
        // Empty POD_NAME should fall back to hostname-based ID
        std::env::set_var("POD_NAME", "");
        let node_id = generate_node_id();
        // Should not be empty string
        assert!(!node_id.is_empty(), "node_id should not be empty");
        assert!(node_id.contains('_'), "Should use hostname-based format");
        std::env::remove_var("POD_NAME");
    }

    #[test]
    fn test_generate_node_id_unique() {
        std::env::remove_var("POD_NAME");
        let id1 = generate_node_id();
        let id2 = generate_node_id();
        // Should have different suffixes
        assert_ne!(id1, id2, "Each call should generate a unique ID");
    }

    #[test]
    fn test_get_local_ip_with_timeout_completes_quickly() {
        use std::time::Instant;

        let start = Instant::now();
        let ip = get_local_ip_with_timeout(1);
        let elapsed = start.elapsed();

        // Should complete within 1 second + some overhead
        assert!(
            elapsed.as_secs() < 2,
            "Should complete within timeout, took {:?}",
            elapsed
        );

        // Should return a valid IP string (either real IP or fallback)
        assert!(
            ip.parse::<std::net::IpAddr>().is_ok() || ip == "0.0.0.0",
            "Should return valid IP or fallback: {}",
            ip
        );
    }

    #[test]
    fn test_get_local_ip_with_timeout_zero_seconds() {
        // Zero timeout should immediately return fallback
        let ip = get_local_ip_with_timeout(0);
        assert_eq!(ip, "0.0.0.0", "Zero timeout should return fallback");
    }
}
