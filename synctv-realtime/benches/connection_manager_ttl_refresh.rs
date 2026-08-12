#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use criterion::{criterion_group, criterion_main, Criterion};
#[path = "../tests/integration_test_helpers.rs"]
mod integration_test_helpers;

use integration_test_helpers::TestRedis;
use synctv_core::models::id::{RoomId, UserId};
use synctv_core::service::OnlinePresenceService;
use synctv_core::SharedStateProfile;
use synctv_realtime::sync::{build_connection_manager, ConnectionLimits};

fn uid(s: &str) -> UserId {
    s.parse().expect("valid numeric user id")
}

fn rid(s: &str) -> RoomId {
    s.parse().expect("valid numeric room id")
}

async fn setup_redis() -> (TestRedis, redis::aio::ConnectionManager) {
    let redis = TestRedis::start().await;
    let client = redis::Client::open(redis.redis_url.as_str()).unwrap();
    let conn = client.get_connection_manager().await.unwrap();
    (redis, conn)
}

fn bench_ttl_refresh_large_scale(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("connection_manager_ttl_refresh_large_scale", |b| {
        b.to_async(&rt).iter(|| async {
            let (_redis, conn) = setup_redis().await;

            let limits = ConnectionLimits {
                max_total: 5000,
                max_per_user: 1000,
                max_per_room: 5000,
                ..Default::default()
            };
            let manager = build_connection_manager(
                limits,
                &SharedStateProfile::for_cluster_runtime(
                    Some(synctv_core::direct_runtime(conn.clone())),
                    "ttl_large:",
                    true,
                ),
                Arc::new(OnlinePresenceService::local()),
                "ttl-refresh-bench",
            )
            .expect("shared realtime connection runtime should initialize");

            let num_connections = 1000;
            let num_users = 100;
            let num_rooms = 10;

            for i in 0..num_connections {
                let user_idx = i % num_users;
                let room_idx = i % num_rooms;
                let conn_id = format!("conn_{i}");
                let user_id = uid(&(user_idx + 1).to_string());
                let room_id = rid(&(room_idx + 1).to_string());

                manager.register(conn_id.clone(), user_id).await.unwrap();
                manager.join_room(&conn_id, room_id).await.unwrap();
            }

            let mut test_conn = conn.clone();
            for i in 0..num_users {
                let key = format!("ttl_large:connections:actor:user:{}", i + 1);
                let _: () = redis::cmd("EXPIRE")
                    .arg(&key)
                    .arg(10)
                    .query_async(&mut test_conn)
                    .await
                    .unwrap();
            }
            for i in 0..num_rooms {
                let key = format!("ttl_large:connections:room:{}", i + 1);
                let _: () = redis::cmd("EXPIRE")
                    .arg(&key)
                    .arg(10)
                    .query_async(&mut test_conn)
                    .await
                    .unwrap();
            }

            let refresh_start = Instant::now();
            manager.test_refresh_distributed_counter_ttls().await;
            let refresh_time = refresh_start.elapsed();

            let total_key = "ttl_large:connections:total";
            let ttl: i64 = redis::cmd("TTL")
                .arg(total_key)
                .query_async(&mut test_conn)
                .await
                .unwrap();
            assert!(ttl > 10);
            assert!(refresh_time < Duration::from_secs(30));
        });
    });
}

criterion_group!(benches, bench_ttl_refresh_large_scale);
criterion_main!(benches);
