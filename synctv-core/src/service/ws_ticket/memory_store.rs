use async_trait::async_trait;

use crate::Result;

use super::{TicketStore, WsTicketData, now_unix_seconds};

mod claim;

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

    fn expire_after_update(
        &self,
        _key: &String,
        value: &TtlTicketData,
        _updated_at: std::time::Instant,
        _remaining: Option<std::time::Duration>,
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
    pub fn new() -> Self {
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
            .entry_by_ref(ticket)
            .and_upsert_with(|_| async {
                TtlTicketData {
                    data: data.clone(),
                    ttl: std::time::Duration::from_secs(ttl_secs),
                }
            })
            .await;
        Ok(())
    }

    async fn load(&self, ticket: &str) -> Result<Option<WsTicketData>> {
        let Some(entry) = self.cache.get(ticket).await else {
            return Ok(None);
        };

        let now = now_unix_seconds();
        if !claim::within_ttl(entry.data.created_at, now, entry.ttl.as_secs()) {
            return Ok(None);
        }

        Ok(Some(entry.data))
    }

    async fn claim(&self, ticket: &str, expected_ticket: &WsTicketData) -> Result<bool> {
        use moka::ops::compute::{CompResult, Op};
        let result = self
            .cache
            .entry_by_ref(ticket)
            .and_compute_with(|entry| async move {
                if let Some(entry) = entry {
                    let value = entry.value();
                    if claim::may_consume(
                        value.data == *expected_ticket,
                        value.data.created_at,
                        now_unix_seconds(),
                        value.ttl.as_secs(),
                    ) {
                        return Op::Remove;
                    }
                }
                Op::Nop
            })
            .await;
        Ok(matches!(result, CompResult::Removed(_)))
    }

    fn supports_cluster_runtime(&self) -> bool {
        false
    }
}
