use std::time::SystemTime;

#[must_use]
pub fn current_time() -> u32 {
    let duration = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);

    match duration {
        Ok(result) => result.as_millis() as u32,
        _ => 0,
    }
}
