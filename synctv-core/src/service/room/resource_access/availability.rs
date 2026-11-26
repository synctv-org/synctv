#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub enum ClientResourceAvailability {
    Available,
    CreatorInactive,
}

impl ClientResourceAvailability {
    #[must_use]
    #[cfg_attr(verus_only, verus_spec(out => ensures out == (self is Available)))]
    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }
}

#[cfg_attr(verus_only, verus_spec(out => ensures (out is Available) == (!banned && active_creator)))]
pub(super) const fn classify(banned: bool, active_creator: bool) -> ClientResourceAvailability {
    if !banned && active_creator {
        ClientResourceAvailability::Available
    } else {
        ClientResourceAvailability::CreatorInactive
    }
}

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum GuestRefusal {
    Private,
    Banned,
    Closed,
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (public && !banned && !closed),
    (out matches Err(GuestRefusal::Private)) == !public,
    (out matches Err(GuestRefusal::Banned)) == (public && banned),
    (out matches Err(GuestRefusal::Closed)) == (public && !banned && closed),
))]
pub(super) const fn check_guest(
    public: bool,
    banned: bool,
    closed: bool,
) -> Result<(), GuestRefusal> {
    if !public {
        Err(GuestRefusal::Private)
    } else if banned {
        Err(GuestRefusal::Banned)
    } else if closed {
        Err(GuestRefusal::Closed)
    } else {
        Ok(())
    }
}
