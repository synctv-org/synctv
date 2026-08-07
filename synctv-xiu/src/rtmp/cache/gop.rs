use {crate::streamhub::define::FrameData, std::collections::VecDeque, std::sync::Arc};

/// Max frames per GOP to prevent unbounded memory growth.
/// 1500 frames ≈ 1 minute at 24fps, generous for any reasonable GOP.
const MAX_FRAMES_PER_GOP: usize = 1500;

/// Max memory per GOP (100 MB).
/// Each frame can vary widely in size (keyframes are larger).
const MAX_MEMORY_PER_GOP: usize = 100 * 1024 * 1024;

fn clone_or_unwrap_frozen_frames(frames: Arc<Vec<FrameData>>) -> Vec<FrameData> {
    match Arc::try_unwrap(frames) {
        Ok(frames) => frames,
        Err(shared_frames) => (*shared_frames).clone(),
    }
}

/// A single Group of Pictures.
///
/// Internally stores frames in `Arc<Vec<FrameData>>` so that cloning a `Gop`
/// (e.g., when a new subscriber joins and receives cached GOPs) is O(1) --
/// only the Arc reference count is bumped, not the entire frame payload.
///
/// While the GOP is still being built (active GOP at the back of the deque),
/// frames are accumulated in `pending`. When the GOP is finalized (next keyframe
/// arrives) or when `get_gops()` is called, pending frames are frozen into
/// the Arc.
#[derive(Clone)]
pub struct Gop {
    /// Frozen (immutable) frames -- cheap to clone via Arc.
    frozen: Arc<Vec<FrameData>>,
    /// Frames being accumulated for the current (active) GOP.
    /// Empty once frozen.
    pending: Vec<FrameData>,
    /// Estimated memory usage in bytes.
    memory_bytes: usize,
}

impl Default for Gop {
    fn default() -> Self {
        Self::new()
    }
}

impl Gop {
    #[must_use]
    pub fn new() -> Self {
        Self {
            frozen: Arc::new(Vec::new()),
            pending: Vec::new(),
            memory_bytes: 0,
        }
    }

    /// Estimate the memory size of a `FrameData` in bytes.
    ///
    /// For `MediaInfo`, we account for both the inline struct size and any
    /// heap-allocated fields (e.g., `String`s).  Currently `MediaInfo` has no
    /// heap data, so `heap_size()` returns 0, but using this pattern ensures
    /// correctness if fields are added in the future.
    pub(crate) const fn frame_memory_size(data: &FrameData) -> usize {
        match data {
            FrameData::Video { data, .. }
            | FrameData::Audio { data, .. }
            | FrameData::MetaData { data, .. } => data.len(),
            FrameData::MediaInfo { media_info } => {
                std::mem::size_of::<crate::streamhub::define::MediaInfo>() + media_info.heap_size()
            }
        }
    }

    /// Attempt to save a frame. Returns `true` if the frame was stored,
    /// `false` if it was dropped due to per-GOP limits.
    fn save_frame_data(&mut self, data: FrameData) -> bool {
        let total = self.frozen.len() + self.pending.len();
        if total >= MAX_FRAMES_PER_GOP {
            if total == MAX_FRAMES_PER_GOP {
                tracing::warn!(
                    "GOP reached MAX_FRAMES_PER_GOP ({MAX_FRAMES_PER_GOP}), dropping subsequent frames until next keyframe"
                );
            }
            return false;
        }

        // Check memory limit
        let frame_size = Self::frame_memory_size(&data);
        if self.memory_bytes + frame_size > MAX_MEMORY_PER_GOP {
            tracing::warn!(
                current_memory_mb = (self.memory_bytes / 1024 / 1024),
                frame_size_kb = (frame_size / 1024),
                max_memory_mb = (MAX_MEMORY_PER_GOP / 1024 / 1024),
                "GOP reached memory limit, dropping frame"
            );
            return false;
        }

        self.memory_bytes += frame_size;
        self.pending.push(data);
        true
    }

    /// Freeze pending frames into the Arc for zero-copy clone.
    fn freeze(&mut self) {
        if !self.pending.is_empty() {
            let mut all_frames = Vec::with_capacity(self.frozen.len() + self.pending.len());
            all_frames.extend_from_slice(&self.frozen);
            all_frames.append(&mut self.pending);
            self.frozen = Arc::new(all_frames);
            // Note: memory_bytes is not reset since frozen frames still consume memory
        }
    }

    /// Get estimated memory usage in bytes.
    #[must_use]
    pub const fn memory_bytes(&self) -> usize {
        self.memory_bytes
    }

    /// Get all frame data as a slice (frozen frames only; call `freeze()` first).
    #[must_use]
    pub fn frame_data(&self) -> &[FrameData] {
        &self.frozen
    }

    /// Get all frame data (frozen + pending), consuming self.
    #[must_use]
    pub fn get_frame_data(mut self) -> Vec<FrameData> {
        self.freeze();
        clone_or_unwrap_frozen_frames(self.frozen)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.frozen.len() + self.pending.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Default maximum total bytes across all GOPs per stream (500 MB).
///
/// When exceeded, the oldest GOP is dropped even if `gop_num` has room.
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 500 * 1024 * 1024;

const fn resolve_max_total_bytes(max_total_bytes: Option<usize>) -> usize {
    match max_total_bytes {
        Some(value) => value,
        None => DEFAULT_MAX_TOTAL_BYTES,
    }
}

/// Default global memory limit across ALL streams (2 GB).
///
/// Without this limit, each stream has an independent 500 MB budget, so
/// 100 concurrent streams could consume up to 50 GB. The global tracker
/// provides a shared ceiling: when the sum of all per-stream GOP caches
/// exceeds this value, new frames trigger eviction of the oldest GOPs
/// within their stream, reducing global pressure.
#[derive(Clone)]
pub struct Gops {
    entries: VecDeque<Gop>,
    size: usize,
    /// Maximum total bytes across all GOPs. When exceeded, oldest GOP is evicted.
    max_total_bytes: usize,
    /// Current total bytes across all GOPs.
    current_total_bytes: usize,
}

impl Default for Gops {
    fn default() -> Self {
        Self::new(1, None)
    }
}

impl Gops {
    /// Create a new `Gops` cache with the given GOP count limit and optional
    /// per-stream memory cap.
    ///
    /// - `max_total_bytes`: per-stream cap. `None` uses [`DEFAULT_MAX_TOTAL_BYTES`] (500 MB).
    #[must_use]
    pub fn new(size: usize, max_total_bytes: Option<usize>) -> Self {
        Self {
            entries: VecDeque::from([Gop::new()]),
            size,
            max_total_bytes: resolve_max_total_bytes(max_total_bytes),
            current_total_bytes: 0,
        }
    }

    /// Get the current total memory in bytes across all GOPs.
    #[must_use]
    pub const fn current_total_bytes(&self) -> usize {
        self.current_total_bytes
    }

    /// Get the configured maximum total bytes.
    #[must_use]
    pub const fn max_total_bytes(&self) -> usize {
        self.max_total_bytes
    }

    /// Get the number of currently cached GOPs.
    #[must_use]
    pub fn gop_count(&self) -> usize {
        self.entries.len()
    }

    /// Get the configured maximum number of GOPs (size limit).
    #[must_use]
    pub const fn max_gop_count(&self) -> usize {
        self.size
    }

    /// Evict the oldest GOP from this stream, updating per-stream and global counters.
    /// Returns the number of bytes evicted, or 0 if nothing could be evicted.
    fn evict_oldest_gop(&mut self, reason: &str) -> usize {
        if self.entries.len() <= 1 {
            return 0;
        }
        if let Some(evicted) = self.entries.pop_front() {
            let evicted_bytes = evicted.memory_bytes();
            self.current_total_bytes = self.current_total_bytes.saturating_sub(evicted_bytes);
            tracing::warn!(
                evicted_bytes,
                remaining_gops = self.entries.len(),
                stream_bytes = self.current_total_bytes,
                max_total_bytes = self.max_total_bytes,
                reason,
                "GOP evicted"
            );
            evicted_bytes
        } else {
            0
        }
    }

    pub fn save_frame_data(&mut self, data: FrameData, is_key_frame: bool) {
        if self.size == 0 {
            return;
        }

        if is_key_frame {
            // Freeze the current back GOP before pushing a new one,
            // so it's ready for zero-copy clone.
            let should_start_new_gop = self.entries.back().is_some_and(|back| !back.is_empty());
            if should_start_new_gop {
                if let Some(back) = self.entries.back_mut() {
                    back.freeze();
                }
                if self.entries.len() == self.size {
                    self.evict_oldest_gop("GOP count limit reached");
                }
                self.entries.push_back(Gop::new());
            }
        }

        // Check memory limit BEFORE adding the frame to keep accounting precise.
        let frame_bytes = Gop::frame_memory_size(&data);
        while self.current_total_bytes + frame_bytes > self.max_total_bytes
            && self.entries.len() > 1
        {
            if self.evict_oldest_gop("per-stream memory limit (pre-frame)") == 0 {
                break;
            }
        }

        if self.entries.is_empty() {
            self.entries.push_back(Gop::new());
        }

        if self
            .entries
            .back_mut()
            .is_some_and(|gop| gop.save_frame_data(data))
        {
            self.current_total_bytes += frame_bytes;
        }
    }

    /// Returns `true` if the GOP cache is enabled (has non-zero size limit).
    #[must_use]
    pub const fn is_enabled(&self) -> bool {
        self.size != 0
    }

    /// Get all GOPs as a reference. Freezes any pending frames first so
    /// callers can use `frame_data()` on each Gop without cloning.
    #[must_use]
    pub fn get_gops(&mut self) -> &VecDeque<Gop> {
        // Freeze the active GOP so frame_data() returns all frames
        if let Some(back) = self.entries.back_mut() {
            back.freeze();
        }
        &self.entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    /// Helper: create a video frame of a given size.
    fn video_frame(size: usize) -> FrameData {
        FrameData::Video {
            timestamp: 0,
            data: Bytes::from(vec![0u8; size]),
        }
    }

    #[test]
    fn test_gop_basic_save_and_freeze() {
        let mut gop = Gop::new();
        assert!(gop.is_empty());

        let stored = gop.save_frame_data(video_frame(100));
        assert!(stored);
        assert_eq!(gop.len(), 1);
        assert_eq!(gop.memory_bytes(), 100);
    }

    #[test]
    fn test_gop_get_frame_data_clones_shared_frozen_frames() {
        let mut gop = Gop::new();
        assert!(gop.save_frame_data(video_frame(100)));
        gop.freeze();

        let cloned = gop.clone();

        assert_eq!(gop.get_frame_data().len(), 1);
        assert_eq!(cloned.get_frame_data().len(), 1);
    }

    #[test]
    fn test_gops_uses_default_total_byte_limit() {
        let gops = Gops::new(1, None);

        assert_eq!(gops.max_total_bytes(), DEFAULT_MAX_TOTAL_BYTES);
    }

    #[test]
    fn test_gops_uses_custom_total_byte_limit() {
        let gops = Gops::new(1, Some(1024));

        assert_eq!(gops.max_total_bytes(), 1024);
    }

    #[test]
    fn test_first_keyframe_reuses_initial_empty_gop() {
        let mut gops = Gops::new(5, Some(1024));

        gops.save_frame_data(video_frame(600), true);

        assert_eq!(gops.gop_count(), 1);
        assert_eq!(gops.current_total_bytes(), 600);
    }

    #[test]
    fn test_gops_per_stream_eviction() {
        // Per-stream limit of 1 KB, 5 GOPs max
        let mut gops = Gops::new(5, Some(1024));

        // Fill with 1 GOP of ~600 bytes
        gops.save_frame_data(video_frame(600), true);
        assert_eq!(gops.current_total_bytes(), 600);

        // Start second GOP with ~600 bytes → total 1200 > 1024 → evicts first
        gops.save_frame_data(video_frame(600), true);
        assert_eq!(gops.current_total_bytes(), 600);
    }

    #[test]
    fn test_per_stream_memory_limit() {
        let mut gops = Gops::new(3, Some(500));

        gops.save_frame_data(video_frame(200), true);
        gops.save_frame_data(video_frame(200), true);
        assert_eq!(gops.current_total_bytes(), 400);

        gops.save_frame_data(video_frame(200), true);
        assert_eq!(gops.current_total_bytes(), 400);
    }
}
