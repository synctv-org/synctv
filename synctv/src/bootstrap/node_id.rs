/// Generate a unique node ID for this server instance.
///
/// Prefers the `POD_NAME` environment variable (set by Kubernetes downward API)
/// for predictable, consistent node IDs in K8s deployments.
/// Falls back to `POD_IP`, then hostname, then `unknown` for non-K8s environments.
/// This function intentionally avoids outbound network probing during startup.
pub fn generate_node_id() -> String {
    generate_node_id_with(&|name| std::env::var(name).ok())
}

fn generate_node_id_with(get_env: &impl Fn(&str) -> Option<String>) -> String {
    // In Kubernetes, POD_NAME is injected via the downward API and provides
    // a stable, predictable identifier (e.g. "synctv-0", "synctv-abc123")
    if let Some(pod_name) = get_env("POD_NAME").filter(|value| !value.is_empty()) {
        return pod_name;
    }

    if let Some(pod_ip) = get_env("POD_IP").filter(|value| !value.is_empty()) {
        return pod_ip;
    }

    let hostname = hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .filter(|hostname| !hostname.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    // Add random suffix for uniqueness
    let suffix = synctv_common::snanoid!(6);

    format!("{hostname}-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn with_node_env<T>(
        pod_name: Option<&str>,
        pod_ip: Option<&str>,
        test: impl FnOnce(&HashMap<String, String>) -> T,
    ) -> T {
        let mut env = HashMap::new();
        if let Some(value) = pod_name {
            env.insert("POD_NAME".to_string(), value.to_string());
        }
        if let Some(value) = pod_ip {
            env.insert("POD_IP".to_string(), value.to_string());
        }
        test(&env)
    }

    #[test]
    fn test_generate_node_id_uses_pod_ip_before_hostname() {
        with_node_env(Some(""), Some("10.2.3.4"), |env| {
            let node_id = generate_node_id_with(&|name| env.get(name).cloned());
            assert_eq!(node_id, "10.2.3.4");
        });
    }

    #[test]
    fn test_generate_node_id_empty_pod_name_falls_back_to_hostname() {
        with_node_env(Some(""), None, |env| {
            let node_id = generate_node_id_with(&|name| env.get(name).cloned());
            assert!(!node_id.is_empty(), "node_id should not be empty");
            assert!(node_id.contains('-'), "Should use hostname-based format");
            assert_ne!(node_id, "unknown", "Should include uniqueness suffix");
        });
    }
}
