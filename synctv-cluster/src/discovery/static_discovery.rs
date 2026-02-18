//! Static peer discovery for non-K8s environments
//!
//! Periodically health-checks a configured list of peer gRPC addresses
//! and registers alive peers into the NodeRegistry. This enables cluster
//! formation without Kubernetes DNS or other dynamic discovery mechanisms.

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;
use tonic::transport::Endpoint;
use tracing::{debug, info, warn};

use crate::discovery::node_registry::NodeInfo;
use crate::discovery::NodeRegistry;

/// Configuration for static peer discovery
#[derive(Debug, Clone)]
pub struct StaticDiscoveryConfig {
    /// List of peer gRPC addresses (e.g., ["host1:50051", "host2:50051"])
    pub peers: Vec<String>,
    /// How often to probe peers (seconds)
    pub probe_interval_secs: u64,
    /// Timeout for each gRPC connect attempt
    pub connect_timeout: Duration,
    /// Cluster secret for authenticated probing
    pub cluster_secret: String,
}

impl Default for StaticDiscoveryConfig {
    fn default() -> Self {
        Self {
            peers: Vec::new(),
            probe_interval_secs: 10,
            connect_timeout: Duration::from_secs(3),
            cluster_secret: String::new(),
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
    pub fn start(self) -> tokio::task::JoinHandle<()> {
        let peers = self.config.peers.clone();
        let interval_secs = self.config.probe_interval_secs;
        let connect_timeout = self.config.connect_timeout;
        let cancel_token = self.cancel_token.clone();
        let node_registry = self.node_registry.clone();
        let cluster_secret = self.config.cluster_secret.clone();

        info!(
            peer_count = peers.len(),
            interval_secs = interval_secs,
            "Starting static peer discovery"
        );

        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
            // Skip immediate first tick; nodes may not be ready yet
            ticker.tick().await;

            loop {
                tokio::select! {
                    () = cancel_token.cancelled() => {
                        info!("Static peer discovery cancelled");
                        return;
                    }
                    _ = ticker.tick() => {
                        for peer_addr in &peers {
                            let alive = Self::probe_peer(
                                peer_addr,
                                connect_timeout,
                                &cluster_secret,
                            ).await;

                            if alive {
                                // Register the peer as a remote node
                                let node_info = NodeInfo::new(
                                    format!("static_{}", peer_addr.replace([':', '.'], "_")),
                                    peer_addr.clone(),
                                    String::new(), // HTTP address unknown from static config
                                );

                                if let Err(e) = node_registry.register_remote(node_info).await {
                                    warn!(
                                        peer = %peer_addr,
                                        error = %e,
                                        "Failed to register static peer in NodeRegistry"
                                    );
                                } else {
                                    debug!(peer = %peer_addr, "Static peer registered/refreshed");
                                }
                            } else {
                                debug!(peer = %peer_addr, "Static peer unreachable, skipping");
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
    async fn probe_peer(
        address: &str,
        connect_timeout: Duration,
        cluster_secret: &str,
    ) -> bool {
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
            if let Ok(val) = cluster_secret.parse::<tonic::metadata::MetadataValue<_>>() {
                request.metadata_mut().insert("x-cluster-secret", val);
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
