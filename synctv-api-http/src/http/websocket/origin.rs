#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Clone, Copy)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum Scheme {
    Http,
    Https,
}

#[cfg(verus_only)]
verus! {
    pub(super) open spec fn folded(b: u8) -> int { if 65 <= b <= 90 { b + 32 } else { b as int } }
    pub(super) open spec fn same_host(a: Seq<u8>, b: Seq<u8>) -> bool {
        a.len() == b.len() && forall|i:int| #![trigger a[i]] #![trigger b[i]] 0 <= i < a.len() ==> folded(a[i]) == folded(b[i])
    }
}

#[cfg_attr(verus_only, verus_spec(out => ensures out == folded(b)))]
const fn fold_ascii(b: u8) -> u8 {
    if b >= b'A' && b <= b'Z' { b + 32 } else { b }
}

#[cfg_attr(verus_only, verus_spec(out => ensures out == same_host(a@, b@)))]
fn equal_hostname(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant i <= a.len() == b.len(), forall|j:int| #![trigger a@[j]] #![trigger b@[j]] 0 <= j < i ==> folded(a@[j]) == folded(b@[j]),
        decreases a.len() - i,
    ))]
    while i < a.len() {
        if fold_ascii(a[i]) != fold_ascii(b[i]) {
            return false;
        }
        i += 1;
    }
    true
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out == (same_host(origin_host@, request_host@) &&
        (forwarded.is_none() || forwarded == Some(scheme)) &&
        (match origin_port { Some(p) => p as int, None => if scheme is Http {80int} else {443int} }) ==
        (match request_port { Some(p) => p as int, None => if scheme is Http {80int} else {443int} })),
))]
pub(super) fn matches_authority(
    origin_host: &[u8],
    request_host: &[u8],
    scheme: Scheme,
    forwarded: Option<Scheme>,
    origin_port: Option<u16>,
    request_port: Option<u16>,
) -> bool {
    let same_scheme = matches!(
        (scheme, forwarded),
        (_, None) | (Scheme::Http, Some(Scheme::Http)) | (Scheme::Https, Some(Scheme::Https))
    );
    let default_port = match scheme {
        Scheme::Http => 80,
        Scheme::Https => 443,
    };
    same_scheme
        && origin_port.unwrap_or(default_port) == request_port.unwrap_or(default_port)
        && equal_hostname(origin_host, request_host)
}
