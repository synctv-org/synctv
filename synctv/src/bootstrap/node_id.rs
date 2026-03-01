/// Generate a unique node ID for this server instance.
/// Prefers the `POD_NAME` environment variable (set by Kubernetes downward API)
/// for predictable, consistent node IDs in K8s deployments.
/// Falls back to hostname + local IP + random suffix for non-K8s environments.
pub fn generate_node_id() -> String {
    // In Kubernetes, POD_NAME is injected via the downward API and provides
    // a stable, predictable identifier (e.g. "synctv-0", "synctv-abc123")
    if let Ok(pod_name) = std::env::var("POD_NAME") {
        if !pod_name.is_empty() {
            return pod_name;
        }
    }

    use std::net::UdpSocket;

    // Try to get hostname, fallback to "unknown"
    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .unwrap_or_else(|| "unknown".to_string());

    // Get local IP address if available
    let local_ip = UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("8.8.8.8:80").map(|()| s))
        .and_then(|s| s.local_addr())
        .map_or_else(|_| "0.0.0.0".to_string(), |addr| addr.ip().to_string());

    // Add random suffix for uniqueness
    let suffix = nanoid::nanoid!(6);

    format!("{hostname}_{local_ip}-{suffix}")
}
