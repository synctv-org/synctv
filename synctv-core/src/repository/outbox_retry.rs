#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg(verus_only)]
verus! {
    pub(crate) open spec fn sum(bytes: Seq<u8>, n: nat) -> int
        recommends n <= bytes.len()
        decreases n
    {
        if n == 0 { 0 } else { sum(bytes, (n - 1) as nat) + bytes[n - 1] }
    }
}

/// Keep delivery jitter stable for a job across retries and process restarts.
#[cfg_attr(verus_only, verus_spec(out => ensures out == sum(id@, id@.len()) % 5, out < 5))]
fn jitter(id: &[u8]) -> u8 {
    let mut value = 0u8;
    let mut i = 0usize;
    #[cfg_attr(verus_only, verus_spec(invariant i <= id.len(), value < 5,
        value == sum(id@, i as nat) % 5, decreases id.len() - i))]
    while i < id.len() {
        #[cfg(verus_only)]
        proof! {
            assert(sum(id@, (i + 1) as nat) == sum(id@, i as nat) + id@[i as int]);
            assert((sum(id@, i as nat) + id@[i as int]) % 5 ==
                ((sum(id@, i as nat) % 5) + id@[i as int]) % 5) by (nonlinear_arith);
        }
        value = u8::try_from((u16::from(value) + u16::from(id[i])) % 5).unwrap_or(0);
        i += 1;
    }
    value
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out == vstd::math::min(900, 5 * (1u16 << (if attempt <= 1 { 0 } else if attempt >= 9 { 8 } else { (attempt - 1) as u32 })) + sum(id@, id@.len()) % 5),
    5 <= out <= 900,
))]
pub(super) fn email_delay(attempt: i32, id: &[u8]) -> i32 {
    let exponent = u32::try_from(attempt.clamp(1, 9) - 1).unwrap_or(0);
    let factor = 1u16 << exponent;
    #[cfg(verus_only)]
    proof! { assert(1 <= factor <= 256) by (bit_vector) requires exponent <= 8, factor == 1u16 << exponent; }
    (5 * i32::from(factor) + i32::from(jitter(id))).min(900)
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.0 == vstd::math::min(i32::MAX as int, attempts as int + 1),
    out.1 == (1u16 << (if attempts <= 0 { 1 } else if attempts >= 7 { 8 } else { (attempts + 1) as u32 })),
    out.2 == (attempts >= 11),
))]
pub(super) fn realtime_failure(attempts: i32) -> (i32, i64, bool) {
    let next = if attempts == i32::MAX {
        attempts
    } else {
        attempts + 1
    };
    let exponent = u32::try_from(next.clamp(1, 8)).unwrap_or(1);
    (next, i64::from(1u16 << exponent), next >= 12)
}
