use std::sync::Arc;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct MetaEvictionCandidate {
    pub last_accessed: std::time::SystemTime,
    pub key: String,
}

pub(super) struct UpdatingKeyGuard {
    updating_keys: Arc<dashmap::DashSet<String>>,
    key: String,
}

impl UpdatingKeyGuard {
    pub(super) const fn new(updating_keys: Arc<dashmap::DashSet<String>>, key: String) -> Self {
        Self { updating_keys, key }
    }
}

impl Drop for UpdatingKeyGuard {
    fn drop(&mut self) {
        self.updating_keys.remove(&self.key);
    }
}

const U32_RANGE_AS_F64: f64 = 4_294_967_296.0;

/// Lossless `u64` -> `f64` conversion that avoids clippy's
/// `cast_precision_loss` lint by combining the high and low 32-bit halves.
pub(super) fn u64_to_f64(value: u64) -> f64 {
    let high = u32::try_from(value >> 32).unwrap_or(u32::MAX);
    let low = u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX);
    f64::from(high).mul_add(U32_RANGE_AS_F64, f64::from(low))
}

pub(super) fn ratio_u64(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        u64_to_f64(numerator) / u64_to_f64(denominator)
    }
}
