use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::sync::{Mutex, MutexGuard};
use synctv_core::models::{RoomId, UserId};
use synctv_core::service::{
    BruteForceProtection, InMemoryTokenBlacklistStore, JwtService, RateLimiter, UserService,
};
use synctv_core::{
    cache::{KeyBuilder, UsernameCache},
    service::{JwtValidator, SecurityPipeline},
};
use synctv_realtime::sync::{
    BroadcastResult, ConnectionId, PublishRequest, RealtimeEvent, SharedRealtimeEvent,
};
use tokio::sync::{broadcast, mpsc};

use crate::impls::{AdminApiRuntime, AdminReadServices, ClientApiRuntime, RequestExecutor};
use crate::realtime_fanout::ChannelRealtimeFanoutService;
use synctv_realtime::fanout::{RealtimeEventService, RealtimeFanoutService, RealtimeMetrics};

fn lock_or_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn channel_realtime_fanout_service(
    sender: mpsc::Sender<PublishRequest>,
) -> std::sync::Arc<dyn RealtimeFanoutService> {
    std::sync::Arc::new(ChannelRealtimeFanoutService { sender })
}

pub fn local_request_executor() -> RequestExecutor {
    let jwt_service = JwtService::new("test-request-executor-secret-minimum-32-chars")
        .expect("test JWT service should build");
    let pool = sqlx::postgres::PgPoolOptions::new()
        .connect_lazy("postgres://synctv:synctv@127.0.0.1:5432/synctv_test")
        .expect("test pool should build lazily");
    let user_service = Arc::new(UserService::new_for_tests(
        &pool,
        jwt_service.clone(),
        UsernameCache::local_only("test:request-executor:user:".to_string(), 100, 60),
        Arc::new(InMemoryTokenBlacklistStore::new(1000, 3600, 86400)),
        KeyBuilder::new("test:request-executor"),
        BruteForceProtection::in_memory("test:request-executor:brute:".to_string()),
    ));
    RequestExecutor::new(
        Arc::new(crate::ApiRuntimeSettings::default()),
        Arc::new(JwtValidator::new(Arc::new(jwt_service))),
        Arc::new(SecurityPipeline::new(&user_service)),
        Arc::new(RateLimiter::local_only(
            "test:request-executor:rate:".to_string(),
        )),
    )
}

pub fn proxy_signing_key(seed: &'static [u8]) -> Arc<crate::proxy_signature::ProxySigningKey> {
    Arc::new(
        crate::proxy_signature::ProxySigningKey::try_derive_from(seed)
            .expect("test signing key should derive"),
    )
}

pub fn admin_read_services(user_service: &UserService) -> AdminReadServices {
    let write_pool = user_service.pool().clone();
    let read_pool = user_service.eventually_consistent_pool().clone();
    AdminReadServices {
        system_stats_service: Arc::new(synctv_core::service::SystemStatsService::new(
            read_pool.clone(),
        )),
        review_service: Arc::new(synctv_core::service::ReviewService::new_with_read_pool(
            write_pool.clone(),
            read_pool.clone(),
        )),
        ban_record_service: Arc::new(synctv_core::service::BanRecordService::new_with_read_pool(
            write_pool.clone(),
            read_pool.clone(),
        )),
        content_report_service: Arc::new(
            synctv_core::service::ContentReportService::new_with_read_pool(write_pool, read_pool),
        ),
    }
}

pub fn client_api_runtime() -> ClientApiRuntime {
    ClientApiRuntime::local_disabled(
        Arc::new(local_request_executor()),
        proxy_signing_key(b"test-client-api-runtime-signing-key-32-bytes"),
    )
}

#[allow(dead_code)]
pub fn admin_api_runtime() -> AdminApiRuntime {
    AdminApiRuntime::local_disabled(
        Arc::new(local_request_executor()),
        proxy_signing_key(b"test-admin-api-runtime-signing-key-32-bytes!"),
        Arc::new(synctv_core::provider::ProviderStoreRegistry::local_only(
            "test:admin:",
        )),
    )
}

#[derive(Default)]
pub struct RecordingRealtimeEventService {
    pub broadcast_calls: AtomicUsize,
    pub publish_only_calls: AtomicUsize,
    pub broadcast_events: Mutex<Vec<RealtimeEvent>>,
    pub room_calls: AtomicUsize,
    pub admin_calls: AtomicUsize,
    pub room_events: Mutex<Vec<(String, RealtimeEvent)>>,
    pub admin_events: Mutex<Vec<RealtimeEvent>>,
    pub distributed_enabled: bool,
    node_id: String,
}

impl RecordingRealtimeEventService {
    pub fn with_node(node_id: impl Into<String>, distributed_enabled: bool) -> Self {
        Self {
            node_id: node_id.into(),
            distributed_enabled,
            ..Self::default()
        }
    }

    pub fn room_event_count(&self) -> usize {
        lock_or_recover(&self.room_events).len()
    }

    pub fn room_events(&self) -> Vec<RealtimeEvent> {
        lock_or_recover(&self.room_events)
            .iter()
            .map(|(_, event)| event.clone())
            .collect()
    }

    pub fn admin_events(&self) -> Vec<RealtimeEvent> {
        lock_or_recover(&self.admin_events).clone()
    }

    pub fn broadcast_events(&self) -> Vec<RealtimeEvent> {
        lock_or_recover(&self.broadcast_events).clone()
    }
}

#[async_trait]
impl RealtimeEventService for RecordingRealtimeEventService {
    async fn subscribe_with_id(
        &self,
        _room_id: RoomId,
        _user_id: UserId,
        connection_id: ConnectionId,
    ) -> synctv_realtime::Result<(mpsc::Receiver<SharedRealtimeEvent>, ConnectionId)> {
        let (_tx, rx) = mpsc::channel(16);
        Ok((rx, connection_id))
    }

    fn unsubscribe(&self, _connection_id: &str) {}

    fn broadcast(&self, event: RealtimeEvent) -> BroadcastResult {
        self.broadcast_calls.fetch_add(1, Ordering::SeqCst);
        lock_or_recover(&self.broadcast_events).push(event);
        BroadcastResult {
            local_sent: 1,
            redis_sent: false,
        }
    }

    fn publish_only(&self, _event: RealtimeEvent) -> bool {
        self.publish_only_calls.fetch_add(1, Ordering::SeqCst);
        false
    }

    fn broadcast_local(&self, room_id: &RoomId, event: &RealtimeEvent) -> usize {
        self.room_calls.fetch_add(1, Ordering::SeqCst);
        lock_or_recover(&self.room_events).push((room_id.to_string(), event.clone()));
        1
    }

    fn broadcast_admin_local(&self, event: &RealtimeEvent) -> usize {
        self.admin_calls.fetch_add(1, Ordering::SeqCst);
        lock_or_recover(&self.admin_events).push(event.clone());
        1
    }

    fn subscribe_admin_events(&self) -> broadcast::Receiver<RealtimeEvent> {
        let (tx, rx) = broadcast::channel(16);
        drop(tx);
        rx
    }

    fn metrics(&self) -> RealtimeMetrics {
        RealtimeMetrics {
            distributed_enabled: self.distributed_enabled,
        }
    }

    fn node_id(&self) -> &str {
        if self.node_id.is_empty() {
            "recording-realtime-event-service"
        } else {
            &self.node_id
        }
    }

    async fn shutdown(&self) {}
}
