//! Static peer discovery for non-K8s environments
//!
//! Periodically health-checks a configured list of peer cluster addresses
//! and registers alive peers into the NodeRegistry. This enables cluster
//! formation without Kubernetes DNS or other dynamic discovery mechanisms.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use http::uri::Authority;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::discovery::node_registry::NodeInfo;
use crate::discovery::{ClusterNodeDirectory, ProbedNodeIdentity};

/// Number of consecutive probe failures before unregistering a peer.
const FAILURE_THRESHOLD: u32 = 3;

/// Normalize a configured static peer to an internal cluster listener address.
pub fn normalize_static_peer_address(
    peer_address: &str,
    default_cluster_port: u16,
) -> crate::Result<String> {
    let peer_address = peer_address.trim();
    if peer_address.is_empty() {
        return Err(crate::Error::Configuration(
            "static peer address must not be empty".to_string(),
        ));
    }

    if peer_address.parse::<std::net::Ipv6Addr>().is_ok() {
        return Ok(format!("[{peer_address}]:{default_cluster_port}"));
    }

    let authority = peer_address.parse::<Authority>().map_err(|error| {
        crate::Error::Configuration(format!(
            "invalid static peer address '{peer_address}': {error}"
        ))
    })?;
    if authority.host().is_empty() {
        return Err(crate::Error::Configuration(format!(
            "invalid static peer address '{peer_address}': host must not be empty"
        )));
    }

    let explicit_port = if peer_address.starts_with('[') {
        let closing_bracket = peer_address.find(']').ok_or_else(|| {
            crate::Error::Configuration(format!(
                "invalid static peer address '{peer_address}': missing closing IPv6 bracket"
            ))
        })?;
        let suffix = &peer_address[closing_bracket + 1..];
        if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or_else(|| {
                crate::Error::Configuration(format!(
                    "invalid static peer address '{peer_address}': expected ':' after IPv6 bracket"
                ))
            })?)
        }
    } else {
        peer_address.rsplit_once(':').map(|(_, port)| port)
    };

    if let Some(port) = explicit_port {
        if !matches!(port.parse::<u16>(), Ok(1..=u16::MAX)) {
            return Err(crate::Error::Configuration(format!(
                "invalid static peer address '{peer_address}': port must be between 1 and 65535"
            )));
        }
    }

    Ok(if explicit_port.is_some() {
        authority.to_string()
    } else {
        format!("{authority}:{default_cluster_port}")
    })
}

/// Configuration for a single static peer
#[derive(Debug, Clone)]
pub struct StaticPeerConfig {
    /// Internal cluster gRPC address (for example, `host1:50051`).
    pub cluster_address: String,
}

/// Configuration for static peer discovery
#[derive(Debug, Clone)]
pub struct StaticDiscoveryConfig {
    /// List of peer configurations
    pub peers: Vec<StaticPeerConfig>,
    /// How often to probe peers (seconds)
    pub probe_interval_secs: u64,
    /// Timeout for each gRPC connect attempt
    pub connect_timeout: Duration,
    /// Cluster secret for authenticated probing
    pub cluster_secret: String,
    /// Cluster port appended to a host-only peer entry.
    pub default_cluster_port: u16,
}

impl Default for StaticDiscoveryConfig {
    fn default() -> Self {
        Self {
            peers: Vec::new(),
            probe_interval_secs: 10,
            connect_timeout: Duration::from_secs(3),
            cluster_secret: String::new(),
            default_cluster_port: 50051,
        }
    }
}

/// Static peer discovery service
///
/// Periodically probes each configured peer address via gRPC `GetNodes`
/// and registers responsive peers into the NodeRegistry. Peers that
/// fail to respond are logged and skipped; they will be retried on
/// the next probe cycle.
pub struct StaticDiscovery {
    config: StaticDiscoveryConfig,
    node_registry: Arc<dyn ClusterNodeDirectory>,
    cancel_token: CancellationToken,
}

impl StaticDiscovery {
    /// Create a new StaticDiscovery
    pub fn new<N>(
        config: StaticDiscoveryConfig,
        node_registry: Arc<N>,
        cancel_token: CancellationToken,
    ) -> Self
    where
        N: ClusterNodeDirectory + 'static,
    {
        Self::from_runtime(config, node_registry, cancel_token)
    }

    pub fn from_runtime(
        config: StaticDiscoveryConfig,
        node_registry: Arc<dyn ClusterNodeDirectory>,
        cancel_token: CancellationToken,
    ) -> Self {
        Self {
            config,
            node_registry,
            cancel_token,
        }
    }

    /// Start the background probe loop.
    ///
    /// Returns the JoinHandle for the spawned task.
    fn discovered_node_info(
        discovered: &ProbedNodeIdentity,
        normalized_cluster_address: String,
    ) -> Option<NodeInfo> {
        if discovered.node_id.is_empty() {
            return None;
        }

        Some(
            NodeInfo::new(discovered.node_id.clone(), normalized_cluster_address)
                .with_epoch(discovered.epoch),
        )
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        let peers = self.config.peers.clone();
        let interval_secs = self.config.probe_interval_secs;
        let connect_timeout = self.config.connect_timeout;
        let cancel_token = self.cancel_token.clone();
        let node_registry = self.node_registry.clone();
        let cluster_secret = self.config.cluster_secret.clone();
        let default_cluster_port = self.config.default_cluster_port;

        info!(
            peer_count = peers.len(),
            interval_secs = interval_secs,
            "Starting static peer discovery"
        );

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
            // Skip immediate first tick; nodes may not be ready yet
            ticker.tick().await;

            // Track consecutive failures and known epochs per peer for unregistration
            let mut consecutive_failures: HashMap<String, u32> = HashMap::new();
            let mut peer_epochs: HashMap<String, u64> = HashMap::new();
            let mut peer_node_ids: HashMap<String, String> = HashMap::new();

            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Static peer discovery cancelled");
                        return;
                    }
                    _ = ticker.tick() => {
                        // Probe all peers concurrently using JoinSet
                        let mut join_set = tokio::task::JoinSet::new();

                        for peer in peers.clone() {
                            let timeout = connect_timeout;
                            let secret = cluster_secret.clone();
                            join_set.spawn(async move {
                                let normalized_cluster_address = match normalize_static_peer_address(
                                    &peer.cluster_address,
                                    default_cluster_port,
                                ) {
                                    Ok(address) => address,
                                    Err(error) => {
                                        warn!(
                                            peer = %peer.cluster_address,
                                            error = %error,
                                            "Skipping invalid static peer"
                                        );
                                        return (peer, String::new(), None);
                                    }
                                };
                                let discovered = Self::probe_peer(
                                    &normalized_cluster_address,
                                    timeout,
                                    &secret,
                                ).await;
                                (peer, normalized_cluster_address, discovered)
                            });
                        }

                        // Collect results
                        while let Some(result) = join_set.join_next().await {
                            let (peer, normalized_cluster_address, discovered) = match result {
                                Ok(r) => r,
                                Err(e) => {
                                    warn!(error = %e, "Static peer probe task panicked");
                                    continue;
                                }
                            };

                            if let Some(discovered) = discovered {
                                // Reset failure counter on success
                                consecutive_failures.remove(&peer.cluster_address);

                                let Some(node_info) = Self::discovered_node_info(
                                    &discovered,
                                    normalized_cluster_address,
                                ) else {
                                    warn!(
                                        peer = %peer.cluster_address,
                                        "Static peer probe succeeded but did not provide a usable node identity"
                                    );
                                    continue;
                                };
                                let registration_epoch = node_info.epoch;

                                if let Err(e) = node_registry.register_remote(node_info).await {
                                    warn!(
                                        peer = %peer.cluster_address,
                                        error = %e,
                                        "Failed to register static peer in NodeRegistry"
                                    );
                                } else {
                                    peer_epochs.insert(peer.cluster_address.clone(), registration_epoch);
                                    peer_node_ids.insert(
                                        peer.cluster_address.clone(),
                                        discovered.node_id.clone(),
                                    );
                                    debug!(peer = %peer.cluster_address, "Static peer registered/refreshed");
                                }
                            } else {
                                let failures = consecutive_failures.entry(peer.cluster_address.clone()).or_insert(0);
                                *failures += 1;

                                if *failures >= FAILURE_THRESHOLD {
                                    let epoch = peer_epochs.remove(&peer.cluster_address);
                                    warn!(
                                        peer = %peer.cluster_address,
                                        consecutive_failures = *failures,
                                        epoch = ?epoch,
                                        "Static peer unreachable for {} consecutive probes, unregistering",
                                        *failures
                                    );
                                    let node_id = peer_node_ids.remove(&peer.cluster_address);
                                    if let Some(node_id) = node_id {
                                        if let Err(e) = node_registry.unregister_remote(&node_id, epoch).await {
                                            warn!(
                                                peer = %peer.cluster_address,
                                                error = %e,
                                                "Failed to unregister disappeared static peer"
                                            );
                                        }
                                    } else {
                                        warn!(
                                            peer = %peer.cluster_address,
                                            "Static peer disappeared before a real node_id was known; skipping unregister"
                                        );
                                    }
                                    // Reset counter after unregistration attempt
                                    consecutive_failures.remove(&peer.cluster_address);
                                } else {
                                    debug!(
                                        peer = %peer.cluster_address,
                                        consecutive_failures = *failures,
                                        "Static peer unreachable, skipping"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        })
    }

    /// Probe a single peer by attempting a gRPC connection.
    ///
    /// Returns `true` if the peer responds, `false` on timeout or error.
    /// Delegates to the shared discovery probe and returns the remote node identity.
    async fn probe_peer(
        cluster_address: &str,
        connect_timeout: Duration,
        cluster_secret: &str,
    ) -> Option<ProbedNodeIdentity> {
        super::probe_node_identity(cluster_address, connect_timeout.as_secs(), cluster_secret).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovered_node_info_uses_peer_reported_node_id() -> Result<(), crate::Error> {
        let discovered = ProbedNodeIdentity {
            node_id: "peer-node-1".to_string(),
            cluster_address: "10.0.0.5:50051".to_string(),
            epoch: 7,
        };

        let normalized_address = normalize_static_peer_address("10.0.0.5:50051", 50052)?;
        let info = StaticDiscovery::discovered_node_info(&discovered, normalized_address)
            .ok_or_else(|| {
                crate::Error::Configuration("peer should map to discovered node info".to_string())
            })?;

        assert_eq!(info.node_id, "peer-node-1");
        assert_eq!(info.cluster_address, "10.0.0.5:50051");
        assert_eq!(info.epoch, 7);
        Ok(())
    }

    #[test]
    fn cluster_address_uses_default_port_only_when_omitted() -> Result<(), crate::Error> {
        assert_eq!(
            normalize_static_peer_address("node.example.com", 50051)?,
            "node.example.com:50051"
        );
        assert_eq!(
            normalize_static_peer_address("node.example.com:51000", 50051)?,
            "node.example.com:51000"
        );
        assert_eq!(
            normalize_static_peer_address("2001:db8::10", 50051)?,
            "[2001:db8::10]:50051"
        );
        assert_eq!(
            normalize_static_peer_address("[2001:db8::10]", 50051)?,
            "[2001:db8::10]:50051"
        );
        Ok(())
    }

    #[test]
    fn cluster_address_rejects_invalid_authorities() {
        for peer in ["", "host:abc", "host:0", "http://host:50051", "host/path"] {
            assert!(
                normalize_static_peer_address(peer, 50051).is_err(),
                "{peer}"
            );
        }
    }
}
