//! RTMP 5.3.1.1 basic header. The first byte selects the width of the stream ID.

#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg(verus_only)]
verus! {
    pub(super) closed spec fn parse(bytes: Seq<u8>) -> Option<(u8, u32, usize)> {
        if bytes.len() == 0 { None } else {
            let first = bytes[0] as int;
            let fmt = (first / 64) as u8;
            let marker = first % 64;
            if marker >= 2 { Some((fmt, marker as u32, 1)) }
            else if marker == 0 && bytes.len() >= 2 { Some((fmt, (64 + bytes[1]) as u32, 2)) }
            else if marker == 1 && bytes.len() >= 3 {
                Some((fmt, (64 + bytes[1] + 256 * bytes[2]) as u32, 3))
            } else { None }
        }
    }

    // A complete header delimits itself, even if the next header or payload is already buffered.
    proof fn framing(header: Seq<u8>, suffix: Seq<u8>, cut: int, fmt: u8, csid: u32, n: usize)
        requires parse(header) == Some((fmt, csid, n)), header.len() == n, 0 <= cut < n,
        ensures parse(header + suffix) == Some((fmt, csid, n)), parse(header.take(cut)) == None,
    {}
}

#[cfg_attr(verus_only, verus_spec(out => ensures out == parse(bytes@),
    out matches Some((fmt, csid, n)) ==> (fmt < 4 && 2 <= csid <= 65599 && 1 <= n <= bytes.len()),
))]
pub(super) fn decode(bytes: &[u8]) -> Option<(u8, u32, usize)> {
    let first = *bytes.first()?;
    let format = first / 64;
    match first % 64 {
        0 => Some((format, 64 + u32::from(*bytes.get(1)?), 2)),
        1 => {
            let low = u32::from(*bytes.get(1)?);
            let high = u32::from(*bytes.get(2)?);
            Some((format, 64 + low + 256 * high, 3))
        }
        id => Some((format, u32::from(id), 1)),
    }
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_some() == (format < 4 && 2 <= csid <= 65599),
    out matches Some((word, n)) ==> (
        n == (if csid <= 63 { 1int } else if csid <= 319 { 2int } else { 3int }) &&
        parse(word@.take(n as int)) == Some((format, csid, n))),
))]
pub(super) fn encode(format: u8, csid: u32) -> Option<([u8; 3], usize)> {
    if format > 3 || !(2..=65599).contains(&csid) {
        return None;
    }
    let first = format * 64;
    if csid < 64 {
        return Some(([first + u8::try_from(csid).ok()?, 0, 0], 1));
    }
    let extended = csid - 64;
    let low = u8::try_from(extended % 256).ok()?;
    if extended < 256 {
        Some(([first, low, 0], 2))
    } else {
        let high = u8::try_from(extended / 256).ok()?;
        Some(([first + 1, low, high], 3))
    }
}
