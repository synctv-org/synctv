/// Generate a unique node ID for this server instance.
///
/// Prefers the `POD_NAME` environment variable (set by Kubernetes downward API)
/// for predictable, consistent node IDs in K8s deployments.
/// Falls back to `POD_IP`, then hostname, then `unknown` for non-K8s environments.
/// This function intentionally avoids outbound network probing during startup.
pub fn generate_node_id() -> String {
    // In Kubernetes, POD_NAME is injected via the downward API and provides
    // a stable, predictable identifier (e.g. "synctv-0", "synctv-abc123")
    if let Ok(pod_name) = std::env::var("POD_NAME") {
        if !pod_name.is_empty() {
            return pod_name;
        }
    }

    if let Ok(pod_ip) = std::env::var("POD_IP") {
        if !pod_ip.is_empty() {
            return pod_ip;
        }
    }

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    // Add random suffix for uniqueness
    let suffix = nanoid::nanoid!(6);

    format!("{hostname}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn with_node_env<T>(pod_name: Option<&str>, pod_ip: Option<&str>, test: impl FnOnce() -> T) -> T {
        let _guard = ENV_LOCK
            .lock()
            .expect("env lock should not be poisoned");
        let original_pod_name = std::env::var("POD_NAME").ok();
        let original_pod_ip = std::env::var("POD_IP").ok();

        match pod_name {
            Some(value) => std::env::set_var("POD_NAME", value),
            None => std::env::remove_var("POD_NAME"),
        }

        match pod_ip {
            Some(value) => std::env::set_var("POD_IP", value),
            None => std::env::remove_var("POD_IP"),
        }

        let result = test();

        match original_pod_name {
            Some(value) => std::env::set_var("POD_NAME", value),
            None => std::env::remove_var("POD_NAME"),
        }

        match original_pod_ip {
            Some(value) => std::env::set_var("POD_IP", value),
            None => std::env::remove_var("POD_IP"),
        }

        result
    }

    #[test]
    fn test_generate_node_id_format() {
        with_node_env(None, None, || {
            let node_id = generate_node_id();
            assert!(
                node_id.contains('-'),
                "node_id should contain hyphen: {node_id}"
            );
        });
    }

    #[test]
    fn test_generate_node_id_with_pod_name() {
        with_node_env(Some("test-pod-123"), None, || {
            let node_id = generate_node_id();
            assert_eq!(node_id, "test-pod-123", "Should use POD_NAME when set");
        });
    }

    #[test]
    fn test_generate_node_id_uses_pod_ip_before_hostname() {
        with_node_env(Some(""), Some("10.2.3.4"), || {
            let node_id = generate_node_id();
            assert_eq!(node_id, "10.2.3.4");
        });
    }

    #[test]
    fn test_generate_node_id_empty_pod_name_falls_back_to_hostname() {
        with_node_env(Some(""), None, || {
            let node_id = generate_node_id();
            assert!(!node_id.is_empty(), "node_id should not be empty");
            assert!(node_id.contains('-'), "Should use hostname-based format");
            assert!(!node_id.contains('_'), "Should not include probed IP format");
        });
    }

    #[test]
    fn test_generate_node_id_unique() {
        with_node_env(None, None, || {
            let id1 = generate_node_id();
            let id2 = generate_node_id();
            assert_ne!(id1, id2, "Each call should generate a unique ID");
        });
    }
}
