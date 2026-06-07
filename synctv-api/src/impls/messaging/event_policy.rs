use synctv_core::models::{RoomId, UserId};
use synctv_realtime::sync::{DisconnectSignal, RealtimeEvent};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UserLeftDeliveryPlan {
    Skip,
    LocalAndRedis,
}

pub(crate) const fn should_broadcast_user_left(
    has_other_local_connection: bool,
    distributed_presence: Result<bool, ()>,
) -> UserLeftDeliveryPlan {
    if has_other_local_connection {
        return UserLeftDeliveryPlan::Skip;
    }

    match distributed_presence {
        Ok(true) => UserLeftDeliveryPlan::Skip,
        Ok(false) | Err(()) => UserLeftDeliveryPlan::LocalAndRedis,
    }
}

pub(crate) const fn should_transition_webrtc_membership(
    current_rtc_joined: Option<bool>,
    target_joined: bool,
) -> Result<bool, &'static str> {
    match current_rtc_joined {
        Some(current) => Ok(current != target_joined),
        None => Err("Connection not found"),
    }
}

pub(crate) fn rebuild_leave_event_for_retry(event: &RealtimeEvent) -> RealtimeEvent {
    match event {
        RealtimeEvent::UserLeft {
            room_id,
            user_id,
            username,
            ..
        } => RealtimeEvent::UserLeft {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            user_id: *user_id,
            username: username.clone(),
            timestamp: chrono::Utc::now(),
        },
        RealtimeEvent::GuestLeft {
            room_id,
            guest_id,
            username,
            ..
        } => RealtimeEvent::GuestLeft {
            event_id: synctv_common::snanoid!(16),
            room_id: *room_id,
            guest_id: guest_id.clone(),
            username: username.clone(),
            timestamp: chrono::Utc::now(),
        },
        _ => event.clone(),
    }
}

#[inline]
pub(crate) fn disconnect_signal_requires_skip_cleanup(
    signal: &DisconnectSignal,
    user_id: &UserId,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    match signal {
        DisconnectSignal::Connection(conn_id) => conn_id == connection_id,
        // A global user disconnect (ban/delete) must still let cleanup emit a
        // room-scoped UserLeft for the connection's current room.
        DisconnectSignal::User(_uid) => false,
        DisconnectSignal::Room(rid) => rid == room_id,
        DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => uid == user_id && rid == room_id,
    }
}

#[inline]
pub(crate) fn admin_event_requires_skip_cleanup(
    event: &RealtimeEvent,
    user_id: &UserId,
    room_id: &RoomId,
) -> bool {
    match event {
        // A global KickUser must still allow connection cleanup to publish a
        // room-scoped UserLeft on the affected room.
        RealtimeEvent::KickUser { user_id: _uid, .. } => false,
        RealtimeEvent::RoomBanned { room_id: rid, .. }
        | RealtimeEvent::RoomOwnerInactive { room_id: rid, .. } => rid == room_id,
        RealtimeEvent::KickUserFromRoom {
            user_id: uid,
            room_id: rid,
            ..
        }
        | RealtimeEvent::UserLeft {
            user_id: uid,
            room_id: rid,
            ..
        } => uid == user_id && rid == room_id,
        _ => false,
    }
}

#[inline]
pub(crate) fn watch_disconnect_signal_matches(
    signal: &DisconnectSignal,
    user_id: &UserId,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    match signal {
        DisconnectSignal::Connection(conn_id) => conn_id == connection_id,
        DisconnectSignal::User(uid) => uid == user_id,
        DisconnectSignal::Room(rid) => rid == room_id,
        DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => uid == user_id && rid == room_id,
    }
}

#[inline]
pub(crate) fn watch_admin_event_matches(
    event: &RealtimeEvent,
    user_id: &UserId,
    room_id: &RoomId,
) -> bool {
    match event {
        RealtimeEvent::KickUser { user_id: uid, .. } => uid == user_id,
        RealtimeEvent::KickUserFromRoom {
            user_id: uid,
            room_id: rid,
            ..
        }
        | RealtimeEvent::UserLeft {
            user_id: uid,
            room_id: rid,
            ..
        } => uid == user_id && rid == room_id,
        RealtimeEvent::RoomDeleted { room_id: rid, .. }
        | RealtimeEvent::RoomBanned { room_id: rid, .. }
        | RealtimeEvent::RoomOwnerInactive { room_id: rid, .. } => rid == room_id,
        _ => false,
    }
}
