use std::sync::Arc;

use crate::{repository::realtime_outbox::NewRealtimeOutboxEvent, Result};

use super::{DeleteEntriesPlan, EntryDeletionImpact, RoomService};

pub type RealtimeOutboxDeleteEntriesEventFactory =
    Arc<dyn Fn(&DeleteEntriesPlan) -> Result<Vec<NewRealtimeOutboxEvent>> + Send + Sync>;

impl RoomService {
    fn committed_delete_entries_plan(impact: &EntryDeletionImpact) -> DeleteEntriesPlan {
        DeleteEntriesPlan {
            deleted_playlist_ids: impact.deleted_playlist_ids.clone(),
            deleted_media_ids: impact.deleted_media_ids.clone(),
            playback_reset: impact.playback_reset,
            playback_state: impact.playback_state.clone(),
        }
    }

    pub(super) async fn insert_delete_entries_outbox_events_tx(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        impact: &EntryDeletionImpact,
        outbox_event_factory: Option<&RealtimeOutboxDeleteEntriesEventFactory>,
    ) -> Result<()> {
        let Some(outbox) = &self.realtime_outbox else {
            return Ok(());
        };
        let Some(factory) = outbox_event_factory else {
            return Ok(());
        };

        let committed_plan = Self::committed_delete_entries_plan(impact);
        for event in factory(&committed_plan)? {
            outbox.insert_with_executor(&event, &mut **tx).await?;
        }
        Ok(())
    }
}
