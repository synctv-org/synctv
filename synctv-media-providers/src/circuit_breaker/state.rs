#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub const CIRCUIT_BREAKER_THRESHOLD: u32 = 5;
#[cfg_attr(verus_only, verus_verify)]
pub const CIRCUIT_BREAKER_TIMEOUT_SECS: i64 = 30;

#[derive(Clone, Copy)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum State {
    Closed(u32),
    Open(i64),
    Probing,
}

impl Default for State {
    fn default() -> Self {
        Self::Closed(0)
    }
}

impl State {
    #[cfg_attr(verus_only, verus_spec(allowed => ensures
        allowed == ((*old(self)) is Closed || ((*old(self)) matches State::Open(t) && now as int >= t + CIRCUIT_BREAKER_TIMEOUT_SECS)),
        ((*old(self)) is Closed || !allowed) ==> *final(self) == *old(self),
        (allowed && !((*old(self)) is Closed)) ==> (*final(self)) is Probing,
    ))]
    pub(super) fn admit(&mut self, now: i64) -> bool {
        match *self {
            Self::Closed(_) => true,
            Self::Open(since)
                if i128::from(now) - i128::from(since)
                    >= i128::from(CIRCUIT_BREAKER_TIMEOUT_SECS) =>
            {
                *self = Self::Probing;
                true
            }
            Self::Open(_) | Self::Probing => false,
        }
    }

    #[cfg_attr(verus_only, verus_spec(opened => ensures
        opened == ((*old(self)) matches State::Closed(n) && n + 1 >= CIRCUIT_BREAKER_THRESHOLD),
        (*old(self)) matches State::Closed(n) ==> (n + 1 < CIRCUIT_BREAKER_THRESHOLD ==>
            *final(self) == State::Closed((n + 1) as u32)),
        (!((*old(self)) is Closed) || opened) ==> *final(self) == State::Open(now),
    ))]
    pub(super) fn fail(&mut self, now: i64) -> bool {
        match *self {
            Self::Closed(n) if n < CIRCUIT_BREAKER_THRESHOLD - 1 => {
                *self = Self::Closed(n + 1);
                false
            }
            previous => {
                *self = Self::Open(now);
                matches!(previous, Self::Closed(_))
            }
        }
    }

    #[cfg_attr(verus_only, verus_spec(ensures *final(self) == State::Closed(0)))]
    pub(super) fn succeed(&mut self) {
        *self = Self::Closed(0);
    }
}
