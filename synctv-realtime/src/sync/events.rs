use super::RealtimeEvent;

/// Helper to publish realtime events to the admin broadcast channel with logging.
pub(super) fn publish_admin_event(
    admin_event_tx: &tokio::sync::broadcast::Sender<RealtimeEvent>,
    event: RealtimeEvent,
    context: &str,
) {
    use tracing::{debug, warn};

    let event_type = event.event_type();
    let room_id = event.room_id().map(ToString::to_string);
    match admin_event_tx.send(event) {
        Ok(receiver_count) => {
            debug!(
                event_type = %event_type,
                room_id = room_id.as_deref().unwrap_or("n/a"),
                receiver_count,
                context,
                "Published realtime admin event"
            );
        }
        Err(error) => {
            warn!(
                event_type = %event_type,
                room_id = room_id.as_deref().unwrap_or("n/a"),
                error = %error,
                context,
                "Failed to publish realtime admin event"
            );
        }
    }
}
