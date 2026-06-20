use async_trait::async_trait;

use crate::models::RoomId;
use crate::Result;

use super::{now_unix_seconds, TicketStore, WsTicketData};

/// Wrapper that pairs ticket data with its per-entry TTL for moka's `Expiry` trait.
#[derive(Clone)]
struct TtlTicketData {
    data: WsTicketData,
    ttl: std::time::Duration,
}

/// Moka `Expiry` implementation that uses the per-entry TTL.
struct TicketEntryExpiry;

impl moka::Expiry<String, TtlTicketData> for TicketEntryExpiry {
    fn expire_after_create(
        &self,
        _key: &String,
        value: &TtlTicketData,
        _now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        Some(value.ttl)
    }
}

/// In-memory ticket store for single-replica deployments using moka cache with per-entry TTL.
pub struct InMemoryTicketStore {
    cache: moka::future::Cache<String, TtlTicketData>,
}

impl InMemoryTicketStore {
    #[must_use]
    pub fn new(_ttl_secs: u64) -> Self {
        Self {
            cache: moka::future::Cache::builder()
                .expire_after(TicketEntryExpiry)
                .max_capacity(10_000)
                .build(),
        }
    }
}

#[async_trait]
impl TicketStore for InMemoryTicketStore {
    async fn store(&self, ticket: &str, data: &WsTicketData, ttl_secs: u64) -> Result<()> {
        self.cache
            .insert(
                ticket.to_string(),
                TtlTicketData {
                    data: data.clone(),
                    ttl: std::time::Duration::from_secs(ttl_secs),
                },
            )
            .await;
        Ok(())
    }

    async fn load(&self, ticket: &str, _expected_room_id: &RoomId) -> Result<Option<WsTicketData>> {
        let Some(entry) = self.cache.get(ticket).await else {
            return Ok(None);
        };

        let now = now_unix_seconds();
        if now.saturating_sub(entry.data.created_at) > entry.ttl.as_secs() {
            self.cache.remove(ticket).await;
            return Ok(None);
        }

        Ok(Some(entry.data))
    }

    async fn claim(
        &self,
        ticket: &str,
        _expected_room_id: &RoomId,
        expected_ticket: &WsTicketData,
    ) -> Result<bool> {
        let Some(entry) = self.cache.get(ticket).await else {
            return Ok(false);
        };
        let now = now_unix_seconds();
        if now.saturating_sub(entry.data.created_at) > entry.ttl.as_secs() {
            self.cache.remove(ticket).await;
            return Ok(false);
        }
        if entry.data != *expected_ticket {
            return Ok(false);
        }

        let Some(removed) = self.cache.remove(ticket).await else {
            return Ok(false);
        };
        if now.saturating_sub(removed.data.created_at) > removed.ttl.as_secs() {
            return Ok(false);
        }

        Ok(removed.data == *expected_ticket)
    }

    fn supports_cluster_runtime(&self) -> bool {
        false
    }
}
