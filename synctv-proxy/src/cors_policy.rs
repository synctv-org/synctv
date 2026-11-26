#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum PreflightDecision<'a> {
    Wildcard,
    NoOrigin,
    Forbidden,
    Allowed(&'a str),
}

#[cfg_attr(verus_only, verus_spec(decision => ensures
    (decision matches PreflightDecision::Wildcard) == wildcard,
    (decision matches PreflightDecision::NoOrigin) == (!wildcard && origin.is_none()),
    (decision matches PreflightDecision::Allowed(_)) == (!wildcard && origin.is_some() &&
        exists|j:int| 0 <= j < allowed.len() && #[trigger] allowed@[j]@ == origin.unwrap()@),
    match decision { PreflightDecision::Allowed(s) => origin == Some(s), _ => true },
    (decision matches PreflightDecision::Forbidden) == (!wildcard && origin.is_some() &&
        forall|j:int| 0 <= j < allowed.len() ==> #[trigger] allowed@[j]@ != origin.unwrap()@),
))]
pub(super) fn preflight_decision<'a>(
    wildcard: bool,
    origin: Option<&'a str>,
    allowed: &[String],
) -> PreflightDecision<'a> {
    if wildcard {
        return PreflightDecision::Wildcard;
    }
    let Some(value) = origin else {
        return PreflightDecision::NoOrigin;
    };
    let mut i = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant !wildcard, origin == Some(value), i <= allowed.len(), forall|j:int| 0 <= j < i ==> #[trigger] allowed@[j]@ != value@,
        decreases allowed.len() - i,
    ))]
    while i < allowed.len() {
        if allowed[i].as_str() == value {
            return PreflightDecision::Allowed(value);
        }
        i += 1;
    }
    PreflightDecision::Forbidden
}
