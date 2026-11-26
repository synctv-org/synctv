//! Authorization for writes to per-member permission overrides.
#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum ActorError {
    SelfEdit,
    ElevatedTarget,
}

#[cfg_attr(verus_only, verus_spec(result => ensures
    result.is_ok() == (actor != target && (!elevated || actor == owner)),
    (result matches Err(ActorError::SelfEdit)) == (actor == target),
    (result matches Err(ActorError::ElevatedTarget)) == (actor != target && elevated && actor != owner),
))]
pub(super) fn check_actor(
    owner: i64,
    actor: i64,
    target: i64,
    elevated: bool,
) -> Result<(), ActorError> {
    if actor == target {
        Err(ActorError::SelfEdit)
    } else if elevated && owner != actor {
        Err(ActorError::ElevatedTarget)
    } else {
        Ok(())
    }
}

#[cfg_attr(verus_only, verus_spec(valid => ensures valid ==
    (if admin { added == 0 && removed == 0 } else { admin_added == 0 && admin_removed == 0 }),
))]
pub(super) fn valid_channel(
    admin: bool,
    added: u64,
    removed: u64,
    admin_added: u64,
    admin_removed: u64,
) -> bool {
    if admin {
        added == 0 && removed == 0
    } else {
        admin_added == 0 && admin_removed == 0
    }
}
