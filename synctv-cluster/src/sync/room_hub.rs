use dashmap::DashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use synctv_core::models::id::{RoomId, UserId};
use tokio::sync::{broadcast, mpsc};
use tracing::{debug, info, warn};

use super::events::ClusterEvent;

/// Notification about room lifecycle changes (first subscriber / last unsubscribe).
/// Sent to the Redis Pub/Sub subscriber task so it can dynamically subscribe/unsubscribe
/// to specific room channels instead of using a global `psubscribe("synctv:room:*")`.
#[derive(Debug, Clone)]
pub enum RoomLifecycleEvent {
    /// First subscriber joined this room on the local node.
    RoomActivated(RoomId),
    /// Last subscriber left this room on the local node.
    RoomDeactivated(RoomId),
}

/// Capacity for the room lifecycle broadcast channel.
/// Large enough to avoid drops during room churn; events are small.
const LIFECYCLE_CHANNEL_CAPACITY: usize = 1024;

/// Handle for a client connection subscription
pub type ConnectionId = String;

/// Capacity for per-subscriber message channels.
/// Messages are dropped with a warning when a subscriber is too slow.
const SUBSCRIBER_CHANNEL_CAPACITY: usize = 256;

/// Number of consecutive drops before automatically disconnecting a slow subscriber.
const MAX_CONSECUTIVE_DROPS: u32 = 10;

/// Message sender for a client connection
pub type MessageSender = mpsc::Sender<ClusterEvent>;

/// Subscriber information
#[derive(Debug)]
pub struct Subscriber {
    pub connection_id: ConnectionId,
    pub user_id: UserId,
    pub sender: MessageSender,
    /// Consecutive message drops due to a full channel
    consecutive_drops: Arc<AtomicU32>,
}

impl Clone for Subscriber {
    fn clone(&self) -> Self {
        Self {
            connection_id: self.connection_id.clone(),
            user_id: self.user_id.clone(),
            sender: self.sender.clone(),
            consecutive_drops: self.consecutive_drops.clone(),
        }
    }
}

/// In-memory hub for routing messages to connected clients in rooms
/// This handles local message distribution (single node)
#[derive(Clone, Debug)]
pub struct RoomMessageHub {
    /// Map of `room_id` -> list of subscribers
    rooms: Arc<DashMap<RoomId, Vec<Subscriber>>>,

    /// Map of `connection_id` -> (`room_id`, `user_id`) for cleanup
    connections: Arc<DashMap<ConnectionId, (RoomId, UserId)>>,

    /// Broadcast sender for room lifecycle events (first join / last leave).
    /// The Redis Pub/Sub subscriber task listens on a receiver to dynamically
    /// subscribe/unsubscribe to specific room channels.
    lifecycle_tx: broadcast::Sender<RoomLifecycleEvent>,
}

impl RoomMessageHub {
    /// Create a new `RoomMessageHub`
    #[must_use]
    pub fn new() -> Self {
        let (lifecycle_tx, _) = broadcast::channel(LIFECYCLE_CHANNEL_CAPACITY);
        Self {
            rooms: Arc::new(DashMap::new()),
            connections: Arc::new(DashMap::new()),
            lifecycle_tx,
        }
    }

    /// Subscribe to room lifecycle events (room activated / deactivated).
    /// Used by the Redis Pub/Sub subscriber task.
    #[must_use]
    pub fn subscribe_lifecycle(&self) -> broadcast::Receiver<RoomLifecycleEvent> {
        self.lifecycle_tx.subscribe()
    }

    /// Subscribe a client to room events
    /// Returns a receiver for messages
    pub fn subscribe(
        &self,
        room_id: RoomId,
        user_id: UserId,
        connection_id: ConnectionId,
    ) -> mpsc::Receiver<ClusterEvent> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_CAPACITY);

        let subscriber = Subscriber {
            connection_id: connection_id.clone(),
            user_id: user_id.clone(),
            sender: tx,
            consecutive_drops: Arc::new(AtomicU32::new(0)),
        };

        // Atomically check-and-insert using DashMap's entry API.
        // This avoids the TOCTOU race between `contains_key` + `entry().or_default()`
        // where two concurrent subscribes could both see the room as new.
        let is_new_room = match self.rooms.entry(room_id.clone()) {
            dashmap::mapref::entry::Entry::Occupied(mut entry) => {
                entry.get_mut().push(subscriber);
                false
            }
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(vec![subscriber]);
                true
            }
        };

        // Track connection for cleanup
        self.connections
            .insert(connection_id.clone(), (room_id.clone(), user_id.clone()));

        // Emit lifecycle event if this is the first subscriber for the room
        if is_new_room {
            let _ = self.lifecycle_tx.send(RoomLifecycleEvent::RoomActivated(room_id.clone()));
        }

        info!(
            room_id = %room_id.as_str(),
            user_id = %user_id.as_str(),
            connection_id = %connection_id,
            "Client subscribed to room"
        );

        rx
    }

    /// Unsubscribe a client from room events
    pub fn unsubscribe(&self, connection_id: &str) {
        if let Some((_, (room_id, user_id))) = self.connections.remove(connection_id) {
            let mut room_deactivated = false;

            // Atomically remove subscriber and clean up empty room using
            // DashMap's `remove_if` to avoid the TOCTOU race between checking
            // `is_empty()`, dropping the guard, and calling `remove()`. A
            // concurrent `subscribe` could insert a new subscriber between
            // the guard drop and the remove, causing the new subscriber to
            // be silently lost.
            //
            // We first retain to remove the target subscriber, then use
            // `remove_if` which holds the shard lock while checking emptiness
            // and removing the entry atomically.
            if let Some(mut subscribers) = self.rooms.get_mut(&room_id) {
                subscribers.retain(|sub| sub.connection_id != connection_id);
                // We must drop the RefMut before calling remove_if, since both
                // acquire the same shard lock and would deadlock.
                drop(subscribers);
            }

            // Atomically remove the room entry only if it's still empty.
            // If a concurrent subscribe added a new subscriber between the
            // retain and this call, the entry won't be empty and won't be removed.
            if self.rooms.remove_if(&room_id, |_, subscribers| subscribers.is_empty()).is_some() {
                room_deactivated = true;
                debug!(room_id = %room_id.as_str(), "Room has no more subscribers, removed");
            }

            // Emit lifecycle event if the last subscriber left
            if room_deactivated {
                let _ = self.lifecycle_tx.send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()));
            }

            info!(
                room_id = %room_id.as_str(),
                user_id = %user_id.as_str(),
                connection_id = %connection_id,
                "Client unsubscribed from room"
            );
        } else {
            warn!(
                connection_id = %connection_id,
                "Attempted to unsubscribe unknown connection"
            );
        }
    }

    /// Broadcast an event to all subscribers in a room.
    ///
    /// Subscribers that fail to receive messages for `MAX_CONSECUTIVE_DROPS`
    /// consecutive broadcasts are automatically disconnected to prevent
    /// unbounded backpressure from a single slow client.
    pub fn broadcast(&self, room_id: &RoomId, event: ClusterEvent) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();

        if let Some(subscribers) = self.rooms.get(room_id) {
            for subscriber in subscribers.iter() {
                match subscriber.sender.try_send(event.clone()) {
                    Ok(()) => {
                        // Reset consecutive drop counter on successful send
                        subscriber.consecutive_drops.store(0, Ordering::Relaxed);
                        sent_count += 1;
                        debug!(
                            room_id = %room_id.as_str(),
                            user_id = %subscriber.user_id.as_str(),
                            connection_id = %subscriber.connection_id,
                            event_type = %event.event_type(),
                            "Event sent to client"
                        );
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        let drops = subscriber.consecutive_drops.fetch_add(1, Ordering::Relaxed) + 1;
                        if drops >= MAX_CONSECUTIVE_DROPS {
                            warn!(
                                room_id = %room_id.as_str(),
                                user_id = %subscriber.user_id.as_str(),
                                connection_id = %subscriber.connection_id,
                                consecutive_drops = drops,
                                "Disconnecting persistently slow subscriber after {} consecutive drops",
                                MAX_CONSECUTIVE_DROPS
                            );
                            failed_connections.push(subscriber.connection_id.clone());
                        } else {
                            warn!(
                                room_id = %room_id.as_str(),
                                user_id = %subscriber.user_id.as_str(),
                                connection_id = %subscriber.connection_id,
                                event_type = %event.event_type(),
                                consecutive_drops = drops,
                                "Subscriber channel full, dropping event for slow consumer"
                            );
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        warn!(
                            room_id = %room_id.as_str(),
                            user_id = %subscriber.user_id.as_str(),
                            connection_id = %subscriber.connection_id,
                            "Subscriber channel closed, marking for cleanup"
                        );
                        failed_connections.push(subscriber.connection_id.clone());
                    }
                }
            }
        }

        // Clean up failed/slow connections (drop the read guard first)
        for conn_id in failed_connections {
            self.unsubscribe(&conn_id);
        }

        if sent_count > 0 {
            debug!(
                room_id = %room_id.as_str(),
                sent_count = sent_count,
                event_type = %event.event_type(),
                "Event broadcast complete"
            );
        }

        sent_count
    }

    /// Broadcast an event to a specific user in a room
    pub fn broadcast_to_user(
        &self,
        room_id: &RoomId,
        user_id: &UserId,
        event: ClusterEvent,
    ) -> usize {
        let mut sent_count = 0;
        let mut failed_connections = Vec::new();

        if let Some(subscribers) = self.rooms.get(room_id) {
            for subscriber in subscribers.iter() {
                if subscriber.user_id == *user_id {
                    match subscriber.sender.try_send(event.clone()) {
                        Ok(()) => {
                            sent_count += 1;
                            debug!(
                                room_id = %room_id.as_str(),
                                user_id = %subscriber.user_id.as_str(),
                                connection_id = %subscriber.connection_id,
                                event_type = %event.event_type(),
                                "Event sent to specific user"
                            );
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            warn!(
                                room_id = %room_id.as_str(),
                                user_id = %subscriber.user_id.as_str(),
                                connection_id = %subscriber.connection_id,
                                event_type = %event.event_type(),
                                "Subscriber channel full, dropping event for slow consumer"
                            );
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            warn!(
                                room_id = %room_id.as_str(),
                                user_id = %subscriber.user_id.as_str(),
                                connection_id = %subscriber.connection_id,
                                "Subscriber channel closed, marking for cleanup"
                            );
                            failed_connections.push(subscriber.connection_id.clone());
                        }
                    }
                }
            }
        }

        // Clean up failed connections
        for conn_id in failed_connections {
            self.unsubscribe(&conn_id);
        }

        sent_count
    }

    /// Broadcast an event to a specific connection in a room.
    ///
    /// Used for targeted delivery (e.g., WebRTC signaling to a specific peer).
    /// Returns 1 if sent, 0 if the connection was not found or the channel was full.
    pub fn broadcast_to_connection(
        &self,
        room_id: &RoomId,
        connection_id: &str,
        event: ClusterEvent,
    ) -> usize {
        let mut result = 0;
        let mut failed_connection: Option<ConnectionId> = None;

        if let Some(subscribers) = self.rooms.get(room_id) {
            for subscriber in subscribers.iter() {
                if subscriber.connection_id == connection_id {
                    let event_type = event.event_type().to_string();
                    match subscriber.sender.try_send(event) {
                        Ok(()) => {
                            debug!(
                                room_id = %room_id.as_str(),
                                connection_id = %connection_id,
                                event_type = %event_type,
                                "Event sent to specific connection"
                            );
                            result = 1;
                        }
                        Err(mpsc::error::TrySendError::Full(_)) => {
                            warn!(
                                room_id = %room_id.as_str(),
                                connection_id = %connection_id,
                                "Subscriber channel full, dropping targeted event"
                            );
                        }
                        Err(mpsc::error::TrySendError::Closed(_)) => {
                            warn!(
                                room_id = %room_id.as_str(),
                                connection_id = %connection_id,
                                "Subscriber channel closed for targeted event"
                            );
                            failed_connection = Some(subscriber.connection_id.clone());
                        }
                    }
                    break;
                }
            }
        }
        // Drop the DashMap read guard above before calling unsubscribe(),
        // which takes a write lock, to avoid deadlock on the same shard.

        // Clean up closed connection
        if let Some(conn_id) = failed_connection {
            self.unsubscribe(&conn_id);
        }

        result
    }

    /// Get the number of subscribers in a room
    #[must_use]
    pub fn subscriber_count(&self, room_id: &RoomId) -> usize {
        self.rooms
            .get(room_id)
            .map_or(0, |subscribers| subscribers.len())
    }

    /// Get the number of active rooms
    #[must_use]
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// Get all active room IDs (rooms with at least one subscriber)
    #[must_use]
    pub fn active_room_ids(&self) -> Vec<RoomId> {
        self.rooms.iter().map(|entry| entry.key().clone()).collect()
    }

    /// Get total number of active connections
    #[must_use] 
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Remove all subscribers for a room and clean up connection tracking.
    ///
    /// Used when a room is deleted on another replica: after broadcasting the
    /// `RoomDeleted` event, the hub removes the room so senders are dropped and
    /// WebSocket read loops terminate.
    pub fn remove_room(&self, room_id: &RoomId) {
        if let Some((_, subscribers)) = self.rooms.remove(room_id) {
            for sub in &subscribers {
                self.connections.remove(&sub.connection_id);
            }
            // Emit lifecycle event since the room is no longer active
            let _ = self.lifecycle_tx.send(RoomLifecycleEvent::RoomDeactivated(room_id.clone()));
            info!(
                room_id = %room_id.as_str(),
                removed_connections = subscribers.len(),
                "Removed all subscribers for deleted room"
            );
        }
    }

    /// Get all subscribers in a room (for debugging/monitoring)
    #[must_use]
    pub fn get_room_subscribers(&self, room_id: &RoomId) -> Vec<(UserId, ConnectionId)> {
        self.rooms
            .get(room_id)
            .map(|subscribers| {
                subscribers
                    .iter()
                    .map(|sub| (sub.user_id.clone(), sub.connection_id.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl Default for RoomMessageHub {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    #[tokio::test]
    async fn test_subscribe_and_broadcast() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("test_user".to_string());

        // Subscribe
        let mut rx = hub.subscribe(room_id.clone(), user_id.clone(), "conn1".to_string());

        assert_eq!(hub.subscriber_count(&room_id), 1);
        assert_eq!(hub.connection_count(), 1);

        // Broadcast event
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user_id.clone(),
            username: "testuser".to_string(),
            message: "Hello!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let sent_count = hub.broadcast(&room_id, event.clone());
        assert_eq!(sent_count, 1);

        // Receive event
        let received = rx.recv().await.unwrap();
        assert_eq!(received.event_type(), "chat_message");
    }

    #[tokio::test]
    async fn test_unsubscribe() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("test_user".to_string());

        // Subscribe
        let _rx = hub.subscribe(room_id.clone(), user_id.clone(), "conn1".to_string());
        assert_eq!(hub.subscriber_count(&room_id), 1);

        // Unsubscribe
        hub.unsubscribe("conn1");
        assert_eq!(hub.subscriber_count(&room_id), 0);
        assert_eq!(hub.connection_count(), 0);
        assert_eq!(hub.room_count(), 0);
    }

    #[tokio::test]
    async fn test_multiple_subscribers() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        // Subscribe two clients
        let mut rx1 = hub.subscribe(room_id.clone(), user1.clone(), "conn1".to_string());
        let mut rx2 = hub.subscribe(room_id.clone(), user2.clone(), "conn2".to_string());

        assert_eq!(hub.subscriber_count(&room_id), 2);

        // Broadcast event
        let event = ClusterEvent::ChatMessage {
            event_id: nanoid::nanoid!(16),
            room_id: room_id.clone(),
            user_id: user1.clone(),
            username: "user1".to_string(),
            message: "Hello!".to_string(),
            timestamp: Utc::now(),
            position: None,
            color: None,
        };

        let sent_count = hub.broadcast(&room_id, event.clone());
        assert_eq!(sent_count, 2);

        // Both should receive
        let received1 = rx1.recv().await.unwrap();
        let received2 = rx2.recv().await.unwrap();

        assert_eq!(received1.event_type(), "chat_message");
        assert_eq!(received2.event_type(), "chat_message");
    }

    #[tokio::test]
    async fn test_broadcast_to_specific_user() {
        let hub = RoomMessageHub::new();
        let room_id = RoomId::from_string("test_room".to_string());
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        // Subscribe two clients
        let mut rx1 = hub.subscribe(room_id.clone(), user1.clone(), "conn1".to_string());
        let mut rx2 = hub.subscribe(room_id.clone(), user2.clone(), "conn2".to_string());

        // Broadcast to user1 only
        let event = ClusterEvent::SystemNotification {
            event_id: nanoid::nanoid!(16),
            message: "Private message".to_string(),
            level: crate::sync::NotificationLevel::Info,
            timestamp: Utc::now(),
        };

        let sent_count = hub.broadcast_to_user(&room_id, &user1, event.clone());
        assert_eq!(sent_count, 1);

        // Only user1 should receive
        let received1 = tokio::time::timeout(std::time::Duration::from_millis(100), rx1.recv())
            .await
            .unwrap()
            .unwrap();

        assert_eq!(received1.event_type(), "system_notification");

        // User2 should not receive
        let received2 =
            tokio::time::timeout(std::time::Duration::from_millis(100), rx2.recv()).await;

        assert!(
            received2.is_err(),
            "User2 should not have received the message"
        );
    }

    #[tokio::test]
    async fn test_lifecycle_events_on_subscribe_unsubscribe() {
        let hub = RoomMessageHub::new();
        let mut lifecycle_rx = hub.subscribe_lifecycle();

        let room_id = RoomId::from_string("test_room".to_string());
        let user1 = UserId::from_string("user1".to_string());
        let user2 = UserId::from_string("user2".to_string());

        // First subscriber triggers RoomActivated
        let _rx1 = hub.subscribe(room_id.clone(), user1.clone(), "conn1".to_string());
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(matches!(event, RoomLifecycleEvent::RoomActivated(ref rid) if rid.as_str() == "test_room"));

        // Second subscriber does NOT trigger RoomActivated
        let _rx2 = hub.subscribe(room_id.clone(), user2.clone(), "conn2".to_string());
        assert!(lifecycle_rx.try_recv().is_err());

        // Unsubscribe first user: room still has subscribers, no event
        hub.unsubscribe("conn1");
        assert!(lifecycle_rx.try_recv().is_err());

        // Unsubscribe last user: triggers RoomDeactivated
        hub.unsubscribe("conn2");
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid.as_str() == "test_room"));
    }

    #[tokio::test]
    async fn test_lifecycle_events_on_remove_room() {
        let hub = RoomMessageHub::new();
        let mut lifecycle_rx = hub.subscribe_lifecycle();

        let room_id = RoomId::from_string("test_room".to_string());
        let user_id = UserId::from_string("user1".to_string());

        // Subscribe triggers RoomActivated
        let _rx = hub.subscribe(room_id.clone(), user_id.clone(), "conn1".to_string());
        let _ = lifecycle_rx.try_recv().unwrap(); // consume RoomActivated

        // remove_room triggers RoomDeactivated
        hub.remove_room(&room_id);
        let event = lifecycle_rx.try_recv().unwrap();
        assert!(matches!(event, RoomLifecycleEvent::RoomDeactivated(ref rid) if rid.as_str() == "test_room"));
    }
}
