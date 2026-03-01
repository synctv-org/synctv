use std::time::SystemTime;

#[must_use]
pub fn current_time() -> u32 {
    let duration = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);

    match duration {
        Ok(result) => u32::try_from(result.as_nanos()).expect("REASON"),
        _ => 0,
    }
}
