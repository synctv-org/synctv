#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum RoomAccessRejection {
    Banned,
    Closed,
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (!banned && !closed),
    (out matches Err(RoomAccessRejection::Banned)) == banned,
    (out matches Err(RoomAccessRejection::Closed)) == (!banned && closed),
))]
pub(super) const fn room(banned: bool, closed: bool) -> Result<(), RoomAccessRejection> {
    if banned {
        Err(RoomAccessRejection::Banned)
    } else if closed {
        Err(RoomAccessRejection::Closed)
    } else {
        Ok(())
    }
}

#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum Authority {
    GlobalAdmin,
    RoomAdmin,
    MediaCreator,
    NonMember,
    Unauthorized,
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    (out is GlobalAdmin) == global_admin,
    (out is RoomAdmin) == (!global_admin && member && room_admin),
    (out is NonMember) == (!global_admin && !member),
    (out is MediaCreator) == (!global_admin && member && !room_admin && creator == Some(user)),
    (out is Unauthorized) == (!global_admin && member && !room_admin && creator != Some(user)),
))]
pub(super) fn authority(
    global_admin: bool,
    member: bool,
    room_admin: bool,
    creator: Option<i64>,
    user: i64,
) -> Authority {
    if global_admin {
        Authority::GlobalAdmin
    } else if !member {
        Authority::NonMember
    } else if room_admin {
        Authority::RoomAdmin
    } else if creator == Some(user) {
        Authority::MediaCreator
    } else {
        Authority::Unauthorized
    }
}
