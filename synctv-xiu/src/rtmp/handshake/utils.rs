use std::time::SystemTime;

#[must_use]
pub fn timestamp_ms() -> u32 {
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(duration) => {
            u32::try_from(duration.as_millis().min(u128::from(u32::MAX))).unwrap_or(u32::MAX)
        }
        Err(err) => {
            tracing::warn!(
                "system clock is before UNIX_EPOCH while building RTMP handshake timestamp: {err}"
            );
            0
        }
    }
}
