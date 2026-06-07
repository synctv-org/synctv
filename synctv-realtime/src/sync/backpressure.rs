use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Buffer pressure level for backpressure signaling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BufferPressure {
    /// Buffer is under normal load.
    Normal,
    /// Buffer is under moderate pressure.
    Moderate,
    /// Buffer is under high pressure.
    High,
    /// Buffer is at capacity.
    Critical,
}

impl BufferPressure {
    /// Check if this pressure level allows sending non-critical events.
    #[must_use]
    pub const fn allows_non_critical(self) -> bool {
        matches!(self, Self::Normal | Self::Moderate)
    }

    /// Check if this pressure level only allows critical events.
    #[must_use]
    pub const fn critical_only(self) -> bool {
        matches!(self, Self::High | Self::Critical)
    }
}

pub(crate) const fn retry_buffer_warn_threshold(max_retry_buffer: usize) -> usize {
    max_retry_buffer.saturating_mul(4) / 5
}

const fn retry_buffer_high_threshold(max_retry_buffer: usize) -> usize {
    max_retry_buffer.saturating_mul(9) / 10
}

#[derive(Clone)]
pub(crate) struct BufferPressureState {
    retry_buffer_size: Arc<AtomicUsize>,
    critical_buffer_size: Arc<AtomicUsize>,
    max_retry_buffer: usize,
    warn_threshold: usize,
    high_threshold: usize,
}

impl BufferPressureState {
    pub(crate) fn new(max_retry_buffer: usize) -> Self {
        Self {
            retry_buffer_size: Arc::new(AtomicUsize::new(0)),
            critical_buffer_size: Arc::new(AtomicUsize::new(0)),
            max_retry_buffer,
            warn_threshold: retry_buffer_warn_threshold(max_retry_buffer),
            high_threshold: retry_buffer_high_threshold(max_retry_buffer),
        }
    }

    fn pressure(&self) -> BufferPressure {
        let retry_size = self.retry_buffer_size.load(Ordering::Relaxed);
        let critical_size = self.critical_buffer_size.load(Ordering::Relaxed);
        let total = retry_size + critical_size;

        if total >= self.max_retry_buffer {
            BufferPressure::Critical
        } else if retry_size >= self.high_threshold {
            BufferPressure::High
        } else if retry_size >= self.warn_threshold {
            BufferPressure::Moderate
        } else {
            BufferPressure::Normal
        }
    }

    pub(crate) fn set_retry_size(&self, size: usize) {
        self.retry_buffer_size.store(size, Ordering::Relaxed);
    }

    pub(crate) fn set_critical_size(&self, size: usize) {
        self.critical_buffer_size.store(size, Ordering::Relaxed);
    }

    fn retry_buffer_size(&self) -> usize {
        self.retry_buffer_size.load(Ordering::Relaxed)
    }

    fn critical_buffer_size(&self) -> usize {
        self.critical_buffer_size.load(Ordering::Relaxed)
    }
}

/// Handle for checking publish buffer backpressure.
#[derive(Clone)]
pub struct PublishBackpressure {
    state: BufferPressureState,
}

impl PublishBackpressure {
    pub(crate) fn new(state: BufferPressureState) -> Self {
        Self { state }
    }

    /// Get the current buffer pressure level.
    #[must_use]
    pub fn pressure(&self) -> BufferPressure {
        self.state.pressure()
    }

    /// Check if the buffer can accept a non-critical event.
    #[must_use]
    pub fn can_send_non_critical(&self) -> bool {
        self.state.pressure().allows_non_critical()
    }

    /// Get the current retry buffer size.
    #[must_use]
    pub fn retry_buffer_size(&self) -> usize {
        self.state.retry_buffer_size()
    }

    /// Get the current critical buffer size.
    #[must_use]
    pub fn critical_buffer_size(&self) -> usize {
        self.state.critical_buffer_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pressure_levels_classify_non_critical_policy() {
        assert!(BufferPressure::Normal.allows_non_critical());
        assert!(!BufferPressure::Normal.critical_only());

        assert!(BufferPressure::Moderate.allows_non_critical());
        assert!(!BufferPressure::Moderate.critical_only());

        assert!(!BufferPressure::High.allows_non_critical());
        assert!(BufferPressure::High.critical_only());

        assert!(!BufferPressure::Critical.allows_non_critical());
        assert!(BufferPressure::Critical.critical_only());
    }

    #[test]
    fn pressure_state_tracks_retry_and_critical_buffers() {
        let state = BufferPressureState::new(1000);

        assert_eq!(state.pressure(), BufferPressure::Normal);

        state.set_retry_size(800);
        assert_eq!(state.pressure(), BufferPressure::Moderate);

        state.set_retry_size(900);
        assert_eq!(state.pressure(), BufferPressure::High);

        state.set_retry_size(1000);
        assert_eq!(state.pressure(), BufferPressure::Critical);

        state.set_retry_size(500);
        state.set_critical_size(500);
        assert_eq!(state.pressure(), BufferPressure::Critical);
    }

    #[test]
    fn backpressure_handle_reads_shared_state() {
        let state = BufferPressureState::new(1000);
        let backpressure = PublishBackpressure::new(state.clone());

        assert!(backpressure.can_send_non_critical());
        assert_eq!(backpressure.pressure(), BufferPressure::Normal);

        state.set_retry_size(900);
        assert!(!backpressure.can_send_non_critical());
        assert_eq!(backpressure.pressure(), BufferPressure::High);
    }
}
