#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Clone, Copy)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum Scope {
    Connection(bool),
    User(i64),
    Room(i64),
    DeletedRoom(i64),
    RoomUser(i64, i64),
    Unrelated,
}

impl Scope {
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == match self {
        Scope::Connection(same) => same, Scope::User(u) => user == Some(u),
        Scope::Room(r) | Scope::DeletedRoom(r) => room == r,
        Scope::RoomUser(r, u) => room == r && user == Some(u), Scope::Unrelated => false,
    }))]
    pub(super) fn disconnect(self, room: i64, user: Option<i64>) -> bool {
        match self {
            Self::Connection(same) => same,
            Self::User(u) => user == Some(u),
            Self::Room(r) | Self::DeletedRoom(r) => r == room,
            Self::RoomUser(r, u) => r == room && user == Some(u),
            Self::Unrelated => false,
        }
    }

    /// Global kicks and room deletions still run normal presence cleanup.
    #[cfg_attr(verus_only, verus_spec(ok => ensures ok == match self {
        Scope::Connection(same) => same, Scope::Room(r) => room == r,
        Scope::RoomUser(r, u) => room == r && user == Some(u), _ => false,
    }))]
    pub(super) fn skip_cleanup(self, room: i64, user: Option<i64>) -> bool {
        !matches!(self, Self::User(_) | Self::DeletedRoom(_)) && self.disconnect(room, user)
    }
}

#[cfg_attr(verus_only, verus_spec(ok => ensures ok == (!has_other_local_connection && distributed_presence != Ok(true))))]
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
