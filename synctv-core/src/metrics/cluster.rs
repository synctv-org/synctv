use super::*;

pub static CLUSTER_CONNECTIONS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "synctv_cluster_connections_total",
        "Current number of active connections on this cluster node",
    )
});

pub static NODE_ACTIVE_ROOMS: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "synctv_node_active_rooms",
        "Current number of active rooms on this node",
    )
});

pub static REALTIME_EVENTS_PUBLISHED: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "synctv_realtime_events_published_total",
            "Total realtime events published",
            &["event_type"],
        )
    });

pub static REALTIME_EVENTS_RECEIVED: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "synctv_realtime_events_received_total",
            "Total realtime events received from other nodes",
            &["event_type"],
        )
    });

pub static REALTIME_EVENTS_DROPPED: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "synctv_realtime_events_dropped_total",
            "Total realtime events dropped",
            &["reason"],
        )
    });

pub static CLUSTER_HEARTBEAT_FAILURES: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "synctv_cluster_heartbeat_failures",
            "Consecutive Redis heartbeat failures for network partition detection",
        )
    });

pub static LEADER_ELECTION_STATE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "synctv_cluster_leader_election_state",
        "Leader election state (1 = leader, 0 = follower)",
    )
});

pub static LEADER_ELECTION_EPOCH: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "synctv_cluster_leader_election_epoch",
        "Leader election epoch (fencing token), incremented on each leadership acquisition",
    )
});

pub static LEADER_ELECTION_CONSECUTIVE_FAILURES: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "synctv_cluster_leader_election_consecutive_failures",
            "Consecutive leader election failures (network partition or backend outage detection)",
        )
    });

pub static CLUSTER_EPOCH_MISMATCH_QUARANTINE: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "synctv_cluster_epoch_mismatch_quarantine",
            "Epoch mismatch quarantine state (1 = quarantined due to split-brain, 0 = normal)",
        )
    });

pub static LEADER_ELECTION_MODE: std::sync::LazyLock<IntGauge> = std::sync::LazyLock::new(|| {
    int_gauge(
        "synctv_cluster_leader_election_mode",
        "Leader election mode (0=standalone, 1=redis, 2=k8s_lease)",
    )
});

pub static DISTRIBUTED_COUNTER_TTL_REFRESHES: std::sync::LazyLock<IntCounterVec> =
    std::sync::LazyLock::new(|| {
        int_counter_vec(
            "synctv_cluster_distributed_counter_ttl_refreshes_total",
            "Total distributed counter TTL refresh operations",
            &["result"],
        )
    });

pub static DISTRIBUTED_COUNTER_TTL_KEYS_REFRESHED: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "synctv_cluster_distributed_counter_ttl_keys_refreshed",
            "Number of keys refreshed in the last TTL refresh cycle",
        )
    });

pub static DISTRIBUTED_COUNTER_TTL_CONSECUTIVE_FAILURES: std::sync::LazyLock<IntGauge> =
    std::sync::LazyLock::new(|| {
        int_gauge(
            "synctv_cluster_distributed_counter_ttl_consecutive_failures",
            "Consecutive TTL refresh failures (alert when >= 3)",
        )
    });
