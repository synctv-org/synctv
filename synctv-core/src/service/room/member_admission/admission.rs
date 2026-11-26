#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum Refusal {
    Closed,
    MissingBanState,
    Banned,
    MissingCooldown,
    Cooldown,
}

#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (!closed && banned == Some(false) && cooldown == Some(false)),
    (out matches Err(Refusal::Closed)) == closed,
    (out matches Err(Refusal::MissingBanState)) == (!closed && banned.is_none()),
    (out matches Err(Refusal::Banned)) == (!closed && banned == Some(true)),
    (out matches Err(Refusal::MissingCooldown)) == (!closed && banned == Some(false) && cooldown.is_none()),
    (out matches Err(Refusal::Cooldown)) == (!closed && banned == Some(false) && cooldown == Some(true)),
))]
pub(super) fn admit(
    closed: bool,
    banned: Option<bool>,
    cooldown: Option<bool>,
) -> Result<(), Refusal> {
    match (closed, banned, cooldown) {
        (true, _, _) => Err(Refusal::Closed),
        (_, None, _) => Err(Refusal::MissingBanState),
        (_, Some(true), _) => Err(Refusal::Banned),
        (_, _, None) => Err(Refusal::MissingCooldown),
        (_, _, Some(true)) => Err(Refusal::Cooldown),
        (_, _, Some(false)) => Ok(()),
    }
}
