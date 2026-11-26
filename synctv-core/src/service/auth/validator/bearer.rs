#[cfg(verus_only)]
use vstd::prelude::*;

/// Locate the body after the case-insensitive ASCII scheme and one space.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_some() == (bytes.len() > 7 &&
        (bytes[0] == 66 || bytes[0] == 98) && (bytes[1] == 69 || bytes[1] == 101) &&
        (bytes[2] == 65 || bytes[2] == 97) && (bytes[3] == 82 || bytes[3] == 114) &&
        (bytes[4] == 69 || bytes[4] == 101) && (bytes[5] == 82 || bytes[5] == 114) &&
        bytes[6] == 32),
    out matches Some(i) ==> i == 7 && i < bytes.len(),
))]
pub(super) fn body_offset(bytes: &[u8]) -> Option<usize> {
    if bytes.len() > 7
        && matches!(bytes[0], b'B' | b'b')
        && matches!(bytes[1], b'E' | b'e')
        && matches!(bytes[2], b'A' | b'a')
        && matches!(bytes[3], b'R' | b'r')
        && matches!(bytes[4], b'E' | b'e')
        && matches!(bytes[5], b'R' | b'r')
        && bytes[6] == b' '
    {
        Some(7)
    } else {
        None
    }
}
