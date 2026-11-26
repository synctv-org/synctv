#[cfg(verus_only)]
use vstd::prelude::*;

#[derive(Clone, Copy, PartialEq, Eq)]
#[cfg_attr(verus_only, verus_verify)]
pub(super) enum Depth {
    Single,
    Double,
    Triple,
}

#[cfg(verus_only)]
verus! {
    pub(super) open spec fn dot(s: Seq<u8>, i: int) -> bool {
        0 <= i && i + 1 < s.len() && s[i] == 50 && (s[i+1] == 101 || s[i+1] == 69)
    }
    pub(super) open spec fn escape(s: Seq<u8>, i: int) -> bool {
        0 <= i && i + 1 < s.len() && s[i] == 50 && s[i+1] == 53
    }
    pub(super) open spec fn at(s: Seq<u8>, i: int) -> Option<Depth> {
        if 0 <= i < s.len() && s[i] == 37 {
            if dot(s,i+1) { Some(Depth::Single) }
            else if escape(s,i+1) && dot(s,i+3) { Some(Depth::Double) }
            else if escape(s,i+1) && escape(s,i+3) && dot(s,i+5) { Some(Depth::Triple) }
            else { None }
        } else { None }
    }
}

#[cfg_attr(verus_only, verus_spec(out => requires i < bytes.len(), ensures out == at(bytes@, i as int)))]
fn encoded_at(bytes: &[u8], i: usize) -> Option<Depth> {
    let remaining = bytes.len() - i;
    if remaining < 3 || bytes[i] != b'%' || bytes[i + 1] != b'2' {
        return None;
    }
    if matches!(bytes[i + 2], b'e' | b'E') {
        return Some(Depth::Single);
    }
    if bytes[i + 2] != b'5' || remaining < 5 || bytes[i + 3] != b'2' {
        return None;
    }
    if matches!(bytes[i + 4], b'e' | b'E') {
        return Some(Depth::Double);
    }
    if bytes[i + 4] == b'5'
        && remaining >= 7
        && bytes[i + 5] == b'2'
        && matches!(bytes[i + 6], b'E' | b'e')
    {
        Some(Depth::Triple)
    } else {
        None
    }
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_none() == (forall|k:int| 0 <= k < bytes.len() ==> #[trigger] at(bytes@,k).is_none()),
    out.is_some() ==> exists|k:int| 0 <= k < bytes.len() && #[trigger] at(bytes@,k) == out &&
        forall|j:int| 0 <= j < k ==> #[trigger] at(bytes@,j).is_none(),
))]
pub(super) fn first(bytes: &[u8]) -> Option<Depth> {
    let mut i = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant i <= bytes.len(), forall|j:int| 0 <= j < i ==> #[trigger] at(bytes@,j).is_none(),
        decreases bytes.len() - i,
    ))]
    while i < bytes.len() {
        let found = encoded_at(bytes, i);
        if found.is_some() {
            return found;
        }
        i += 1;
    }
    None
}
