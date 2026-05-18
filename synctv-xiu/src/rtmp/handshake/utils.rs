use std::time::SystemTime;

#[must_use]
pub fn timestamp_ms() -> u32 {
    let duration = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);

    match duration {
        Ok(result) => u32::try_from(result.as_millis()).unwrap_or(u32::MAX),
        _ => 0,
    }
}
