//! RFC 9000 variable-length integers and HTTP/3 first-frame admission.

#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub const MAX_CONTROL_FRAME_LEN: usize = 64 * 1024;

/// Maximum combined size of the frame type and frame length varints.
#[cfg_attr(verus_only, verus_verify)]
pub(super) const MAX_FRAME_HEADER: usize = 16;

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum Verdict {
    NeedMore,
    Ok,
    TooBig(u64),
}

#[cfg(verus_only)]
verus! {
    spec fn width(first: u8) -> nat {
        if first < 64 { 1 } else if first < 128 { 2 } else if first < 192 { 4 } else { 8 }
    }

    // The first two bits encode width. The remaining digits are big-endian.
    spec fn prefix_value(bytes: Seq<u8>, start: int, count: nat) -> int
        decreases count
    {
        if count <= 1 { bytes[start] as int % 64 }
        else { 256 * prefix_value(bytes, start, (count - 1) as nat) + bytes[start + count - 1] as int }
    }

    spec fn capacity(count: nat) -> int
        decreases count
    {
        if count <= 1 { 64 } else { 256 * capacity((count - 1) as nat) }
    }

    proof fn capacity_fits(count: nat)
        requires 1 <= count <= 8
        ensures capacity(count) <= 0x4000_0000_0000_0000int
    {
        reveal_with_fuel(capacity, 8);
        assert(count == 1 || count == 2 || count == 3 || count == 4
            || count == 5 || count == 6 || count == 7 || count == 8);
    }

    spec fn complete(bytes: Seq<u8>, start: int) -> bool {
        0 <= start < bytes.len() && start + width(bytes[start]) <= bytes.len()
    }

    pub(super) closed spec fn first_frame(bytes: Seq<u8>) -> Verdict {
        if !complete(bytes, 0) {
            Verdict::NeedMore
        } else {
            let prefix = width(bytes[0]);
            let ty = prefix_value(bytes, 0, prefix);
            if ty == 0 || ty >= 64 { Verdict::Ok }
            else if !complete(bytes, prefix as int) { Verdict::NeedMore }
            else {
                let len_width = width(bytes[prefix as int]);
                let len = prefix_value(bytes, prefix as int, len_width);
                if prefix + len_width + len <= MAX_CONTROL_FRAME_LEN { Verdict::Ok }
                else { Verdict::TooBig(len as u64) }
            }
        }
    }
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_some() == complete(buf@, start as int),
    result matches Some((value, len)) ==> (
        len == width(buf@[start as int])
        && start + len <= buf.len()
        && value == prefix_value(buf@, start as int, len as nat)
        && value < 0x4000_0000_0000_0000int),
))]
fn read_varint(buf: &[u8], start: usize) -> Option<(u64, usize)> {
    if start >= buf.len() {
        return None;
    }
    let first = buf[start];
    #[cfg(verus_only)]
    proof! { assert(first >> 6 <= 3u8) by(bit_vector); }
    let len = 1_usize << (first >> 6);
    #[cfg(verus_only)]
    proof! {
        assert(len == width(first)) by(bit_vector)
            requires len == 1usize << (first >> 6);
        assert((first & 63) == first % 64) by(bit_vector);
    }
    if len > buf.len() - start {
        return None;
    }
    let mut value = u64::from(first & 63);
    let mut i = 1;
    #[cfg_attr(verus_only, verus_spec(
        invariant
            1 <= i <= len <= 8,
            start + len <= buf.len(),
            value == prefix_value(buf@, start as int, i as nat),
            0 <= value < capacity(i as nat),
        decreases len - i,
    ))]
    while i < len {
        #[cfg(verus_only)]
        proof! { capacity_fits((i + 1) as nat); }
        value = value * 256 + u64::from(buf[start + i]);
        i += 1;
    }
    #[cfg(verus_only)]
    proof! { capacity_fits(i as nat); }
    Some((value, len))
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result == first_frame(header@),
    result is NeedMore ==> header.len() < MAX_FRAME_HEADER,
))]
pub(super) fn verdict(header: &[u8]) -> Verdict {
    let Some((frame_type, type_len)) = read_varint(header, 0) else {
        return Verdict::NeedMore;
    };
    if frame_type == 0 || frame_type >= 0x40 {
        return Verdict::Ok;
    }
    let Some((length, length_len)) = read_varint(header, type_len) else {
        return Verdict::NeedMore;
    };
    let prefix = type_len + length_len;
    if length > (MAX_CONTROL_FRAME_LEN - prefix) as u64 {
        Verdict::TooBig(length)
    } else {
        Verdict::Ok
    }
}
