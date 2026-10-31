use synctv_core::models::{RoomId, UserId};
use synctv_realtime::sync::{DisconnectSignal, RealtimeEvent};

pub const fn should_broadcast_user_left(
    has_other_local_connection: bool,
    distributed_presence: Result<bool, ()>,
) -> bool {
    if has_other_local_connection {
        return false;
    }

    match distributed_presence {
        Ok(true) => false,
        Ok(false) | Err(()) => true,
    }
}

#[inline]
pub fn disconnect_signal_requires_skip_cleanup(
    signal: &DisconnectSignal,
    user_id: Option<UserId>,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    match signal {
        DisconnectSignal::Connection(conn_id) => conn_id == connection_id,
        // A global user disconnect (ban/delete) must still let cleanup emit a
        // room-scoped UserLeft for the connection's current room.
        DisconnectSignal::User(_uid) => false,
        DisconnectSignal::Room { room_id: rid, .. } => rid == room_id,
        DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => user_id == Some(*uid) && rid == room_id,
    }
}

#[inline]
pub fn admin_event_requires_skip_cleanup(
    event: &RealtimeEvent,
    user_id: Option<UserId>,
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
        } => user_id == Some(*uid) && rid == room_id,
        _ => false,
    }
}

#[inline]
pub fn watch_disconnect_signal_matches(
    signal: &DisconnectSignal,
    user_id: Option<UserId>,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    match signal {
        DisconnectSignal::Connection(conn_id) => conn_id == connection_id,
        DisconnectSignal::User(uid) => user_id == Some(*uid),
        DisconnectSignal::Room { room_id: rid, .. } => rid == room_id,
        DisconnectSignal::UserFromRoom {
            user_id: uid,
            room_id: rid,
        } => user_id == Some(*uid) && rid == room_id,
    }
}

#[inline]
pub fn watch_admin_event_matches(
    event: &RealtimeEvent,
    user_id: Option<UserId>,
    room_id: &RoomId,
) -> bool {
    match event {
        RealtimeEvent::KickUser { user_id: uid, .. } => user_id == Some(*uid),
        RealtimeEvent::KickUserFromRoom {
            user_id: uid,
            room_id: rid,
            ..
        } => user_id == Some(*uid) && rid == room_id,
        RealtimeEvent::RoomDeleted { room_id: rid, .. }
        | RealtimeEvent::RoomBanned { room_id: rid, .. }
        | RealtimeEvent::RoomOwnerInactive { room_id: rid, .. } => rid == room_id,
        _ => false,
    }
}

/// Guest revocation is terminal for guest-principal watch sessions, mirroring
/// the WebSocket transport's `guest_access_revocation_reason` control. The
/// revocation fence keeps reconnection denied; this ends the live stream.
#[inline]
pub fn watch_guest_access_revocation_reason<'a>(
    is_guest: bool,
    event: &'a RealtimeEvent,
    room_id: &RoomId,
) -> Option<&'a str> {
    if !is_guest {
        return None;
    }
    match event {
        RealtimeEvent::GuestAccessRevoked {
            room_id: rid,
            reason,
            ..
        } if rid == room_id => Some(reason),
        _ => None,
    }
}
