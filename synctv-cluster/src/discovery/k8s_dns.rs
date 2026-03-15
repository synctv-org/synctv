//! Kubernetes DNS-based cluster node discovery
//!
//! Discovers cluster peers by resolving A records from a Kubernetes headless service.
//! Pattern: `{service-name}.{namespace}.svc.cluster.local`
//!
//! Each resolved IP corresponds to a pod backing the headless service.
//! Combined with a known gRPC/HTTP port, this yields routable peer addresses.
//!
//! **Important**: DNS discovery supplements but does not replace Redis. Full cluster
//! functionality (health monitoring, load balancing, pub/sub) still requires Redis.
//! DNS provides faster detection of newly-scaled pods; Redis provides the
//! NodeRegistry, HealthMonitor, and LoadBalancer infrastructure.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{interval, Duration};
use tokio_util::sync::CancellationToken;

use super::node_registry::{NodeInfo, NodeRegistry};
use crate::error::{Error, Result};

/// Discovered peer from DNS resolution
#[derive(Debug, Clone)]
pub struct DnsPeer {
    /// IP address resolved from DNS
    pub ip: String,
    /// gRPC address (ip:grpc_port)
    pub grpc_address: String,
    /// HTTP address (ip:http_port)
    pub http_address: String,
}

/// Kubernetes DNS-based discovery for cluster peers.
///
/// Resolves the headless service DNS name to discover peer pod IPs,
/// then constructs gRPC/HTTP addresses using configured ports.
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
    /// gRPC port used by all peers
    grpc_port: u16,
    /// HTTP port used by all peers
    http_port: u16,
    /// This node's pod IP (to exclude self from peer list)
    self_ip: String,
    /// Cached list of discovered peers
    peers: Arc<RwLock<Vec<DnsPeer>>>,
    /// Optional reference to NodeRegistry for syncing discovered peers.
    /// When set, `refresh()` will register new peers and unregister
    /// disappeared peers via `NodeRegistry::register_remote()` /
    /// `NodeRegistry::unregister_remote()`.
    node_registry: Option<Arc<NodeRegistry>>,
    /// Tracks the registration epoch for each peer IP so that
    /// `unregister_remote` can pass the correct epoch for validation.
    peer_epochs: Arc<RwLock<HashMap<String, u64>>>,
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
    /// Ports are read from config (grpc_port, http_port).
    pub fn from_env(grpc_port: u16, http_port: u16) -> Result<Self> {
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
            grpc_port,
            http_port,
            self_ip,
            peers: Arc::new(RwLock::new(Vec::new())),
            node_registry: None,
            peer_epochs: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Create with explicit parameters (for testing or non-standard setups).
    pub fn new(dns_name: String, grpc_port: u16, http_port: u16, self_ip: String) -> Self {
        Self {
            dns_name,
            grpc_port,
            http_port,
            self_ip,
            peers: Arc::new(RwLock::new(Vec::new())),
            node_registry: None,
            peer_epochs: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Attach a `NodeRegistry` so that DNS-discovered peers are automatically
    /// registered/unregistered in Redis. Without this, DNS discovery only
    /// populates the local peer cache but not the shared NodeRegistry.
    pub fn with_node_registry(mut self, registry: Arc<NodeRegistry>) -> Self {
        self.node_registry = Some(registry);
        self
    }

    /// Perform a single DNS resolution and return discovered peers.
    pub async fn resolve_once(&self) -> Result<Vec<DnsPeer>> {
        let lookup_addr = format!("{}:{}", self.dns_name, self.grpc_port);

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
            let (grpc_address, http_address) = if addr.ip().is_ipv6() {
                (
                    format!("[{}]:{}", ip, self.grpc_port),
                    format!("[{}]:{}", ip, self.http_port),
                )
            } else {
                (
                    format!("{}:{}", ip, self.grpc_port),
                    format!("{}:{}", ip, self.http_port),
                )
            };

            peers.push(DnsPeer {
                ip: ip.clone(),
                grpc_address,
                http_address,
            });
        }

        Ok(peers)
    }

    /// Resolve peers and update the internal cache.
    ///
    /// When a `NodeRegistry` is attached (via [`with_node_registry`]), this also:
    /// - Registers newly discovered peers via `NodeRegistry::register_remote()`
    /// - Unregisters peers that have disappeared from DNS via `NodeRegistry::unregister_remote()`
    pub async fn refresh(&self) -> Result<()> {
        match self.resolve_once().await {
            Ok(new_peers) => {
                let count = new_peers.len();

                // Compute diffs for NodeRegistry sync before updating cache
                if let Some(ref registry) = self.node_registry {
                    let old_peers = self.peers.read().await;
                    let old_ips: HashSet<&str> = old_peers.iter().map(|p| p.ip.as_str()).collect();
                    let new_ips: HashSet<&str> = new_peers.iter().map(|p| p.ip.as_str()).collect();

                    // Register new peers and track their epochs
                    for peer in &new_peers {
                        if !old_ips.contains(peer.ip.as_str()) {
                            let mut info = NodeInfo::new(
                                peer.ip.clone(),
                                peer.grpc_address.clone(),
                                peer.http_address.clone(),
                            );
                            info.metadata
                                .insert("discovery".to_string(), "k8s_dns".to_string());
                            let registration_epoch = info.epoch;
                            if let Err(e) = registry.register_remote(info).await {
                                tracing::warn!(
                                    peer_ip = %peer.ip,
                                    error = %e,
                                    "Failed to register DNS-discovered peer in NodeRegistry"
                                );
                            } else {
                                // Track the epoch used for registration
                                self.peer_epochs
                                    .write()
                                    .await
                                    .insert(peer.ip.clone(), registration_epoch);
                                tracing::info!(
                                    peer_ip = %peer.ip,
                                    grpc_address = %peer.grpc_address,
                                    epoch = registration_epoch,
                                    "DNS-discovered peer registered in NodeRegistry"
                                );
                            }
                        }
                    }

                    // Unregister disappeared peers with their tracked epoch
                    for peer in old_peers.iter() {
                        if !new_ips.contains(peer.ip.as_str()) {
                            let tracked_epoch =
                                self.peer_epochs.read().await.get(&peer.ip).copied();
                            if let Err(e) =
                                registry.unregister_remote(&peer.ip, tracked_epoch).await
                            {
                                tracing::warn!(
                                    peer_ip = %peer.ip,
                                    epoch = ?tracked_epoch,
                                    error = %e,
                                    "Failed to unregister disappeared DNS peer from NodeRegistry"
                                );
                            } else {
                                // Remove the tracked epoch for the departed peer
                                self.peer_epochs.write().await.remove(&peer.ip);
                                tracing::info!(
                                    peer_ip = %peer.ip,
                                    epoch = ?tracked_epoch,
                                    "Disappeared DNS peer unregistered from NodeRegistry"
                                );
                            }
                        }
                    }
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

    /// Convert discovered peers to `NodeInfo` structs for compatibility
    /// with the existing cluster infrastructure (health monitor, load balancer).
    pub async fn get_peers_as_node_info(&self) -> Vec<NodeInfo> {
        let peers = self.peers.read().await;
        peers
            .iter()
            .map(|peer| {
                let mut info = NodeInfo::new(
                    peer.ip.clone(),
                    peer.grpc_address.clone(),
                    peer.http_address.clone(),
                );
                info.metadata
                    .insert("discovery".to_string(), "k8s_dns".to_string());
                info
            })
            .collect()
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

    /// Build a `HashMap<String, NodeInfo>` keyed by node_id (IP) for
    /// compatibility with code that needs to look up peers by ID.
    pub async fn get_peer_map(&self) -> HashMap<String, NodeInfo> {
        let peers = self.get_peers_as_node_info().await;
        peers
            .into_iter()
            .map(|info| (info.node_id.clone(), info))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_k8s_dns_discovery_new() {
        let disc = K8sDnsDiscovery::new(
            "synctv-headless.default.svc.cluster.local".to_string(),
            50051,
            8080,
            "10.0.0.1".to_string(),
        );
        assert_eq!(disc.dns_name(), "synctv-headless.default.svc.cluster.local");
        assert_eq!(disc.grpc_port, 50051);
        assert_eq!(disc.http_port, 8080);
        assert_eq!(disc.self_ip, "10.0.0.1");
    }

    #[test]
    fn test_from_env_requires_pod_ip() {
        let headless_prev = std::env::var("HEADLESS_SERVICE_NAME").ok();
        let namespace_prev = std::env::var("POD_NAMESPACE").ok();
        let pod_ip_prev = std::env::var("POD_IP").ok();

        std::env::set_var("HEADLESS_SERVICE_NAME", "synctv-headless");
        std::env::set_var("POD_NAMESPACE", "default");
        std::env::remove_var("POD_IP");

        let result = K8sDnsDiscovery::from_env(50051, 8080);

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

        let err = match result {
            Ok(_) => panic!("missing POD_IP must fail closed"),
            Err(err) => err,
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
            50051,
            8080,
            "10.0.0.1".to_string(),
        );
        let peers = disc.get_peers().await;
        assert!(peers.is_empty());
    }

    #[tokio::test]
    async fn test_get_peers_as_node_info_empty() {
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            50051,
            8080,
            "10.0.0.1".to_string(),
        );
        let nodes = disc.get_peers_as_node_info().await;
        assert!(nodes.is_empty());
    }

    #[tokio::test]
    async fn test_get_peer_map_empty() {
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            50051,
            8080,
            "10.0.0.1".to_string(),
        );
        let map = disc.get_peer_map().await;
        assert!(map.is_empty());
    }

    #[tokio::test]
    async fn test_refresh_loop_stops_when_parent_shutdown_token_is_cancelled() {
        let disc = K8sDnsDiscovery::new(
            "test.default.svc.cluster.local".to_string(),
            50051,
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
}
