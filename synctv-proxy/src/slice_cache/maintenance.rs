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

#[allow(clippy::cast_precision_loss)]
pub(super) fn ratio_u64(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}
