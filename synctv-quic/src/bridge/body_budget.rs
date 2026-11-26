#[cfg(verus_only)]
use vstd::prelude::*;

/// Count the entire DATA buffer before copying it or draining the receive stream.
#[cfg_attr(verus_only, verus_spec(next => ensures
    next.is_some() == (received as int + incoming <= limit),
    next matches Some(n) ==> n as int == received as int + incoming,
))]
pub(super) fn account(received: usize, incoming: usize, limit: usize) -> Option<usize> {
    let next = received.checked_add(incoming)?;
    if next <= limit { Some(next) } else { None }
}
