use tonic::Status;

use crate::proto::{
    GetServerStateResponse, ServerStateCluster, ServerStateClusterNode,
    ServerStateClusterStatus as ProtoClusterStatus, ServerStateCpu,
    ServerStateCpuStatus as ProtoCpuStatus, ServerStateDatabase, ServerStateDatabasePool,
    ServerStateDatabaseStatus as ProtoDatabaseStatus, ServerStateEmail,
    ServerStateEmailStatus as ProtoEmailStatus, ServerStateLivestream,
    ServerStateLivestreamStatus as ProtoLivestreamStatus, ServerStateMemory,
    ServerStateMemoryStatus as ProtoMemoryStatus, ServerStateNode, ServerStateNodeFailure,
    ServerStateNodeStatus as ProtoNodeStatus, ServerStateRealtime, ServerStateRedis,
    ServerStateRedisStatus as ProtoRedisStatus, ServerStateSliceCache,
    ServerStateSliceCacheStatus as ProtoSliceCacheStatus, ServerStateSummary, ServerStateWebRtc,
    ServerStateWebRtcStatus as ProtoWebRtcStatus, ServerStateWsTicket,
    ServerStateWsTicketStatus as ProtoWsTicketStatus,
};
use synctv_proto::admin as admin_proto;

pub(crate) fn map_server_state_error(error: &synctv_core::service::ServerStateError) -> Status {
    match error {
        synctv_core::service::ServerStateError::InvalidSelection => {
            Status::invalid_argument(error.to_string())
        }
        synctv_core::service::ServerStateError::ClusterUnavailable(_)
        | synctv_core::service::ServerStateError::MissingClusterSecret
        | synctv_core::service::ServerStateError::InvalidClusterSecret => {
            Status::failed_precondition(error.to_string())
        }
        synctv_core::service::ServerStateError::Cluster(_)
        | synctv_core::service::ServerStateError::RemoteRequest { .. }
        | synctv_core::service::ServerStateError::RemoteDecode { .. } => {
            Status::unavailable(error.to_string())
        }
    }
}

pub(crate) fn server_state_to_management(
    response: synctv_core::service::ServerStateResponse,
) -> GetServerStateResponse {
    GetServerStateResponse {
        scope: response.scope.as_str().to_string(),
        summary: Some(ServerStateSummary {
            status: node_status_to_management(response.summary.status),
            healthy_nodes: response.summary.healthy_nodes,
            degraded_nodes: response.summary.degraded_nodes,
            unhealthy_nodes: response.summary.unhealthy_nodes,
            failed_nodes: response.summary.failed_nodes,
        }),
        nodes: response
            .nodes
            .into_iter()
            .map(server_state_node_to_management)
            .collect(),
        failures: response
            .failures
            .into_iter()
            .map(|failure| ServerStateNodeFailure {
                node_id: failure.node_id,
                error: failure.error,
            })
            .collect(),
    }
}

fn server_state_node_to_management(node: synctv_core::service::ServerStateNode) -> ServerStateNode {
    ServerStateNode {
        node_id: node.node_id,
        status: node_status_to_management(node.status),
        updated_at: node.updated_at,
        version: node.version,
        api_address: node.api_address,
        realtime: Some(ServerStateRealtime {
            distributed_enabled: node.realtime.distributed_enabled,
            connection_count: node.realtime.connection_count,
        }),
        database: Some(ServerStateDatabase {
            status: database_status_to_management(node.database.status),
            host: node.database.host,
            port: node.database.port,
            database: node.database.database,
            max_connections: node.database.max_connections,
            min_connections: node.database.min_connections,
            connect_timeout_seconds: node.database.connect_timeout_seconds,
            idle_timeout_seconds: node.database.idle_timeout_seconds,
            max_lifetime_seconds: node.database.max_lifetime_seconds,
            primary_pool: Some(server_state_database_pool_to_management(
                &node.database.primary_pool,
            )),
            read_pool_enabled: node.database.read_pool_enabled,
            read_host: node.database.read_host,
            read_port: node.database.read_port,
            read_pool: Some(server_state_database_pool_to_management(
                &node.database.read_pool,
            )),
            message: node.database.message.unwrap_or_default(),
        }),
        redis: Some(ServerStateRedis {
            status: redis_status_to_management(node.redis.status),
            configured: node.redis.configured,
            deployment_mode: node.redis.deployment_mode,
            database: node.redis.database,
            key_prefix: node.redis.key_prefix,
            connect_timeout_seconds: node.redis.connect_timeout_seconds,
            response_timeout_seconds: node.redis.response_timeout_seconds,
            pipeline_buffer_size: node.redis.pipeline_buffer_size,
            sentinel_master_name: node.redis.sentinel_master_name,
            sentinel_node_count: node.redis.sentinel_node_count,
            ping_latency_ms: node.redis.ping_latency_ms,
            message: node.redis.message.unwrap_or_default(),
        }),
        cluster: Some(ServerStateCluster {
            status: cluster_status_to_management(node.cluster.status),
            enabled: node.cluster.enabled,
            discovery_mode: node.cluster.discovery_mode,
            distributed_realtime_enabled: node.cluster.distributed_realtime_enabled,
            node_id_empty: node.cluster.node_id_empty,
            routable_node_count: node.cluster.routable_node_count,
            nodes: node
                .cluster
                .nodes
                .into_iter()
                .map(|cluster_node| ServerStateClusterNode {
                    node_id: cluster_node.node_id,
                    api_address: cluster_node.api_address,
                    last_heartbeat: cluster_node.last_heartbeat,
                    epoch: cluster_node.epoch,
                })
                .collect(),
            message: node.cluster.message.unwrap_or_default(),
        }),
        ws_ticket: Some(ServerStateWsTicket {
            status: ws_ticket_status_to_management(node.ws_ticket.status),
            cross_node_capable: node.ws_ticket.cross_node_capable,
            message: node.ws_ticket.message.unwrap_or_default(),
        }),
        email: Some(ServerStateEmail {
            status: email_status_to_management(node.email.status),
            configured: node.email.configured,
        }),
        livestream: Some(ServerStateLivestream {
            status: livestream_status_to_management(node.livestream.status),
            configured: node.livestream.configured,
            active_publisher_count: node.livestream.active_publisher_count,
            active_room_count: node.livestream.active_room_count,
            rtmp_port: node.livestream.rtmp_port,
            public_rtmp_host: node.livestream.public_rtmp_host,
            gop_cache_size: node.livestream.gop_cache_size,
            gop_cache_max_memory_mb: node.livestream.gop_cache_max_memory_mb,
            stream_timeout_seconds: node.livestream.stream_timeout_seconds,
            hls_storage_backend: node.livestream.hls_storage_backend,
            hls_storage_path: node.livestream.hls_storage_path,
            hls_memory_max_mb: node.livestream.hls_memory_max_mb,
        }),
        memory: Some(ServerStateMemory {
            status: memory_status_to_management(node.memory.status),
            used_bytes: node.memory.used_bytes,
            total_bytes: node.memory.total_bytes,
            available_bytes: node.memory.available_bytes,
            usage_percent: node.memory.usage_percent,
        }),
        webrtc: Some(ServerStateWebRtc {
            status: webrtc_status_to_management(node.webrtc.status),
            mode: node.webrtc.mode,
            builtin_stun_configured: node.webrtc.builtin_stun_configured,
            builtin_stun_state: node.webrtc.builtin_stun_state,
            reason: node.webrtc.reason,
            local_addr: node.webrtc.local_addr,
            external_addr: node.webrtc.external_addr,
            message: node.webrtc.message.unwrap_or_default(),
        }),
        cpu: Some(ServerStateCpu {
            status: cpu_status_to_management(node.cpu.status),
            available_parallelism: node.cpu.available_parallelism,
            current_load_1m: node.cpu.current_load_1m,
            load_ratio_1m: node.cpu.load_ratio_1m,
            load_average_1m: node.cpu.load_average_1m,
            load_average_5m: node.cpu.load_average_5m,
            load_average_15m: node.cpu.load_average_15m,
        }),
        slice_cache: Some(ServerStateSliceCache {
            status: slice_cache_status_to_management(node.slice_cache.status),
            engine_enabled: node.slice_cache.engine_enabled,
            backend: node.slice_cache.backend,
            file_cache_dir: node.slice_cache.file_cache_dir,
            slice_size: node.slice_cache.slice_size,
            max_cache_size: node.slice_cache.max_cache_size,
            segment_ttl_secs: node.slice_cache.segment_ttl_secs,
            stale_max_age_secs: node.slice_cache.stale_max_age_secs,
            stale_while_revalidate: node.slice_cache.stale_while_revalidate,
            eviction_interval_secs: node.slice_cache.eviction_interval_secs,
            watermark_ratio: node.slice_cache.watermark_ratio,
            current_size_bytes: node.slice_cache.current_size_bytes,
            entry_count: node.slice_cache.entry_count,
            metadata_entries: node.slice_cache.metadata_entries,
            updating_entries: node.slice_cache.updating_entries,
            lock_count: node.slice_cache.lock_count,
            usage_ratio: node.slice_cache.usage_ratio,
        }),
    }
}

fn server_state_database_pool_to_management(
    pool: &synctv_core::service::ServerStateDatabasePool,
) -> ServerStateDatabasePool {
    ServerStateDatabasePool {
        size: pool.size,
        idle_connections: pool.idle_connections,
        active_connections: pool.active_connections,
    }
}

fn node_status_to_management(status: synctv_core::service::ServerStateNodeStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateNodeStatus::Healthy => ProtoNodeStatus::Healthy,
        synctv_core::service::ServerStateNodeStatus::Degraded => ProtoNodeStatus::Degraded,
        synctv_core::service::ServerStateNodeStatus::Unhealthy => ProtoNodeStatus::Unhealthy,
    }
    .into()
}

fn database_status_to_management(status: synctv_core::service::ServerStateDatabaseStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateDatabaseStatus::Healthy => ProtoDatabaseStatus::Healthy,
        synctv_core::service::ServerStateDatabaseStatus::Unhealthy => {
            ProtoDatabaseStatus::Unhealthy
        }
    }
    .into()
}

fn redis_status_to_management(status: synctv_core::service::ServerStateRedisStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateRedisStatus::Healthy => ProtoRedisStatus::Healthy,
        synctv_core::service::ServerStateRedisStatus::NotConfigured => {
            ProtoRedisStatus::NotConfigured
        }
        synctv_core::service::ServerStateRedisStatus::Unhealthy => ProtoRedisStatus::Unhealthy,
    }
    .into()
}

fn cluster_status_to_management(status: synctv_core::service::ServerStateClusterStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateClusterStatus::Healthy => ProtoClusterStatus::Healthy,
        synctv_core::service::ServerStateClusterStatus::Unhealthy => ProtoClusterStatus::Unhealthy,
        synctv_core::service::ServerStateClusterStatus::Disabled => ProtoClusterStatus::Disabled,
    }
    .into()
}

fn ws_ticket_status_to_management(status: synctv_core::service::ServerStateWsTicketStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateWsTicketStatus::Healthy => ProtoWsTicketStatus::Healthy,
        synctv_core::service::ServerStateWsTicketStatus::Unhealthy => {
            ProtoWsTicketStatus::Unhealthy
        }
    }
    .into()
}

fn email_status_to_management(status: synctv_core::service::ServerStateEmailStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateEmailStatus::Configured => ProtoEmailStatus::Configured,
        synctv_core::service::ServerStateEmailStatus::NotConfigured => {
            ProtoEmailStatus::NotConfigured
        }
    }
    .into()
}

fn livestream_status_to_management(
    status: synctv_core::service::ServerStateLivestreamStatus,
) -> i32 {
    match status {
        synctv_core::service::ServerStateLivestreamStatus::Configured => {
            ProtoLivestreamStatus::Configured
        }
        synctv_core::service::ServerStateLivestreamStatus::NotConfigured => {
            ProtoLivestreamStatus::NotConfigured
        }
    }
    .into()
}

fn memory_status_to_management(status: synctv_core::service::ServerStateMemoryStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateMemoryStatus::Healthy => ProtoMemoryStatus::Healthy,
        synctv_core::service::ServerStateMemoryStatus::Unhealthy => ProtoMemoryStatus::Unhealthy,
        synctv_core::service::ServerStateMemoryStatus::Unknown => ProtoMemoryStatus::Unknown,
    }
    .into()
}

fn webrtc_status_to_management(status: synctv_core::service::ServerStateWebRtcStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateWebRtcStatus::Healthy => ProtoWebRtcStatus::Healthy,
        synctv_core::service::ServerStateWebRtcStatus::Degraded => ProtoWebRtcStatus::Degraded,
        synctv_core::service::ServerStateWebRtcStatus::Disabled => ProtoWebRtcStatus::Disabled,
    }
    .into()
}

fn cpu_status_to_management(status: synctv_core::service::ServerStateCpuStatus) -> i32 {
    match status {
        synctv_core::service::ServerStateCpuStatus::Healthy => ProtoCpuStatus::Healthy,
        synctv_core::service::ServerStateCpuStatus::Degraded => ProtoCpuStatus::Degraded,
        synctv_core::service::ServerStateCpuStatus::Unhealthy => ProtoCpuStatus::Unhealthy,
        synctv_core::service::ServerStateCpuStatus::Unknown => ProtoCpuStatus::Unknown,
    }
    .into()
}

fn slice_cache_status_to_management(
    status: synctv_core::service::ServerStateSliceCacheStatus,
) -> i32 {
    match status {
        synctv_core::service::ServerStateSliceCacheStatus::Healthy => {
            ProtoSliceCacheStatus::Healthy
        }
        synctv_core::service::ServerStateSliceCacheStatus::Disabled => {
            ProtoSliceCacheStatus::Disabled
        }
    }
    .into()
}

pub(crate) fn map_slice_cache_error(
    error: &synctv_core::service::SliceCacheManagementError,
) -> Status {
    match error {
        synctv_core::service::SliceCacheManagementError::InvalidSelection => {
            Status::invalid_argument(error.to_string())
        }
        synctv_core::service::SliceCacheManagementError::ClusterUnavailable(_)
        | synctv_core::service::SliceCacheManagementError::MissingClusterSecret
        | synctv_core::service::SliceCacheManagementError::InvalidClusterSecret => {
            Status::failed_precondition(error.to_string())
        }
        synctv_core::service::SliceCacheManagementError::Cluster(_)
        | synctv_core::service::SliceCacheManagementError::RemoteRequest { .. } => {
            Status::unavailable(error.to_string())
        }
    }
}

pub(crate) fn slice_cache_selection(
    node_id: String,
    all_nodes: bool,
) -> synctv_core::service::SliceCacheSelection {
    synctv_core::service::SliceCacheSelection {
        node_id: (!node_id.trim().is_empty()).then_some(node_id),
        all_nodes,
    }
}

fn slice_cache_config_to_management(
    config: synctv_core::service::SliceCacheConfigInfo,
) -> admin_proto::SliceCacheConfigInfo {
    admin_proto::SliceCacheConfigInfo {
        engine_enabled: config.engine_enabled,
        backend: config.backend,
        file_cache_dir: config.file_cache_dir,
        slice_size: config.slice_size,
        max_cache_size: config.max_cache_size,
        segment_ttl_secs: config.segment_ttl_secs,
        stale_max_age_secs: config.stale_max_age_secs,
        stale_while_revalidate: config.stale_while_revalidate,
        eviction_interval_secs: config.eviction_interval_secs,
        watermark_ratio: config.watermark_ratio,
    }
}

fn slice_cache_stats_node_to_management(
    stats: synctv_core::service::SliceCacheStatsNode,
) -> admin_proto::SliceCacheStatsNode {
    admin_proto::SliceCacheStatsNode {
        node_id: stats.node_id,
        config: Some(slice_cache_config_to_management(stats.config)),
        current_size_bytes: stats.current_size_bytes,
        entry_count: stats.entry_count,
        metadata_entries: stats.metadata_entries,
        updating_entries: stats.updating_entries,
        lock_count: stats.lock_count,
        usage_ratio: stats.usage_ratio,
    }
}

fn slice_cache_failure_to_management(
    failure: synctv_core::service::SliceCacheNodeFailure,
) -> admin_proto::SliceCacheNodeFailure {
    admin_proto::SliceCacheNodeFailure {
        node_id: failure.node_id,
        error: failure.error,
    }
}

pub(crate) fn get_slice_cache_stats_to_management(
    response: synctv_core::service::SliceCacheStatsResponse,
) -> admin_proto::GetSliceCacheStatsResponse {
    admin_proto::GetSliceCacheStatsResponse {
        nodes: response
            .nodes
            .into_iter()
            .map(slice_cache_stats_node_to_management)
            .collect(),
        failures: response
            .failures
            .into_iter()
            .map(slice_cache_failure_to_management)
            .collect(),
    }
}

fn purge_slice_cache_node_to_management(
    response: synctv_core::service::SliceCachePurgeNodeResult,
) -> admin_proto::PurgeSliceCacheNodeResult {
    admin_proto::PurgeSliceCacheNodeResult {
        node_id: response.node_id,
        success: response.success,
        removed_entries: response.removed_entries,
        freed_bytes: response.freed_bytes,
        stats: response.stats.map(slice_cache_stats_node_to_management),
    }
}

pub(crate) fn purge_slice_cache_to_management(
    response: synctv_core::service::SliceCachePurgeResponse,
) -> admin_proto::PurgeSliceCacheResponse {
    admin_proto::PurgeSliceCacheResponse {
        success: response.success,
        removed_entries: response.removed_entries,
        freed_bytes: response.freed_bytes,
        stats: response.stats.map(slice_cache_stats_node_to_management),
        nodes: response
            .nodes
            .into_iter()
            .map(purge_slice_cache_node_to_management)
            .collect(),
        failures: response
            .failures
            .into_iter()
            .map(slice_cache_failure_to_management)
            .collect(),
    }
}

fn evict_expired_slice_cache_node_to_management(
    response: synctv_core::service::SliceCacheEvictExpiredNodeResult,
) -> admin_proto::EvictExpiredSliceCacheNodeResult {
    admin_proto::EvictExpiredSliceCacheNodeResult {
        node_id: response.node_id,
        success: response.success,
        removed_expired_entries: response.removed_expired_entries,
        stats: response.stats.map(slice_cache_stats_node_to_management),
    }
}

pub(crate) fn evict_expired_slice_cache_to_management(
    response: synctv_core::service::SliceCacheEvictExpiredResponse,
) -> admin_proto::EvictExpiredSliceCacheResponse {
    admin_proto::EvictExpiredSliceCacheResponse {
        success: response.success,
        removed_expired_entries: response.removed_expired_entries,
        stats: response.stats.map(slice_cache_stats_node_to_management),
        nodes: response
            .nodes
            .into_iter()
            .map(evict_expired_slice_cache_node_to_management)
            .collect(),
        failures: response
            .failures
            .into_iter()
            .map(slice_cache_failure_to_management)
            .collect(),
    }
}
