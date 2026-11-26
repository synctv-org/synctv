#[cfg(verus_only)]
use vstd::prelude::*;

#[cfg_attr(verus_only, verus_verify)]
pub(super) const MAX_LEN: usize = 2048;

#[cfg_attr(verus_only, verus_verify)]
pub(super) enum FieldError {
    Length,
    Newline,
    Null,
}

/// Check bytes interpolated into an HLS playlist line, preserving error precedence.
#[cfg_attr(verus_only, verus_spec(out => ensures
    out.is_ok() == (bytes.len() <= MAX_LEN && !bytes@.contains(10) && !bytes@.contains(13) && !bytes@.contains(0)),
    (out matches Err(FieldError::Length)) == (bytes.len() > MAX_LEN),
    (out matches Err(FieldError::Newline)) == (bytes.len() <= MAX_LEN && (bytes@.contains(10) || bytes@.contains(13))),
    (out matches Err(FieldError::Null)) == (bytes.len() <= MAX_LEN && bytes@.contains(0) && !bytes@.contains(10) && !bytes@.contains(13)),
))]
pub(super) fn validate(bytes: &[u8]) -> Result<(), FieldError> {
    if bytes.len() > MAX_LEN {
        return Err(FieldError::Length);
    }
    let mut has_null = false;
    let mut i = 0;
    #[cfg_attr(verus_only, verus_spec(
        invariant i <= bytes.len() <= MAX_LEN,
            forall|k:int| 0 <= k < i ==> #[trigger] bytes@[k] != 10 && bytes@[k] != 13,
            has_null == (exists|k:int| 0 <= k < i && #[trigger] bytes@[k] == 0),
        decreases bytes.len() - i,
    ))]
    while i < bytes.len() {
        match bytes[i] {
            b'\n' | b'\r' => return Err(FieldError::Newline),
            0 => has_null = true,
            _ => {}
        }
        i += 1;
    }
    if has_null {
        Err(FieldError::Null)
    } else {
        Ok(())
    }
}
