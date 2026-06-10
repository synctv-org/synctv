use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use futures::stream::{FuturesUnordered, StreamExt};
use moka::sync::Cache;
use synctv_cluster::discovery::{ClusterNodeDirectory, NodeInfo};
use synctv_core::models::{RoomId, UserId};
use tonic::metadata::MetadataValue;
use tonic::transport::{Channel, Endpoint};
use tracing::{debug, warn};

use super::proto::{
    realtime_presence_service_client::RealtimePresenceServiceClient as GrpcClient,
    GetRoomConnectionsRequest, GetRoomConnectionsResponse, GetUserOnlineStatusRequest,
    GetUserOnlineStatusResponse, RoomConnection, UserOnlineStatus,
};
use crate::error::{Error, Result};

const DEFAULT_FAN_OUT_AGGREGATE_TIMEOUT: Duration = Duration::from_secs(5);
const CHANNEL_CACHE_TTL_SECS: u64 = 300;
const CHANNEL_CACHE_MAX_CAPACITY: u64 = 256;

#[derive(Debug, Clone)]
pub struct RealtimePresenceClientConfig {
    pub per_node_timeout: Duration,
    pub connect_timeout: Duration,
    pub aggregate_timeout: Duration,
    pub cluster_secret: String,
    pub self_node_id: String,
}

impl Default for RealtimePresenceClientConfig {
    fn default() -> Self {
        Self {
            per_node_timeout: Duration::from_secs(3),
            connect_timeout: Duration::from_secs(2),
            aggregate_timeout: DEFAULT_FAN_OUT_AGGREGATE_TIMEOUT,
            cluster_secret: String::new(),
            self_node_id: String::new(),
        }
    }
}

#[derive(Debug)]
pub struct FanOutResult<T> {
    pub data: T,
    pub nodes_succeeded: usize,
    pub nodes_failed: usize,
    pub failures: Vec<(String, String)>,
}

impl<T> FanOutResult<T> {
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.nodes_failed == 0
    }

    #[must_use]
    pub const fn total_nodes(&self) -> usize {
        self.nodes_succeeded + self.nodes_failed
    }
}

pub struct RealtimePresenceClient {
    node_registry: Arc<dyn ClusterNodeDirectory>,
    config: RealtimePresenceClientConfig,
    channels: Cache<String, Channel>,
}

impl RealtimePresenceClient {
    pub fn new<N>(node_registry: Arc<N>, config: RealtimePresenceClientConfig) -> Self
    where
        N: ClusterNodeDirectory + 'static,
    {
        Self::from_runtime(node_registry, config)
    }

    pub fn from_runtime(
        node_registry: Arc<dyn ClusterNodeDirectory>,
        config: RealtimePresenceClientConfig,
    ) -> Self {
        let channels = Cache::builder()
            .max_capacity(CHANNEL_CACHE_MAX_CAPACITY)
            .time_to_idle(Duration::from_secs(CHANNEL_CACHE_TTL_SECS))
            .build();

        Self {
            node_registry,
            config,
            channels,
        }
    }

    fn channel_cache_key(node_id: &str, address: &str) -> String {
        format!("{node_id}|{address}")
    }

    async fn get_channel(&self, node_id: &str, address: &str) -> Result<Channel> {
        let cache_key = Self::channel_cache_key(node_id, address);
        if let Some(channel) = self.channels.get(&cache_key) {
            return Ok(channel);
        }

        let uri = if address.starts_with("http://") || address.starts_with("https://") {
            address.to_string()
        } else {
            format!("http://{address}")
        };

        let endpoint = Endpoint::from_shared(uri)
            .map_err(|error| Error::Rpc(format!("Invalid endpoint URI for {address}: {error}")))?
            .connect_timeout(self.config.connect_timeout)
            .timeout(self.config.per_node_timeout);

        let channel = endpoint
            .connect()
            .await
            .map_err(|error| Error::Rpc(format!("Failed to connect to {address}: {error}")))?;

        self.channels.insert(cache_key, channel.clone());
        Ok(channel)
    }

    pub fn invalidate_channel(&self, node_id: &str, address: &str) {
        let cache_key = Self::channel_cache_key(node_id, address);
        self.channels.invalidate(&cache_key);
        debug!(node_id = %node_id, address = %address, "Invalidated cached realtime gRPC channel");
    }

    pub fn invalidate_all_channels(&self) {
        self.channels.invalidate_all();
        debug!("Invalidated all cached realtime gRPC channels");
    }

    fn attach_secret<T>(&self, request: &mut tonic::Request<T>) -> Result<()> {
        if self.config.cluster_secret.is_empty() {
            return Err(Error::Rpc(
                "cluster secret is required for realtime presence RPC".to_string(),
            ));
        }

        let value = self
            .config
            .cluster_secret
            .parse::<MetadataValue<tonic::metadata::Ascii>>()
            .map_err(|error| {
                tracing::error!("cluster_secret contains invalid characters (non-ASCII?): {error}");
                Error::Rpc("invalid cluster secret configuration".to_string())
            })?;
        request.metadata_mut().insert("x-cluster-secret", value);
        Ok(())
    }

    async fn fan_out<T, Item, QueryFn, QueryFut, ExtractFn>(
        &self,
        rpc_name: &str,
        query_fn: QueryFn,
        extract_fn: ExtractFn,
    ) -> Result<FanOutResult<Vec<Item>>>
    where
        Item: Send + 'static,
        T: Send + 'static,
        QueryFn: Fn(String, String) -> QueryFut,
        QueryFut: std::future::Future<Output = Result<T>> + Send,
        ExtractFn: Fn(T) -> Vec<Item>,
    {
        let (nodes, _view_mode) =
            self.node_registry
                .get_routable_nodes()
                .await
                .map_err(|error| {
                    Error::Rpc(format!("failed to resolve routable cluster nodes: {error}"))
                })?;
        let remote_nodes = nodes
            .into_iter()
            .filter(|node| node.node_id != self.config.self_node_id)
            .collect::<Vec<NodeInfo>>();

        if remote_nodes.is_empty() {
            return Ok(FanOutResult {
                data: Vec::new(),
                nodes_succeeded: 0,
                nodes_failed: 0,
                failures: Vec::new(),
            });
        }

        let mut pending_nodes = remote_nodes
            .iter()
            .map(|node| (node.node_id.clone(), node.api_address.clone()))
            .collect::<HashMap<_, _>>();
        let mut futs = remote_nodes
            .iter()
            .map(|node| {
                let node_id = node.node_id.clone();
                let address = node.api_address.clone();
                let fut = query_fn(node_id.clone(), address.clone());
                async move { (node_id, address, fut.await) }
            })
            .collect::<FuturesUnordered<_>>();

        let mut all_items = Vec::new();
        let mut nodes_succeeded = 0usize;
        let mut nodes_failed = 0usize;
        let mut failures = Vec::new();
        let deadline = tokio::time::Instant::now() + self.config.aggregate_timeout;

        loop {
            tokio::select! {
                biased;
                maybe_result = futs.next(), if !futs.is_empty() => {
                    if let Some((node_id, address, result)) = maybe_result {
                        pending_nodes.remove(&node_id);
                        match result {
                            Ok(response) => {
                                nodes_succeeded += 1;
                                all_items.extend(extract_fn(response));
                            }
                            Err(error) => {
                                nodes_failed += 1;
                                warn!(node_id = %node_id, address = %address, error = %error, rpc = %rpc_name, "Realtime fan-out failed for node");
                                self.invalidate_channel(&node_id, &address);
                                failures.push((node_id, error.to_string()));
                            }
                        }
                    }
                }
                () = tokio::time::sleep_until(deadline) => {
                    nodes_failed += futs.len();
                    failures.extend(pending_nodes.into_iter().map(|(node_id, address)| {
                        (node_id, format!("aggregate timeout after {:?} while waiting for {address}", self.config.aggregate_timeout))
                    }));
                    break;
                }
                else => break,
            }
        }

        Ok(FanOutResult {
            data: all_items,
            nodes_succeeded,
            nodes_failed,
            failures,
        })
    }

    pub async fn fan_out_user_online_status(
        &self,
        user_ids: Vec<UserId>,
    ) -> Result<FanOutResult<Vec<UserOnlineStatus>>> {
        let user_ids = user_ids
            .into_iter()
            .map(|id| id.as_i64())
            .collect::<Vec<_>>();
        self.fan_out(
            "RealtimePresence/GetUserOnlineStatus",
            |node_id, address| {
                let user_ids = user_ids.clone();
                async move {
                    self.query_user_status_single(&node_id, &address, user_ids)
                        .await
                }
            },
            |response| response.statuses,
        )
        .await
    }

    async fn query_user_status_single(
        &self,
        node_id: &str,
        address: &str,
        user_ids: Vec<i64>,
    ) -> Result<GetUserOnlineStatusResponse> {
        let mut request = tonic::Request::new(GetUserOnlineStatusRequest { user_ids });
        self.attach_secret(&mut request)?;
        let channel = self.get_channel(node_id, address).await?;
        let mut client = GrpcClient::new(channel);

        client
            .get_user_online_status(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|error| {
                Error::Rpc(format!(
                    "Realtime GetUserOnlineStatus RPC failed for {address}: {error}"
                ))
            })
    }

    pub async fn fan_out_room_connections(
        &self,
        room_id: RoomId,
    ) -> Result<FanOutResult<Vec<RoomConnection>>> {
        let room_id = room_id.as_i64();
        self.fan_out(
            "RealtimePresence/GetRoomConnections",
            |node_id, address| async move {
                self.query_room_connections_single(&node_id, &address, room_id)
                    .await
            },
            |response| response.connections,
        )
        .await
    }

    async fn query_room_connections_single(
        &self,
        node_id: &str,
        address: &str,
        room_id: i64,
    ) -> Result<GetRoomConnectionsResponse> {
        let mut request = tonic::Request::new(GetRoomConnectionsRequest { room_id });
        self.attach_secret(&mut request)?;
        let channel = self.get_channel(node_id, address).await?;
        let mut client = GrpcClient::new(channel);

        client
            .get_room_connections(request)
            .await
            .map(tonic::Response::into_inner)
            .map_err(|error| {
                Error::Rpc(format!(
                    "Realtime GetRoomConnections RPC failed for {address}: {error}"
                ))
            })
    }

    #[must_use]
    pub fn merge_user_statuses(statuses: Vec<UserOnlineStatus>) -> Vec<UserOnlineStatus> {
        let mut merged: HashMap<i64, UserOnlineStatus> = HashMap::new();

        for status in statuses {
            merged
                .entry(status.user_id)
                .and_modify(|existing| {
                    existing.is_online |= status.is_online;
                    let mut room_ids = existing.room_ids.iter().copied().collect::<HashSet<i64>>();
                    room_ids.extend(status.room_ids.iter().copied());
                    existing.room_ids = room_ids.into_iter().collect();
                    existing.room_ids.sort_unstable();
                    if !status.node_id.is_empty() {
                        if existing.node_id.is_empty() {
                            existing.node_id.clone_from(&status.node_id);
                        } else if !existing.node_id.split(',').any(|id| id == status.node_id) {
                            existing.node_id.push(',');
                            existing.node_id.push_str(&status.node_id);
                        }
                    }
                })
                .or_insert(status);
        }

        let mut values = merged.into_values().collect::<Vec<_>>();
        values.sort_by_key(|status| status.user_id);
        values
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use synctv_cluster::NodeRegistry;

    use super::*;

    fn test_client(cluster_secret: String) -> synctv_cluster::Result<RealtimePresenceClient> {
        let registry = Arc::new(NodeRegistry::new_local_only(
            "node-a".to_string(),
            30,
            "presence-client-test:",
        )?);
        Ok(RealtimePresenceClient::new(
            registry,
            RealtimePresenceClientConfig {
                cluster_secret,
                self_node_id: "node-a".to_string(),
                ..RealtimePresenceClientConfig::default()
            },
        ))
    }

    #[test]
    fn attach_secret_rejects_empty_cluster_secret(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let client = test_client(String::new())?;
        let mut request = tonic::Request::new(GetUserOnlineStatusRequest { user_ids: vec![1] });

        let error = client
            .attach_secret(&mut request)
            .expect_err("empty cluster secret must fail before remote RPC");

        assert!(
            error.to_string().contains("cluster secret is required"),
            "unexpected error: {error}"
        );
        Ok(())
    }

    #[test]
    fn attach_secret_sets_cluster_secret_metadata(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        let client = test_client("cluster-secret".to_string())?;
        let mut request = tonic::Request::new(GetUserOnlineStatusRequest { user_ids: vec![1] });

        client.attach_secret(&mut request)?;

        assert_eq!(
            request
                .metadata()
                .get("x-cluster-secret")
                .and_then(|value| value.to_str().ok()),
            Some("cluster-secret")
        );
        Ok(())
    }
}
