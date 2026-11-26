//! Outbound address policy once the configured CIDR matches are known.
#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum IpAccess {
    Denied,
    Allowed,
    CheckHost,
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    (result is Denied) == deny_all,
    (result is Allowed) == (!deny_all && (exception || !restricted)),
    (result is CheckHost) == (!deny_all && !exception && restricted),
))]
pub(super) fn classify(deny_all: bool, exception: bool, restricted: bool) -> IpAccess {
    if deny_all {
        IpAccess::Denied
    } else if exception || !restricted {
        IpAccess::Allowed
    } else {
        IpAccess::CheckHost
    }
}

// A hostname exception can unlock RFC 1918 or IPv6 unique-local unicast.
// Other restricted networks require an explicit IP policy.
#[cfg_attr(verus_only, verus_spec(allowed => ensures allowed == (
    0x0a00_0000 <= ip <= 0x0aff_ffff ||
    0xac10_0000 <= ip <= 0xac1f_ffff ||
    0xc0a8_0000 <= ip <= 0xc0a8_ffff),
))]
pub(super) fn private_v4(ip: u32) -> bool {
    let allowed = ip >> 24 == 10 || ip >> 20 == 0xac1 || ip >> 16 == 0xc0a8;
    #[cfg(verus_only)]
    proof! {
        assert((ip >> 24 == 10) == (0x0a00_0000 <= ip <= 0x0aff_ffff)) by(bit_vector);
        assert((ip >> 20 == 0xac1) == (0xac10_0000 <= ip <= 0xac1f_ffff)) by(bit_vector);
        assert((ip >> 16 == 0xc0a8) == (0xc0a8_0000 <= ip <= 0xc0a8_ffff)) by(bit_vector);
    }
    allowed
}

#[cfg_attr(verus_only, verus_spec(allowed => ensures allowed == (
    0xfc00_0000_0000_0000_0000_0000_0000_0000 <= ip <= 0xfdff_ffff_ffff_ffff_ffff_ffff_ffff_ffff),
))]
pub(super) fn private_v6(ip: u128) -> bool {
    #[cfg(verus_only)]
    proof! {
        assert((ip >> 121 == 0x7e) == (
            0xfc00_0000_0000_0000_0000_0000_0000_0000 <= ip <=
            0xfdff_ffff_ffff_ffff_ffff_ffff_ffff_ffff)) by(bit_vector);
    }
    ip >> 121 == 0x7e
}
