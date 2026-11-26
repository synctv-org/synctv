#[cfg(verus_only)]
use vstd::prelude::*;

/// Credentials and the original referrer stay restricted for the rest of a redirect chain.
#[cfg_attr(verus_only, verus_spec(restricted => ensures restricted == (already_restricted || !same_origin)))]
pub(super) const fn restrict_chain(already_restricted: bool, same_origin: bool) -> bool {
    already_restricted || !same_origin
}

#[cfg_attr(verus_only, verus_spec(keep => ensures keep == (
    if name@ == "authorization"@ || name@ == "cookie"@ || name@ == "proxy-authorization"@ {
        !restricted
    } else {
        name@ == "origin"@ || name@ == "referer"@ || name@ == "user-agent"@ || name@ == "range"@ ||
        name@ == "accept"@ || name@ == "accept-language"@ || name@ == "if-none-match"@ || name@ == "if-modified-since"@
    }
)))]
pub(super) fn preserve_redirect_header(name: &str, restricted: bool) -> bool {
    if name == "authorization" || name == "cookie" || name == "proxy-authorization" {
        return !restricted;
    }
    name == "origin"
        || name == "referer"
        || name == "user-agent"
        || name == "range"
        || name == "accept"
        || name == "accept-language"
        || name == "if-none-match"
        || name == "if-modified-since"
}
