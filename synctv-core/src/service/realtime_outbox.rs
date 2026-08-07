use std::sync::Arc;

use crate::{
    repository::realtime_outbox::{NewRealtimeOutboxEvent, RealtimeOutboxRepository},
    Result,
};

#[derive(Clone)]
pub struct RealtimeOutboxService {
    repository: Arc<RealtimeOutboxRepository>,
}

impl RealtimeOutboxService {
    #[must_use]
    pub const fn new(repository: Arc<RealtimeOutboxRepository>) -> Self {
        Self { repository }
    }

    pub async fn insert(&self, event: &NewRealtimeOutboxEvent) -> Result<()> {
        self.repository.insert(event).await
    }
}
