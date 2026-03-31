//! Static peer discovery for non-K8s environments
//!
//! Periodically health-checks a configured list of peer API addresses
//! and registers alive peers into the NodeRegistry. This enables cluster
//! formation without Kubernetes DNS or other dynamic discovery mechanisms.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::discovery::node_registry::NodeInfo;
use crate::discovery::{NodeRegistry, ProbedNodeIdentity};

/// Number of consecutive probe failures before unregistering a peer.
const FAILURE_THRESHOLD: u32 = 3;

/// Configuration for a single static peer
#[derive(Debug, Clone)]
pub struct StaticPeerConfig {
    /// Shared API address (e.g., "host1:8080")
    pub api_address: String,
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
    /// Default API port used to derive api_address from a host-only peer entry.
    pub default_api_port: u16,
}

impl Default for StaticDiscoveryConfig {
    fn default() -> Self {
        Self {
            peers: Vec::new(),
            probe_interval_secs: 10,
            connect_timeout: Duration::from_secs(3),
            cluster_secret: String::new(),
            default_api_port: 8080,
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
    node_registry: Arc<NodeRegistry>,
    cancel_token: CancellationToken,
}

impl StaticDiscovery {
    /// Create a new StaticDiscovery
    pub const fn new(
        config: StaticDiscoveryConfig,
        node_registry: Arc<NodeRegistry>,
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
    /// Normalize a configured peer entry to the shared API address used for
    /// both probing and registration.
    fn derive_api_address(peer_address: &str, default_api_port: u16) -> String {
        // Try to split host:port
        if let Some(colon_pos) = peer_address.rfind(':') {
            let host = &peer_address[..colon_pos];
            format!("{host}:{default_api_port}")
        } else {
            // No port in the address, just append the shared API port.
            format!("{peer_address}:{default_api_port}")
        }
    }

    fn discovered_node_info(
        discovered: &ProbedNodeIdentity,
        api_address: &str,
        default_api_port: u16,
    ) -> Option<NodeInfo> {
        if discovered.node_id.is_empty() {
            return None;
        }

        Some(
            NodeInfo::new(
                discovered.node_id.clone(),
                Self::derive_api_address(api_address, default_api_port),
            )
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
        let default_api_port = self.config.default_api_port;

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
                                let discovered = Self::probe_peer(
                                    &peer.api_address,
                                    timeout,
                                    &secret,
                                    default_api_port,
                                ).await;
                                (peer, discovered)
                            });
                        }

                        // Collect results
                        while let Some(result) = join_set.join_next().await {
                            let (peer, discovered) = match result {
                                Ok(r) => r,
                                Err(e) => {
                                    warn!(error = %e, "Static peer probe task panicked");
                                    continue;
                                }
                            };

                            if let Some(discovered) = discovered {
                                // Reset failure counter on success
                                consecutive_failures.remove(&peer.api_address);

                                let node_info = match Self::discovered_node_info(
                                    &discovered,
                                    &peer.api_address,
                                    default_api_port,
                                ) {
                                    Some(info) => info,
                                    None => {
                                        warn!(
                                            peer = %peer.api_address,
                                            "Static peer probe succeeded but did not provide a usable node identity"
                                        );
                                        continue;
                                    }
                                };

                                let registration_epoch = node_info.epoch;

                                if let Err(e) = node_registry.register_remote(node_info).await {
                                    warn!(
                                        peer = %peer.api_address,
                                        error = %e,
                                        "Failed to register static peer in NodeRegistry"
                                    );
                                } else {
                                    peer_epochs.insert(peer.api_address.clone(), registration_epoch);
                                    peer_node_ids.insert(
                                        peer.api_address.clone(),
                                        discovered.node_id.clone(),
                                    );
                                    debug!(peer = %peer.api_address, "Static peer registered/refreshed");
                                }
                            } else {
                                let failures = consecutive_failures.entry(peer.api_address.clone()).or_insert(0);
                                *failures += 1;

                                if *failures >= FAILURE_THRESHOLD {
                                    let epoch = peer_epochs.remove(&peer.api_address);
                                    warn!(
                                        peer = %peer.api_address,
                                        consecutive_failures = *failures,
                                        epoch = ?epoch,
                                        "Static peer unreachable for {} consecutive probes, unregistering",
                                        *failures
                                    );
                                    let node_id = peer_node_ids.remove(&peer.api_address);
                                    if let Some(node_id) = node_id {
                                        if let Err(e) = node_registry.unregister_remote(&node_id, epoch).await {
                                            warn!(
                                                peer = %peer.api_address,
                                                error = %e,
                                                "Failed to unregister disappeared static peer"
                                            );
                                        }
                                    } else {
                                        warn!(
                                            peer = %peer.api_address,
                                            "Static peer disappeared before a real node_id was known; skipping unregister"
                                        );
                                    }
                                    // Reset counter after unregistration attempt
                                    consecutive_failures.remove(&peer.api_address);
                                } else {
                                    debug!(
                                        peer = %peer.api_address,
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
        address: &str,
        connect_timeout: Duration,
        cluster_secret: &str,
        default_api_port: u16,
    ) -> Option<ProbedNodeIdentity> {
        let api_address = Self::derive_api_address(address, default_api_port);
        super::probe_node_identity(&api_address, connect_timeout.as_secs(), cluster_secret).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_discovered_node_info_uses_peer_reported_node_id() {
        let discovered = ProbedNodeIdentity {
            node_id: "peer-node-1".to_string(),
            api_address: "10.0.0.5:50051".to_string(),
            epoch: 7,
        };

        let info = StaticDiscovery::discovered_node_info(&discovered, "10.0.0.5:50051", 8080)
            .expect("peer should map to discovered node info");

        assert_eq!(info.node_id, "peer-node-1");
        assert_eq!(info.api_address, "10.0.0.5:8080");
        assert_eq!(info.epoch, 7);
    }
}
