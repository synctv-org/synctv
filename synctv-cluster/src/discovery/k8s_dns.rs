//! Kubernetes DNS-based cluster node discovery
//!
//! Discovers cluster peers by resolving A records from a Kubernetes headless service.
//! Pattern: `{service-name}.{namespace}.svc.cluster.local`
//!
//! Each resolved IP corresponds to a pod backing the headless service.
//! Combined with the cluster listener port, this yields a routable internal
//! gRPC address for each peer.
//!
//! **Important**: DNS discovery supplements but does not replace Redis. Full cluster
//! functionality (health monitoring, load balancing, pub/sub) still requires Redis.
//! DNS provides faster detection of newly-scaled pods; Redis provides the
//! NodeRegistry, HealthMonitor, and LoadBalancer infrastructure.

use futures::{stream, StreamExt as _};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;

use super::node_registry::NodeInfo;
use super::probe_node_identity;
use super::runtime::ClusterNodeDirectory;
use crate::error::{Error, Result};

const DNS_PROBE_CONCURRENCY: usize = 16;

/// Discovered peer from DNS resolution
#[derive(Debug, Clone)]
pub struct DnsPeer {
    /// IP address resolved from DNS
    pub ip: String,
    /// Internal cluster gRPC address (`ip:cluster_port`).
    pub cluster_address: String,
}

#[derive(Debug, Clone)]
pub struct K8sDnsDiscoveryOptions {
    pub service_name: String,
    pub namespace: String,
    pub self_ip: String,
    pub cluster_port: u16,
}

/// Kubernetes DNS-based discovery for cluster peers.
///
/// Resolves the headless service DNS name to discover peer pod IPs,
/// then constructs internal gRPC addresses using the cluster port.
///
/// **Redis is required for shared topology.** DNS discovery only handles peer IP
/// resolution. Registry-backed health monitoring, leader election, and connection
/// load balancing depend on Redis. Without Redis, nodes discovered via DNS cannot
/// participate in shared cluster coordination.
/// Configure `REDIS_URL` alongside the K8s DNS environment variables.
#[derive(Clone)]
pub struct K8sDnsDiscovery {
    /// Headless service DNS name (e.g., "synctv-headless.default.svc.cluster.local")
    dns_name: String,
    /// Internal gRPC port used by all peers.
    cluster_port: u16,
    /// This node's pod IP (to exclude self from peer list)
    self_ip: String,
    /// Cached list of discovered peers
    peers: Arc<RwLock<Vec<DnsPeer>>>,
    /// Optional reference to NodeRegistry for syncing discovered peers into the
    /// local transient node view. This supplements Redis-backed membership with
    /// readiness-verified DNS peers before they self-register.
    node_registry: Option<Arc<dyn ClusterNodeDirectory>>,
    /// Tracks the probed node_id for each peer IP so DNS disappearance can
    /// remove only the corresponding transient local entry.
    peer_node_ids: Arc<RwLock<HashMap<String, String>>>,
    /// Optional cluster secret used to authenticate gRPC identity probes.
    cluster_secret: String,
}

impl K8sDnsDiscovery {
    /// Create a K8s DNS discovery instance from structured startup options.
    pub fn from_options(options: &K8sDnsDiscoveryOptions) -> Result<Self> {
        let service_name = options.service_name.trim();
        if service_name.is_empty() {
            return Err(Error::Configuration(
                "HEADLESS_SERVICE_NAME must not be empty".to_string(),
            ));
        }

        let namespace = options.namespace.trim();
        if namespace.is_empty() {
            return Err(Error::Configuration(
                "POD_NAMESPACE must not be empty".to_string(),
            ));
        }

        let self_ip = options.self_ip.trim();
        if self_ip.is_empty() {
            return Err(Error::Configuration("POD_IP must not be empty".to_string()));
        }

        let dns_name = format!("{service_name}.{namespace}.svc.cluster.local");

        Ok(Self {
            dns_name,
            cluster_port: options.cluster_port,
            self_ip: self_ip.to_string(),
            peers: Arc::new(RwLock::new(Vec::new())),
            node_registry: None,
            peer_node_ids: Arc::new(RwLock::new(HashMap::new())),
            cluster_secret: String::new(),
        })
    }

    /// Create with explicit parameters (for testing or non-standard setups).
    pub fn new(dns_name: String, cluster_port: u16, self_ip: String) -> Self {
        Self {
            dns_name,
            cluster_port,
            self_ip,
            peers: Arc::new(RwLock::new(Vec::new())),
            node_registry: None,
            peer_node_ids: Arc::new(RwLock::new(HashMap::new())),
            cluster_secret: String::new(),
        }
    }

    /// Attach a node directory so readiness-verified DNS peers are merged into
    /// the local transient node view used by health monitoring and routing.
    #[must_use]
    pub fn with_node_directory(mut self, registry: Arc<dyn ClusterNodeDirectory>) -> Self {
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

        for (_, info) in verified_peers {
            registry.upsert_discovered_local_node(info).await;
        }

        for node_id in old_mapping
            .values()
            .filter(|node_id| !new_node_ids.contains(*node_id))
        {
            if !registry.remove_discovered_local_node(node_id).await {
                tracing::debug!(
                    node_id,
                    "stale K8s DNS discovered node was already absent from local registry"
                );
            }
        }

        *self.peer_node_ids.write().await = new_mapping;
    }

    /// Perform a single DNS resolution and return discovered peers.
    pub async fn resolve_once(&self) -> Result<Vec<DnsPeer>> {
        let lookup_addr = format!("{}:{}", self.dns_name, self.cluster_port);

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
                format!("[{}]:{}", ip, self.cluster_port)
            } else {
                format!("{}:{}", ip, self.cluster_port)
            };

            peers.push(DnsPeer {
                ip: ip.clone(),
                cluster_address: shared_address,
            });
        }

        Ok(peers)
    }

    /// Resolve peers and update the internal cache.
    ///
    /// When a node directory is attached via [`with_node_directory`](Self::with_node_directory), this also:
    /// - Probes newly discovered peers to confirm gRPC readiness and real node identity
    /// - Merges verified peers into the registry's transient local node view
    /// - Removes disappeared transient DNS-only entries without touching Redis membership
    pub async fn refresh(&self) -> Result<()> {
        match self.resolve_once().await {
            Ok(new_peers) => {
                let count = new_peers.len();

                if self.node_registry.is_some() {
                    let peers = &new_peers;
                    let probe_results = stream::iter(0..new_peers.len())
                        .map(|index| async move {
                            let peer = &peers[index];
                            let identity =
                                probe_node_identity(&peer.cluster_address, 3, &self.cluster_secret)
                                    .await;
                            (peer, identity)
                        })
                        .buffered(DNS_PROBE_CONCURRENCY)
                        .collect::<Vec<_>>()
                        .await;

                    let mut verified_peers = Vec::new();
                    for (peer, identity) in probe_results {
                        if let Some(identity) = identity {
                            let info =
                                NodeInfo::new(identity.node_id, peer.cluster_address.clone())
                                    .with_epoch(identity.epoch);
                            verified_peers.push((peer.ip.clone(), info));
                        } else {
                            tracing::debug!(
                                peer_ip = %peer.ip,
                                cluster_address = %peer.cluster_address,
                                "Skipping DNS peer until gRPC identity probe succeeds"
                            );
                        }
                    }
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
                        if let Err(error) = this.refresh().await {
                            tracing::warn!(
                                error = %error,
                                dns_name = %this.dns_name,
                                "K8s DNS discovery refresh failed"
                            );
                        }
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

    fn make_registry() -> crate::Result<Arc<NodeRegistry>> {
        Ok(Arc::new(NodeRegistry::new_local_only(
            "self".to_string(),
            30,
            "k8s-dns-test:",
        )?))
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
    fn test_from_options_requires_pod_ip() -> crate::Result<()> {
        let options = K8sDnsDiscoveryOptions {
            service_name: "synctv-headless".to_string(),
            namespace: "default".to_string(),
            self_ip: String::new(),
            cluster_port: 50051,
        };
        let result = K8sDnsDiscovery::from_options(&options);

        let Err(err) = result else {
            return Err(crate::Error::Configuration(
                "missing POD_IP must fail closed".to_string(),
            ));
        };
        assert!(
            err.to_string().contains("POD_IP"),
            "configuration error should explicitly mention POD_IP: {err}"
        );
        Ok(())
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
    async fn test_refresh_loop_stops_when_parent_shutdown_token_is_cancelled(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        );
        let shutdown_token = CancellationToken::new();

        let handle = disc.start_refresh_loop(60, shutdown_token.clone());

        shutdown_token.cancel();

        tokio::time::timeout(Duration::from_secs(1), handle).await??;
        Ok(())
    }

    #[tokio::test]
    async fn test_refresh_loop_waits_for_first_interval_before_refreshing(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let disc = K8sDnsDiscovery::new("localhost".to_string(), 8080, "10.0.0.1".to_string());
        let shutdown_token = CancellationToken::new();

        let handle = disc.start_refresh_loop(60, shutdown_token.clone());

        tokio::task::yield_now().await;

        assert!(
            disc.get_peers().await.is_empty(),
            "refresh loop must not perform an immediate DNS refresh before the first interval elapses"
        );

        shutdown_token.cancel();
        handle.await?;
        Ok(())
    }

    #[tokio::test]
    async fn test_sync_verified_peers_uses_probed_node_id_in_registry_local_cache(
    ) -> crate::Result<()> {
        let registry = make_registry()?;
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_directory(registry.clone());

        let node_info =
            NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:50051".to_string()).with_epoch(7);

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), node_info)])
            .await;

        assert!(
            registry.get_node_local("peer-node-1").await.is_some(),
            "verified DNS peers must be keyed by probed node_id"
        );
        assert!(
            registry.get_node_local("10.0.0.2").await.is_none(),
            "DNS IP must not be used as authoritative node_id"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_sync_verified_peers_removes_disappeared_dns_local_entry() -> crate::Result<()> {
        let registry = make_registry()?;
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_directory(registry.clone());

        let node_info = NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:50051".to_string());

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), node_info)])
            .await;
        assert!(registry.get_node_local("peer-node-1").await.is_some());

        disc.sync_verified_peers_to_registry(Vec::new()).await;
        assert!(
            registry.get_node_local("peer-node-1").await.is_none(),
            "vanished DNS peers should be removed from transient local cache"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_sync_verified_peers_does_not_remove_real_node_entry_after_dns_disappears(
    ) -> crate::Result<()> {
        let registry = make_registry()?;
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_directory(registry.clone());

        let dns_node = NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:50051".to_string());

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), dns_node)])
            .await;

        registry
            .test_insert_local(
                NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:50051".to_string())
                    .with_epoch(9),
            )
            .await;

        disc.sync_verified_peers_to_registry(Vec::new()).await;
        assert!(
            registry.get_node_local("peer-node-1").await.is_some(),
            "DNS disappearance must not evict a real node entry that replaced the transient DNS one"
        );
        Ok(())
    }

    #[tokio::test]
    async fn test_sync_verified_peers_refreshes_transient_entry_when_address_changes(
    ) -> crate::Result<()> {
        let registry = make_registry()?;
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            8080,
            "10.0.0.1".to_string(),
        )
        .with_node_directory(registry.clone());

        let original =
            NodeInfo::new("peer-node-1".to_string(), "10.0.0.2:50051".to_string()).with_epoch(7);

        disc.sync_verified_peers_to_registry(vec![("10.0.0.2".to_string(), original)])
            .await;

        let restarted =
            NodeInfo::new("peer-node-1".to_string(), "10.0.0.9:50051".to_string()).with_epoch(8);

        disc.sync_verified_peers_to_registry(vec![("10.0.0.9".to_string(), restarted)])
            .await;

        let node = registry
            .get_node_local("peer-node-1")
            .await
            .ok_or_else(|| {
                crate::Error::NotFound("transient DNS peer should still exist".to_string())
            })?;
        assert_eq!(node.cluster_address, "10.0.0.9:50051");
        assert_eq!(node.epoch, 8);
        Ok(())
    }
}
