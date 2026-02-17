use {std::collections::VecDeque, std::sync::Arc, crate::streamhub::define::FrameData};

/// Max frames per GOP to prevent unbounded memory growth.
/// 1500 frames ≈ 1 minute at 24fps, generous for any reasonable GOP.
const MAX_FRAMES_PER_GOP: usize = 1500;

/// Max memory per GOP (100 MB) to prevent OOM.
/// Each frame can vary widely in size (keyframes are larger).
const MAX_MEMORY_PER_GOP: usize = 100 * 1024 * 1024;

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

    /// Estimate the memory size of a FrameData in bytes.
    ///
    /// For `MediaInfo`, we account for both the inline struct size and any
    /// heap-allocated fields (e.g., `String`s).  Currently `MediaInfo` has no
    /// heap data, so `heap_size()` returns 0, but using this pattern ensures
    /// correctness if fields are added in the future.
    pub(crate) fn frame_memory_size(data: &FrameData) -> usize {
        match data {
            FrameData::Video { data, .. } => data.len(),
            FrameData::Audio { data, .. } => data.len(),
            FrameData::MetaData { data, .. } => data.len(),
            FrameData::MediaInfo { media_info } => {
                std::mem::size_of::<crate::streamhub::define::MediaInfo>()
                    + media_info.heap_size()
            }
        }
    }

    fn save_frame_data(&mut self, data: FrameData) {
        let total = self.frozen.len() + self.pending.len();
        if total >= MAX_FRAMES_PER_GOP {
            if total == MAX_FRAMES_PER_GOP {
                tracing::warn!(
                    "GOP reached MAX_FRAMES_PER_GOP ({MAX_FRAMES_PER_GOP}), dropping subsequent frames until next keyframe"
                );
            }
            return;
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
            return;
        }

        self.memory_bytes += frame_size;
        self.pending.push(data);
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
        Arc::try_unwrap(self.frozen).unwrap_or_else(|arc| (*arc).clone())
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

/// Default maximum total bytes across all GOPs per stream (50 MB).
/// When exceeded, the oldest GOP is dropped even if `gop_num` hasn't been reached.
/// Prevents OOM under high-bitrate multi-stream scenarios (e.g., 4K at 50 Mbps).
pub const DEFAULT_MAX_TOTAL_BYTES: usize = 50 * 1024 * 1024;

#[derive(Clone)]
pub struct Gops {
    gops: VecDeque<Gop>,
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
    /// per-stream memory cap. If `max_total_bytes` is `None`, the built-in
    /// default ([`DEFAULT_MAX_TOTAL_BYTES`], 50 MB) is used.
    #[must_use]
    pub fn new(size: usize, max_total_bytes: Option<usize>) -> Self {
        Self {
            gops: VecDeque::from([Gop::new()]),
            size,
            max_total_bytes: max_total_bytes.unwrap_or(DEFAULT_MAX_TOTAL_BYTES),
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
        self.gops.len()
    }

    /// Get the configured maximum number of GOPs (size limit).
    #[must_use]
    pub const fn max_gop_count(&self) -> usize {
        self.size
    }

    /// Evict the oldest GOP from this stream, updating per-stream counters.
    /// Returns the number of bytes evicted, or 0 if nothing could be evicted.
    fn evict_oldest_gop(&mut self, reason: &str) -> usize {
        if self.gops.len() <= 1 {
            return 0;
        }
        if let Some(evicted) = self.gops.pop_front() {
            let evicted_bytes = evicted.memory_bytes();
            self.current_total_bytes = self.current_total_bytes.saturating_sub(evicted_bytes);
            tracing::warn!(
                evicted_bytes,
                remaining_gops = self.gops.len(),
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
            if let Some(back) = self.gops.back_mut() {
                back.freeze();
            }
            if self.gops.len() == self.size {
                self.evict_oldest_gop("GOP count limit reached");
            }
            self.gops.push_back(Gop::new());
        }

        // Evict oldest GOPs if per-stream memory exceeds limit (keep at least the active GOP)
        while self.current_total_bytes > self.max_total_bytes && self.gops.len() > 1 {
            self.evict_oldest_gop("per-stream memory limit");
        }

        // Check memory limit BEFORE adding the frame to keep accounting precise.
        let frame_bytes = Gop::frame_memory_size(&data);
        while self.current_total_bytes + frame_bytes > self.max_total_bytes
            && self.gops.len() > 1
        {
            if self.evict_oldest_gop("per-stream memory limit (pre-frame)") == 0 {
                break;
            }
        }

        if let Some(gop) = self.gops.back_mut() {
            self.current_total_bytes += frame_bytes;
            gop.save_frame_data(data);
        } else {
            tracing::error!("should not be here!");
        }
    }

    /// Returns `true` if the GOP cache is configured (size > 0).
    #[must_use]
    #[deprecated(since = "0.1.0", note = "Use is_enabled() instead")]
    pub const fn setted(&self) -> bool {
        self.size != 0
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
        if let Some(back) = self.gops.back_mut() {
            back.freeze();
        }
        &self.gops
    }
}
