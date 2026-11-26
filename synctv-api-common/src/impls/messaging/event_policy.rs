use synctv_core::models::{RoomId, UserId};
use synctv_realtime::sync::{DisconnectSignal, RealtimeEvent};

mod scope;
use scope::Scope;
pub use scope::should_broadcast_user_left;

fn signal_scope(signal: &DisconnectSignal, connection_id: &str) -> Scope {
    match signal {
        DisconnectSignal::Connection(id) => Scope::Connection(id == connection_id),
        DisconnectSignal::User(u) => Scope::User(u.get()),
        DisconnectSignal::Room { room_id, .. } => Scope::Room(room_id.get()),
        DisconnectSignal::UserFromRoom { user_id, room_id } => {
            Scope::RoomUser(room_id.get(), user_id.get())
        }
    }
}

fn admin_scope(event: &RealtimeEvent) -> Scope {
    match event {
        RealtimeEvent::KickUser { user_id, .. } => Scope::User(user_id.get()),
        RealtimeEvent::RoomDeleted { room_id, .. } => Scope::DeletedRoom(room_id.get()),
        RealtimeEvent::RoomBanned { room_id, .. }
        | RealtimeEvent::RoomOwnerInactive { room_id, .. } => Scope::Room(room_id.get()),
        RealtimeEvent::KickUserFromRoom {
            user_id, room_id, ..
        } => Scope::RoomUser(room_id.get(), user_id.get()),
        _ => Scope::Unrelated,
    }
}

#[inline]
pub fn disconnect_signal_requires_skip_cleanup(
    signal: &DisconnectSignal,
    user_id: Option<UserId>,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    signal_scope(signal, connection_id).skip_cleanup(room_id.get(), user_id.map(UserId::get))
}

#[inline]
pub fn admin_event_requires_skip_cleanup(
    event: &RealtimeEvent,
    user_id: Option<UserId>,
    room_id: &RoomId,
) -> bool {
    admin_scope(event).skip_cleanup(room_id.get(), user_id.map(UserId::get))
}

#[inline]
pub fn watch_disconnect_signal_matches(
    signal: &DisconnectSignal,
    user_id: Option<UserId>,
    room_id: &RoomId,
    connection_id: &str,
) -> bool {
    signal_scope(signal, connection_id).disconnect(room_id.get(), user_id.map(UserId::get))
}

#[inline]
pub fn watch_admin_event_matches(
    event: &RealtimeEvent,
    user_id: Option<UserId>,
    room_id: &RoomId,
) -> bool {
    admin_scope(event).disconnect(room_id.get(), user_id.map(UserId::get))
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
