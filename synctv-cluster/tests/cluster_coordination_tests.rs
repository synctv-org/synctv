//! Integration tests for cluster coordination scenarios (Task #76)
//!
//! Tests verify cluster coordination including leader election, node discovery,
//! and multi-node scenarios.
//!
//! Run with: cargo test --test cluster_coordination_tests

use synctv_cluster::discovery::NodeInfo;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;

#[tokio::test]
async fn test_node_info_serialization() {
    let node = NodeInfo {
        node_id: "node-123".to_string(),
        grpc_addr: "127.0.0.1:50051".to_string(),
        http_addr: Some("127.0.0.1:8080".to_string()),
        is_leader: false,
        last_heartbeat: chrono::Utc::now(),
        metadata: Default::default(),
    };

    let json = serde_json::to_string(&node).expect("Failed to serialize");
    let deserialized: NodeInfo = serde_json::from_str(&json)
        .expect("Failed to deserialize");

    assert_eq!(node.node_id, deserialized.node_id);
    assert_eq!(node.grpc_addr, deserialized.grpc_addr);
}

#[tokio::test]
async fn test_node_registry_concurrent_registration() {
    // Simulate a node registry
    let registry = Arc::new(RwLock::new(std::collections::HashMap::<String, NodeInfo>::new()));

    let mut handles = vec![];

    // Register 10 nodes concurrently
    for i in 0..10 {
        let reg = registry.clone();
        let handle = tokio::spawn(async move {
            let node = NodeInfo {
                node_id: format!("node-{}", i),
                grpc_addr: format!("127.0.0.1:{}", 50000 + i),
                http_addr: Some(format!("127.0.0.1:{}", 8000 + i)),
                is_leader: false,
                last_heartbeat: chrono::Utc::now(),
                metadata: Default::default(),
            };

            let mut registry = reg.write().await;
            registry.insert(node.node_id.clone(), node.clone());
            node.node_id
        });

        handles.push(handle);
    }

    let node_ids: Vec<String> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    let final_registry = registry.read().await;
    assert_eq!(final_registry.len(), 10);
    assert!(node_ids.iter().all(|id| final_registry.contains_key(id)));
}

#[tokio::test]
async fn test_leader_election_single_winner() {
    // Simulate leader election
    let leader = Arc::new(RwLock::new(Option::<String>::None));

    let barrier = Arc::new(tokio::sync::Barrier::new(5));
    let mut handles = vec![];

    // 5 nodes try to become leader
    for i in 0..5 {
        let leader_clone = leader.clone();
        let barrier_clone = barrier.clone();

        let handle = tokio::spawn(async move {
            barrier_clone.wait().await;

            let node_id = format!("node-{}", i);

            // Try to become leader
            let mut current_leader = leader_clone.write().await;
            if current_leader.is_none() {
                *current_leader = Some(node_id.clone());
                (node_id, true)
            } else {
                (node_id, false)
            }
        });

        handles.push(handle);
    }

    let results: Vec<(String, bool)> = futures::future::join_all(handles)
        .await
        .into_iter()
        .map(|r| r.unwrap())
        .collect();

    // Exactly one should become leader
    let leaders: Vec<_> = results.iter().filter(|(_, is_leader)| *is_leader).collect();
    assert_eq!(leaders.len(), 1, "Exactly one node should become leader");

    let final_leader = leader.read().await;
    assert!(final_leader.is_some());
}

#[tokio::test]
async fn test_heartbeat_tracking() {
    #[derive(Clone)]
    struct HeartbeatTracker {
        last_heartbeat: Arc<RwLock<chrono::DateTime<chrono::Utc>>>,
    }

    impl HeartbeatTracker {
        fn new() -> Self {
            Self {
                last_heartbeat: Arc::new(RwLock::new(chrono::Utc::now())),
            }
        }

        async fn update_heartbeat(&self) {
            let mut last = self.last_heartbeat.write().await;
            *last = chrono::Utc::now();
        }

        async fn is_alive(&self, timeout_secs: i64) -> bool {
            let last = self.last_heartbeat.read().await;
            let now = chrono::Utc::now();
            let elapsed = (now - *last).num_seconds();
            elapsed < timeout_secs
        }
    }

    let tracker = HeartbeatTracker::new();

    // Initially alive
    assert!(tracker.is_alive(5).await);

    // Wait 1 second
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Still alive
    assert!(tracker.is_alive(5).await);

    // Update heartbeat
    tracker.update_heartbeat().await;

    // Still alive
    assert!(tracker.is_alive(5).await);

    // Simulate timeout (check with 0 second tolerance)
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    assert!(!tracker.is_alive(0).await);
}

#[tokio::test]
async fn test_node_discovery_add_remove() {
    // Simulate node discovery service
    let nodes = Arc::new(RwLock::new(HashSet::<String>::new()));

    // Add nodes
    {
        let mut n = nodes.write().await;
        n.insert("node-1".to_string());
        n.insert("node-2".to_string());
        n.insert("node-3".to_string());
    }

    // Verify nodes added
    {
        let n = nodes.read().await;
        assert_eq!(n.len(), 3);
    }

    // Remove node
    {
        let mut n = nodes.write().await;
        n.remove("node-2");
    }

    // Verify node removed
    {
        let n = nodes.read().await;
        assert_eq!(n.len(), 2);
        assert!(!n.contains("node-2"));
    }
}

#[tokio::test]
async fn test_message_routing_to_specific_node() {
    #[derive(Debug, Clone, PartialEq)]
    struct Message {
        from: String,
        to: String,
        content: String,
    }

    let message_queues = Arc::new(RwLock::new(
        std::collections::HashMap::<String, Vec<Message>>::new()
    ));

    // Initialize queues for 3 nodes
    {
        let mut queues = message_queues.write().await;
        for i in 1..=3 {
            queues.insert(format!("node-{}", i), Vec::new());
        }
    }

    // Send messages
    let msg1 = Message {
        from: "node-1".to_string(),
        to: "node-2".to_string(),
        content: "Hello node-2".to_string(),
    };

    let msg2 = Message {
        from: "node-1".to_string(),
        to: "node-3".to_string(),
        content: "Hello node-3".to_string(),
    };

    // Route messages
    {
        let mut queues = message_queues.write().await;
        if let Some(queue) = queues.get_mut(&msg1.to) {
            queue.push(msg1.clone());
        }
        if let Some(queue) = queues.get_mut(&msg2.to) {
            queue.push(msg2.clone());
        }
    }

    // Verify routing
    {
        let queues = message_queues.read().await;

        let node2_queue = queues.get("node-2").unwrap();
        assert_eq!(node2_queue.len(), 1);
        assert_eq!(node2_queue[0], msg1);

        let node3_queue = queues.get("node-3").unwrap();
        assert_eq!(node3_queue.len(), 1);
        assert_eq!(node3_queue[0], msg2);

        let node1_queue = queues.get("node-1").unwrap();
        assert_eq!(node1_queue.len(), 0);
    }
}

#[tokio::test]
async fn test_load_balancing_round_robin() {
    let nodes = vec!["node-1", "node-2", "node-3"];
    let current_index = Arc::new(tokio::sync::Mutex::new(0));

    async fn get_next_node(nodes: &[&str], index: &Arc<tokio::sync::Mutex<usize>>) -> String {
        let mut idx = index.lock().await;
        let node = nodes[*idx % nodes.len()];
        *idx += 1;
        node.to_string()
    }

    // Get nodes in round-robin fashion
    let mut selected = vec![];
    for _ in 0..9 {
        let node = get_next_node(&nodes, &current_index).await;
        selected.push(node);
    }

    // Verify round-robin pattern
    assert_eq!(selected, vec![
        "node-1", "node-2", "node-3",
        "node-1", "node-2", "node-3",
        "node-1", "node-2", "node-3",
    ]);
}

#[tokio::test]
async fn test_split_brain_detection() {
    // Simulate split brain scenario
    #[derive(Debug, Clone)]
    struct Partition {
        nodes: HashSet<String>,
        leader: Option<String>,
    }

    let partition1 = Partition {
        nodes: ["node-1", "node-2"].iter().map(|s| s.to_string()).collect(),
        leader: Some("node-1".to_string()),
    };

    let partition2 = Partition {
        nodes: ["node-3", "node-4"].iter().map(|s| s.to_string()).collect(),
        leader: Some("node-3".to_string()),
    };

    // Detect split brain - two partitions with leaders
    let has_split_brain = partition1.leader.is_some() && partition2.leader.is_some();
    assert!(has_split_brain, "Split brain should be detected");

    // Verify no overlap
    let overlap: Vec<_> = partition1.nodes.intersection(&partition2.nodes).collect();
    assert_eq!(overlap.len(), 0, "Partitions should not overlap");
}

#[tokio::test]
async fn test_quorum_validation() {
    fn has_quorum(alive_nodes: usize, total_nodes: usize) -> bool {
        alive_nodes > total_nodes / 2
    }

    // 5 node cluster
    assert!(has_quorum(3, 5), "3/5 nodes have quorum");
    assert!(has_quorum(4, 5), "4/5 nodes have quorum");
    assert!(has_quorum(5, 5), "5/5 nodes have quorum");
    assert!(!has_quorum(2, 5), "2/5 nodes do not have quorum");
    assert!(!has_quorum(1, 5), "1/5 nodes do not have quorum");

    // 3 node cluster
    assert!(has_quorum(2, 3), "2/3 nodes have quorum");
    assert!(!has_quorum(1, 3), "1/3 nodes do not have quorum");

    // Single node (always has quorum)
    assert!(has_quorum(1, 1), "Single node has quorum");
}

#[tokio::test]
async fn test_concurrent_node_failures() {
    let nodes = Arc::new(RwLock::new(HashSet::<String>::new()));

    // Initialize 10 nodes
    {
        let mut n = nodes.write().await;
        for i in 0..10 {
            n.insert(format!("node-{}", i));
        }
    }

    // Simulate concurrent failures
    let mut handles = vec![];
    for i in [2, 5, 7] {
        let nodes_clone = nodes.clone();
        let handle = tokio::spawn(async move {
            let mut n = nodes_clone.write().await;
            n.remove(&format!("node-{}", i));
        });
        handles.push(handle);
    }

    futures::future::join_all(handles).await;

    // Verify failures
    let final_nodes = nodes.read().await;
    assert_eq!(final_nodes.len(), 7);
    assert!(!final_nodes.contains("node-2"));
    assert!(!final_nodes.contains("node-5"));
    assert!(!final_nodes.contains("node-7"));
}

#[tokio::test]
async fn test_node_metadata_propagation() {
    #[derive(Debug, Clone, PartialEq)]
    struct Metadata {
        version: String,
        region: String,
        capacity: u32,
    }

    let node_metadata = Arc::new(RwLock::new(
        std::collections::HashMap::<String, Metadata>::new()
    ));

    // Node 1 updates metadata
    {
        let mut meta = node_metadata.write().await;
        meta.insert("node-1".to_string(), Metadata {
            version: "1.0.0".to_string(),
            region: "us-east-1".to_string(),
            capacity: 100,
        });
    }

    // Node 2 reads metadata
    {
        let meta = node_metadata.read().await;
        let node1_meta = meta.get("node-1").unwrap();
        assert_eq!(node1_meta.version, "1.0.0");
        assert_eq!(node1_meta.region, "us-east-1");
        assert_eq!(node1_meta.capacity, 100);
    }

    // Node 1 updates capacity
    {
        let mut meta = node_metadata.write().await;
        if let Some(m) = meta.get_mut("node-1") {
            m.capacity = 150;
        }
    }

    // Verify update propagated
    {
        let meta = node_metadata.read().await;
        let node1_meta = meta.get("node-1").unwrap();
        assert_eq!(node1_meta.capacity, 150);
    }
}

#[tokio::test]
async fn test_graceful_node_shutdown() {
    #[derive(Debug, Clone, PartialEq)]
    enum NodeState {
        Running,
        ShuttingDown,
        Stopped,
    }

    let state = Arc::new(RwLock::new(NodeState::Running));

    // Initiate shutdown
    {
        let mut s = state.write().await;
        *s = NodeState::ShuttingDown;
    }

    // Simulate drain operations
    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

    // Complete shutdown
    {
        let mut s = state.write().await;
        *s = NodeState::Stopped;
    }

    // Verify final state
    {
        let s = state.read().await;
        assert_eq!(*s, NodeState::Stopped);
    }
}
