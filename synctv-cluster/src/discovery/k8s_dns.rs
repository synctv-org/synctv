//! Kubernetes DNS-based cluster node discovery
//!
//! Discovers cluster peers by resolving A records from a Kubernetes headless service.
//! Pattern: `{service-name}.{namespace}.svc.cluster.local`
//!
//! Each resolved IP corresponds to a pod backing the headless service.
//! Combined with a known unified API port, this yields a routable peer API
//! address for both gRPC and HTTP traffic.
//!
//! **Important**: DNS discovery supplements but does not replace Redis. Full cluster
//! functionality (health monitoring, load balancing, pub/sub) still requires Redis.
//! DNS provides faster detection of newly-scaled pods; Redis provides the
//! NodeRegistry, HealthMonitor, and LoadBalancer infrastructure.

use futures::future::join_all;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;

use super::node_registry::{NodeInfo, NodeRegistry};
use super::probe_node_identity;
use crate::error::{Error, Result};

/// Discovered peer from DNS resolution
#[derive(Debug, Clone)]
pub struct DnsPeer {
    /// IP address resolved from DNS
    pub ip: String,
    /// Shared API address (ip:api_port)
    pub api_address: String,
}

/// Kubernetes DNS-based discovery for cluster peers.
///
/// Resolves the headless service DNS name to discover peer pod IPs,
/// then constructs shared API addresses using the shared API port.
///
/// **Redis is still required.** DNS discovery only handles peer IP resolution.
/// All cluster functionality -- pub/sub event synchronization, health monitoring,
/// leader election, stream-based catch-up, and connection load balancing -- depends
/// on Redis. Without Redis, nodes discovered via DNS cannot communicate or coordinate.
/// Configure `REDIS_URL` alongside the K8s DNS environment variables.
#[derive(Clone)]
pub struct K8sDnsDiscovery {
    /// Headless service DNS name (e.g., "synctv-headless.default.svc.cluster.local")
    dns_name: String,
    /// Shared API port used by all peers for both gRPC and HTTP.
    api_port: u16,
    /// This node's pod IP (to exclude self from peer list)
    self_ip: String,
    /// Cached list of discovered peers
    peers: Arc<RwLock<Vec<DnsPeer>>>,
    /// Optional reference to NodeRegistry for syncing discovered peers into the
    /// local transient node view. This supplements Redis-backed membership with
    /// readiness-verified DNS peers before they self-register.
    node_registry: Option<Arc<NodeRegistry>>,
    /// Tracks the probed node_id for each peer IP so DNS disappearance can
    /// remove only the corresponding transient local entry.
    peer_node_ids: Arc<RwLock<HashMap<String, String>>>,
    /// Optional cluster secret used to authenticate gRPC identity probes.
    cluster_secret: String,
}

impl K8sDnsDiscovery {
    /// Create a new K8s DNS discovery instance from environment variables.
    ///
    /// Required env vars:
    /// - `HEADLESS_SERVICE_NAME`: name of the K8s headless service
    /// - `POD_NAMESPACE`: namespace of the pod (from downward API)
    /// - `POD_IP`: this pod's IP address (from downward API)
    ///
    /// **Also required**: `REDIS_URL` must be set separately. DNS discovery only
    /// resolves peer IPs; Redis is required for pub/sub, health checks, leader
    /// election, and all other cluster coordination.
    ///
    /// The shared API port is read from config (`server.port`).
    pub fn from_env(api_port: u16) -> Result<Self> {
        let service_name = std::env::var("HEADLESS_SERVICE_NAME").map_err(|_| {
            Error::Configuration(
                "HEADLESS_SERVICE_NAME env var is required for k8s_dns discovery mode".to_string(),
            )
        })?;
        if service_name.is_empty() {
            return Err(Error::Configuration(
                "HEADLESS_SERVICE_NAME must not be empty".to_string(),
            ));
        }

        let namespace = std::env::var("POD_NAMESPACE").map_err(|_| {
            Error::Configuration(
                "POD_NAMESPACE env var is required for k8s_dns discovery mode".to_string(),
            )
        })?;
        if namespace.is_empty() {
            return Err(Error::Configuration(
                "POD_NAMESPACE must not be empty".to_string(),
            ));
        }

        let self_ip = std::env::var("POD_IP").map_err(|_| {
            Error::Configuration(
                "POD_IP env var is required for k8s_dns discovery mode".to_string(),
            )
        })?;
        if self_ip.is_empty() {
            return Err(Error::Configuration("POD_IP must not be empty".to_string()));
        }

        let dns_name = format!("{service_name}.{namespace}.svc.cluster.local");

        Ok(Self {
            dns_name,
            api_port,
            self_ip,
            peers: Arc::new(RwLock::new(Vec::new())),
            node_registry: None,
            peer_node_ids: Arc::new(RwLock::new(HashMap::new())),
            cluster_secret: String::new(),
        })
    }

    /// Create with explicit parameters (for testing or non-standard setups).
    pub fn new(dns_name: String, api_port: u16, self_ip: String) -> Self {
        Self {
            dns_name,
            api_port,
            self_ip,
            peers: Arc::new(RwLock::new(Vec::new())),
            node_registry: None,
            peer_node_ids: Arc::new(RwLock::new(HashMap::new())),
            cluster_secret: String::new(),
        }
    }

    /// Attach a `NodeRegistry` so that readiness-verified DNS peers are merged
    /// into the local transient node view used by health monitoring and routing.
    #[must_use]
    pub fn with_node_registry(mut self, registry: Arc<NodeRegistry>) -> Self {
        self.node_registry = Some(registry);
        self
    }

    /// Configure the cluster secret used for authenticated peer identity probes.
    #[must_use]
    pub fn with_cluster_secret(mut self, cluster_secret: String) -> Self {
        self.cluster_secret = cluster_secret;
        self
    }

    async fn sync_verified_peers_to_registry(&self, verified_peers: Vec<(String, NodeInfo)>) {
        let Some(registry) = &self.node_registry else {
            return;
        };

        let old_mapping = self.peer_node_ids.read().await.clone();
        let new_mapping: HashMap<String, String> = verified_peers
            .iter()
            .map(|(ip, info)| (ip.clone(), info.node_id.clone()))
            .collect();
        let new_node_ids: std::collections::HashSet<String> = verified_peers
            .iter()
            .map(|(_, info)| info.node_id.clone())
            .collect();

        {
            let mut nodes = registry.local_nodes.write().await;
            for (_, info) in verified_peers {
                match nodes.get_mut(&info.node_id) {
                    Some(existing)
                        if existing
                            .metadata
                            .get("discovery")
                            .is_some_and(|value| value == "k8s_dns") =>
                    {
                        *existing = info;
                    }
                    Some(_) => {}
                    None => {
                        nodes.insert(info.node_id.clone(), info);
                    }
                }
            }
        }

        for node_id in old_mapping
            .values()
            .filter(|node_id| !new_node_ids.contains(*node_id))
        {
            let _ = registry
                .remove_discovered_local_node(node_id, "k8s_dns")
                .await;
        }

        *self.peer_node_ids.write().await = new_mapping;
    }

    /// Perform a single DNS resolution and return discovered peers.
    pub async fn resolve_once(&self) -> Result<Vec<DnsPeer>> {
        let lookup_addr = format!("{}:{}", self.dns_name, self.api_port);

        let addrs = tokio::net::lookup_host(&lookup_addr).await.map_err(|e| {
            Error::Configuration(format!("DNS lookup failed for '{}': {}", self.dns_name, e))
        })?;

        let mut peers = Vec::new();
        let mut seen_ips = std::collections::HashSet::new();

        for addr in addrs {
            let ip = addr.ip().to_string();

            // Skip self
            if ip == self.self_ip {
                continue;
            }

            // Deduplicate (DNS may return same IP multiple times)
            if !seen_ips.insert(ip.clone()) {
                continue;
            }

            // Wrap IPv6 addresses in brackets so they form valid socket addresses
            let shared_address = if addr.ip().is_ipv6() {
                format!("[{}]:{}", ip, self.api_port)
            } else {
                format!("{}:{}", ip, self.api_port)
            };

            peers.push(DnsPeer {
                ip: ip.clone(),
                api_address: shared_address,
            });
        }

        Ok(peers)
    }

    /// Resolve peers and update the internal cache.
    ///
    /// When a `NodeRegistry` is attached (via [`with_node_registry`]), this also:
    /// - Probes newly discovered peers to confirm gRPC readiness and real node identity
    /// - Merges verified peers into the registry's transient local node view
    /// - Removes disappeared transient DNS-only entries without touching Redis membership
    pub async fn refresh(&self) -> Result<()> {
        match self.resolve_once().await {
            Ok(new_peers) => {
                let count = new_peers.len();

                if let Some(ref registry) = self.node_registry {
                    let probe_results = join_all(new_peers.iter().map(|peer| async move {
                        let identity =
                            probe_node_identity(&peer.api_address, 3, &self.cluster_secret).await;
                        (peer, identity)
                    }))
                    .await;

                    let mut verified_peers = Vec::new();
                    for (peer, identity) in probe_results {
                        if let Some(identity) = identity {
                            let mut info =
                                NodeInfo::new(identity.node_id, peer.api_address.clone())
                                    .with_epoch(identity.epoch);
                            info.metadata
                                .insert("discovery".to_string(), "k8s_dns".to_string());
                            verified_peers.push((peer.ip.clone(), info));
                        } else {
                            tracing::debug!(
                                peer_ip = %peer.ip,
                                api_address = %peer.api_address,
                                "Skipping DNS peer until gRPC identity probe succeeds"
                            );
                        }
                    }

                    let _ = registry;
                    self.sync_verified_peers_to_registry(verified_peers).await;
                }

                let mut cached = self.peers.write().await;
                *cached = new_peers;
                tracing::debug!(
                    dns_name = %self.dns_name,
                    peer_count = count,
                    "K8s DNS discovery refreshed"
                );
                Ok(())
            }
            Err(e) => {
                tracing::warn!(
                    dns_name = %self.dns_name,
                    error = %e,
                    "K8s DNS discovery refresh failed, keeping cached peers"
                );
                Err(e)
            }
        }
    }

    /// Get the current cached list of discovered peers.
    pub async fn get_peers(&self) -> Vec<DnsPeer> {
        self.peers.read().await.clone()
    }

    /// Start a background loop that periodically re-resolves DNS to track
    /// scaling events (pod additions/removals).
    ///
    /// Returns the real `JoinHandle` of the spawned refresh task.
    ///
    /// Shutdown ownership stays with the caller: pass in the parent
    /// cancellation token that governs application shutdown, and await or
    /// abort the returned task via the caller's own shutdown coordinator.
    pub fn start_refresh_loop(
        &self,
        interval_secs: u64,
        shutdown_token: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let this = self.clone();
        tokio::spawn(async move {
            let mut timer = interval(Duration::from_secs(interval_secs));
            timer.tick().await;

            loop {
                tokio::select! {
                    () = shutdown_token.cancelled() => {
                        tracing::info!("K8s DNS discovery refresh loop shutting down");
                        return;
                    }
                    _ = timer.tick() => {
                        let _ = this.refresh().await;
                    }
                }
            }
        })
    }

    /// Get the DNS name being resolved.
    #[must_use]
    pub fn dns_name(&self) -> &str {
        &self.dns_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::NodeRegistry;
    use std::sync::Arc;

    #[test]
    fn test_k8s_dns_discovery_new() {
        let disc = K8sDnsDiscovery::new(
            "synctv-headless.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        );
        assert_eq!(disc.dns_name(), "synctv-headless.default.svc.cluster.local");
        assert_eq!(disc.api_port, 8080);
        assert_eq!(disc.self_ip, "10.0.0.1");
    }

    #[test]
    fn test_with_cluster_secret_sets_probe_secret() {
        let disc = K8sDnsDiscovery::new(
            "synctv-headless.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_cluster_secret("shared-secret".to_string());

        assert_eq!(disc.cluster_secret, "shared-secret");
    }

    #[test]
    fn test_from_env_requires_pod_ip() {
        let headless_prev = std::env::var("HEADLESS_SERVICE_NAME").ok();
        let namespace_prev = std::env::var("POD_NAMESPACE").ok();
        let pod_ip_prev = std::env::var("POD_IP").ok();

        std::env::set_var("HEADLESS_SERVICE_NAME", "synctv-headless");
        std::env::set_var("POD_NAMESPACE", "default");
        std::env::remove_var("POD_IP");

        let result = K8sDnsDiscovery::from_env(8080);

        match headless_prev {
            Some(value) => std::env::set_var("HEADLESS_SERVICE_NAME", value),
            None => std::env::remove_var("HEADLESS_SERVICE_NAME"),
        }
        match namespace_prev {
            Some(value) => std::env::set_var("POD_NAMESPACE", value),
            None => std::env::remove_var("POD_NAMESPACE"),
        }
        match pod_ip_prev {
            Some(value) => std::env::set_var("POD_IP", value),
            None => std::env::remove_var("POD_IP"),
        }

        let Err(err) = result else {
            panic!("missing POD_IP must fail closed");
        };
        assert!(
            err.to_string().contains("POD_IP"),
            "configuration error should explicitly mention POD_IP: {err}"
        );
    }

    #[tokio::test]
    async fn test_get_peers_empty_initially() {
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        );
        let peers = disc.get_peers().await;
        assert!(peers.is_empty());
    }

    #[tokio::test]
    async fn test_refresh_loop_stops_when_parent_shutdown_token_is_cancelled() {
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        );
        let shutdown_token = CancellationToken::new();

        let handle = disc.start_refresh_loop(60, shutdown_token.clone());

        shutdown_token.cancel();

        tokio::time::timeout(Duration::from_secs(1), handle)
            .await
            .expect("refresh loop should stop promptly when parent shutdown token is cancelled")
            .expect("refresh loop should exit cleanly");
    }

    #[tokio::test]
    async fn test_refresh_loop_waits_for_first_interval_before_refreshing() {
        let disc = K8sDnsDiscovery::new("localhost".to_string(), 8080, "10.0.0.1".to_string());
        let shutdown_token = CancellationToken::new();

        let handle = disc.start_refresh_loop(60, shutdown_token.clone());

        tokio::task::yield_now().await;

        assert!(
            disc.get_peers().await.is_empty(),
            "refresh loop must not perform an immediate DNS refresh before the first interval elapses"
        );

        shutdown_token.cancel();
        handle
            .await
            .expect("refresh loop should exit cleanly after cancellation");
    }

    #[tokio::test]
    async fn test_sync_verified_peers_uses_probed_node_id_in_registry_local_cache() {
        let registry = Arc::new(
            NodeRegistry::new_local_only("self".to_string(), 30, "k8s-dns-test:")
                .expect("local-only registry"),
        );
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_registry(registry.clone());

        let mut node_info =
            NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:8080".to_string()).with_epoch(7);
        node_info
            .metadata
            .insert("discovery".to_string(), "k8s_dns".to_string());

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), node_info)])
            .await;

        assert!(
            registry.test_get_local("peer-node-1").await.is_some(),
            "verified DNS peers must be keyed by probed node_id"
        );
        assert!(
            registry.test_get_local("10.0.0.2").await.is_none(),
            "DNS IP must not be used as authoritative node_id"
        );
    }

    #[tokio::test]
    async fn test_sync_verified_peers_removes_disappeared_dns_local_entry() {
        let registry = Arc::new(
            NodeRegistry::new_local_only("self".to_string(), 30, "k8s-dns-test:")
                .expect("local-only registry"),
        );
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_registry(registry.clone());

        let mut node_info = NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:8080".to_string());
        node_info
            .metadata
            .insert("discovery".to_string(), "k8s_dns".to_string());

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), node_info)])
            .await;
        assert!(registry.test_get_local("peer-node-1").await.is_some());

        disc.sync_verified_peers_to_registry(Vec::new()).await;
        assert!(
            registry.test_get_local("peer-node-1").await.is_none(),
            "vanished DNS peers should be removed from transient local cache"
        );
    }

    #[tokio::test]
    async fn test_sync_verified_peers_does_not_remove_real_node_entry_after_dns_disappears() {
        let registry = Arc::new(
            NodeRegistry::new_local_only("self".to_string(), 30, "k8s-dns-test:")
                .expect("local-only registry"),
        );
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_registry(registry.clone());

        let mut dns_node = NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:8080".to_string());
        dns_node
            .metadata
            .insert("discovery".to_string(), "k8s_dns".to_string());

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), dns_node)])
            .await;

        registry
            .test_insert_local(
                NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:8080".to_string()).with_epoch(9),
            )
            .await;

        disc.sync_verified_peers_to_registry(Vec::new()).await;
        assert!(
            registry.test_get_local("peer-node-1").await.is_some(),
            "DNS disappearance must not evict a real node entry that replaced the transient DNS one"
        );
    }

    #[tokio::test]
    async fn test_sync_verified_peers_refreshes_transient_entry_when_address_changes() {
        let registry = Arc::new(
            NodeRegistry::new_local_only("self".to_string(), 30, "k8s-dns-test:")
                .expect("local-only registry"),
        );
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_registry(registry.clone());

        let mut original =
            NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:8080".to_string()).with_epoch(7);
        original
            .metadata
            .insert("discovery".to_string(), "k8s_dns".to_string());

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), original)])
            .await;

        let mut restarted =
            NodeInfo::new("peer-node-1".to_string(), "10.0.0.9:8080".to_string()).with_epoch(8);
        restarted
            .metadata
            .insert("discovery".to_string(), "k8s_dns".to_string());

        disc.sync_verified_peers_to_registry(vec![("10.0.0.9".to_string(), restarted)])
            .await;

        let node = registry
            .test_get_local("peer-node-1")
            .await
            .expect("transient DNS peer should still exist");
        assert_eq!(node.api_address, "10.0.0.9:8080");
        assert_eq!(node.epoch, 8);
    }
}
