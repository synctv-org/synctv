//! Static peer discovery for non-K8s environments
//!
//! Periodically health-checks a configured list of peer gRPC addresses
//! and registers alive peers into the NodeRegistry. This enables cluster
//! formation without Kubernetes DNS or other dynamic discovery mechanisms.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tonic::transport::Endpoint;
use tracing::{debug, info, warn};

use crate::discovery::node_registry::NodeInfo;
use crate::discovery::NodeRegistry;

/// Number of consecutive probe failures before unregistering a peer.
const FAILURE_THRESHOLD: u32 = 3;

/// Configuration for a single static peer
#[derive(Debug, Clone)]
pub struct StaticPeerConfig {
    /// gRPC address (e.g., "host1:50051")
    pub grpc_address: String,
    /// Optional HTTP address for the peer. If not provided, it will be derived
    /// from the gRPC address by replacing the port with `default_http_port`.
    pub http_address: Option<String>,
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
    /// Default HTTP port used to derive http_address from gRPC address when
    /// `http_address` is not explicitly configured for a peer.
    pub default_http_port: u16,
}

impl Default for StaticDiscoveryConfig {
    fn default() -> Self {
        Self {
            peers: Vec::new(),
            probe_interval_secs: 10,
            connect_timeout: Duration::from_secs(3),
            cluster_secret: String::new(),
            default_http_port: 8080,
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
    pub fn new(
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
    /// Derive an HTTP address from a gRPC address by replacing the port.
    fn derive_http_address(grpc_address: &str, default_http_port: u16) -> String {
        // Try to split host:port
        if let Some(colon_pos) = grpc_address.rfind(':') {
            let host = &grpc_address[..colon_pos];
            format!("{host}:{default_http_port}")
        } else {
            // No port in the address, just append the HTTP port
            format!("{grpc_address}:{default_http_port}")
        }
    }

    pub fn start(self) -> tokio::task::JoinHandle<()> {
        let peers = self.config.peers.clone();
        let interval_secs = self.config.probe_interval_secs;
        let connect_timeout = self.config.connect_timeout;
        let cancel_token = self.cancel_token.clone();
        let node_registry = self.node_registry.clone();
        let cluster_secret = self.config.cluster_secret.clone();
        let default_http_port = self.config.default_http_port;

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
                                let alive = Self::probe_peer(
                                    &peer.grpc_address,
                                    timeout,
                                    &secret,
                                ).await;
                                (peer, alive)
                            });
                        }

                        // Collect results
                        while let Some(result) = join_set.join_next().await {
                            let (peer, alive) = match result {
                                Ok(r) => r,
                                Err(e) => {
                                    warn!(error = %e, "Static peer probe task panicked");
                                    continue;
                                }
                            };

                            let node_id = format!("static_{}", peer.grpc_address.replace([':', '.'], "_"));

                            if alive {
                                // Reset failure counter on success
                                consecutive_failures.remove(&peer.grpc_address);

                                let http_address = peer.http_address.clone().unwrap_or_else(|| {
                                    Self::derive_http_address(&peer.grpc_address, default_http_port)
                                });

                                // Use epoch=0 for static discovery peers. We don't
                                // know the peer's actual epoch, and register_remote
                                // only accepts registrations where incoming epoch >=
                                // existing epoch. Using 0 means a static-discovery
                                // registration will never overwrite a peer that has
                                // self-registered with its real epoch (>= 1).
                                let node_info = NodeInfo::new(
                                    node_id.clone(),
                                    peer.grpc_address.clone(),
                                    http_address,
                                )
                                .with_epoch(0);
                                let registration_epoch = node_info.epoch;

                                if let Err(e) = node_registry.register_remote(node_info).await {
                                    warn!(
                                        peer = %peer.grpc_address,
                                        error = %e,
                                        "Failed to register static peer in NodeRegistry"
                                    );
                                } else {
                                    peer_epochs.insert(peer.grpc_address.clone(), registration_epoch);
                                    debug!(peer = %peer.grpc_address, "Static peer registered/refreshed");
                                }
                            } else {
                                let failures = consecutive_failures.entry(peer.grpc_address.clone()).or_insert(0);
                                *failures += 1;

                                if *failures >= FAILURE_THRESHOLD {
                                    let epoch = peer_epochs.remove(&peer.grpc_address);
                                    warn!(
                                        peer = %peer.grpc_address,
                                        consecutive_failures = *failures,
                                        epoch = ?epoch,
                                        "Static peer unreachable for {} consecutive probes, unregistering",
                                        *failures
                                    );
                                    if let Err(e) = node_registry.unregister_remote(&node_id, epoch).await {
                                        warn!(
                                            peer = %peer.grpc_address,
                                            error = %e,
                                            "Failed to unregister disappeared static peer"
                                        );
                                    }
                                    // Reset counter after unregistration attempt
                                    consecutive_failures.remove(&peer.grpc_address);
                                } else {
                                    debug!(
                                        peer = %peer.grpc_address,
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
    async fn probe_peer(address: &str, connect_timeout: Duration, cluster_secret: &str) -> bool {
        let uri = if address.starts_with("http://") || address.starts_with("https://") {
            address.to_string()
        } else {
            format!("http://{address}")
        };

        let endpoint = match Endpoint::from_shared(uri) {
            Ok(ep) => ep.connect_timeout(connect_timeout).timeout(connect_timeout),
            Err(e) => {
                warn!(peer = %address, error = %e, "Invalid peer address");
                return false;
            }
        };

        let channel = match endpoint.connect().await {
            Ok(ch) => ch,
            Err(_) => return false,
        };

        // Use the GetNodes RPC as a health check
        use crate::grpc::synctv::cluster::cluster_service_client::ClusterServiceClient;
        use crate::grpc::synctv::cluster::GetNodesRequest;
        let mut client = ClusterServiceClient::new(channel);
        let mut request = tonic::Request::new(GetNodesRequest { status_filter: 0 });

        if !cluster_secret.is_empty() {
            match cluster_secret.parse::<tonic::metadata::MetadataValue<tonic::metadata::Ascii>>() {
                Ok(val) => {
                    request.metadata_mut().insert("x-cluster-secret", val);
                }
                Err(e) => {
                    warn!(error = %e, "cluster_secret contains invalid characters for gRPC metadata, skipping probe");
                    return false;
                }
            }
        }

        match client.get_nodes(request).await {
            Ok(_) => true,
            Err(e) => {
                debug!(peer = %address, error = %e, "Peer health check failed");
                false
            }
        }
    }
}
