//! Concurrency tests for RtmpStreamHandler cache lock optimization.
//!
//! These tests verify:
//! - High concurrent access to video/audio/metadata caches
//! - No deadlocks under contention
//! - Correctness of parallel frame saving

#![allow(clippy::unwrap_used)]
use parking_lot::RwLock;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// Import the actual SplitCache from the library
use bytes::BytesMut;
use synctv_xiu::rtmp::cache::SplitCache;
use synctv_xiu::streamhub::define::FrameData;

const SPLIT_LOCK_READ_WRITE_ITERATIONS: usize = 1000;
const CONTENTION_ITERATIONS: usize = 5000;
const CONTENTION_THREADS: usize = 16;
const SPLIT_CACHE_CONCURRENT_SAVES_ITERATIONS: usize = 100;
const HIGH_CONTENTION_THREADS: usize = 8;
const HIGH_CONTENTION_ITERATIONS: usize = 200;

fn usize_to_u8(value: usize) -> u8 {
    u8::try_from(value).expect("test value should fit in u8")
}

fn usize_to_u32(value: usize) -> u32 {
    u32::try_from(value).expect("test value should fit in u32")
}

// ==================================================================
// Simulated Split Lock Architecture (for performance baseline)
// ==================================================================

/// Simulated split cache structure for benchmarking
struct SplitCacheSim {
    video_seq: RwLock<(Vec<u8>, u32)>,
    audio_seq: RwLock<(Vec<u8>, u32)>,
    metadata: RwLock<(Vec<u8>, u32)>,
}

impl SplitCacheSim {
    const fn new() -> Self {
        Self {
            video_seq: RwLock::new((Vec::new(), 0)),
            audio_seq: RwLock::new((Vec::new(), 0)),
            metadata: RwLock::new((Vec::new(), 0)),
        }
    }

    fn save_video(&self, data: &[u8], ts: u32) {
        let mut guard = self.video_seq.write();
        *guard = (data.to_vec(), ts);
    }

    fn save_audio(&self, data: &[u8], ts: u32) {
        let mut guard = self.audio_seq.write();
        *guard = (data.to_vec(), ts);
    }

    fn save_metadata(&self, data: &[u8], ts: u32) {
        let mut guard = self.metadata.write();
        *guard = (data.to_vec(), ts);
    }

    fn get_video(&self) -> (Vec<u8>, u32) {
        self.video_seq.read().clone()
    }

    fn get_audio(&self) -> (Vec<u8>, u32) {
        self.audio_seq.read().clone()
    }
}

/// Test concurrent read/write with split locks
#[test]
fn test_split_lock_concurrent_read_write() {
    let cache = Arc::new(SplitCacheSim::new());

    // Writer thread
    let writer_cache = Arc::clone(&cache);
    let writer = thread::spawn(move || {
        for i in 0..SPLIT_LOCK_READ_WRITE_ITERATIONS {
            let data = vec![1u8; 64];
            writer_cache.save_video(&data, usize_to_u32(i));
        }
    });

    // Reader threads
    let reader_caches: Vec<_> = (0..4).map(|_| Arc::clone(&cache)).collect();
    let readers: Vec<_> = reader_caches
        .into_iter()
        .map(|c| {
            thread::spawn(move || {
                let mut last_ts = 0u32;
                for _ in 0..SPLIT_LOCK_READ_WRITE_ITERATIONS {
                    let (_, ts) = c.get_video();
                    // Timestamps should be monotonically increasing
                    // (or equal due to race conditions, which is fine)
                    assert!(ts >= last_ts || ts == 0);
                    last_ts = ts;
                }
            })
        })
        .collect();

    writer.join().unwrap();
    for r in readers {
        r.join().unwrap();
    }
}

/// Test no deadlock under heavy contention
#[test]
fn test_no_deadlock_under_contention() {
    let cache = Arc::new(SplitCacheSim::new());

    let handles: Vec<_> = (0..CONTENTION_THREADS)
        .map(|tid| {
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                for i in 0..CONTENTION_ITERATIONS {
                    let data = vec![usize_to_u8(tid); 32];
                    // Interleave operations to increase contention
                    let ts = usize_to_u32(i);
                    cache.save_video(&data, ts);
                    cache.save_audio(&data, ts);
                    cache.save_metadata(&data, ts);
                    // Also do reads
                    let _ = cache.get_video();
                    let _ = cache.get_audio();
                }
            })
        })
        .collect();

    // Set a timeout - if there's a deadlock, this will hang
    let result = thread::spawn(move || {
        for h in handles {
            h.join().unwrap();
        }
    });

    // Should complete within reasonable time
    // 16 threads * 5000 iterations * 5 ops = 400,000 operations
    // Even with contention, should complete in < 5 seconds
    let timeout = Duration::from_secs(10);
    let start = Instant::now();
    loop {
        if result.is_finished() {
            break;
        }
        assert!(
            start.elapsed() <= timeout,
            "Deadlock detected: test did not complete within {timeout:?}"
        );
        thread::sleep(Duration::from_millis(100));
    }
}

// ==================================================================
// Correctness Tests
// ==================================================================

/// Test that split cache maintains data integrity
#[test]
fn test_split_cache_data_integrity() {
    let cache = Arc::new(SplitCacheSim::new());

    let writer = Arc::clone(&cache);
    let h1 = thread::spawn(move || {
        for i in 0..100u32 {
            let data = vec![0xAA; i as usize + 1];
            writer.save_video(&data, i);
        }
    });

    let writer = Arc::clone(&cache);
    let h2 = thread::spawn(move || {
        for i in 0..100u32 {
            let data = vec![0xBB; i as usize + 1];
            writer.save_audio(&data, i);
        }
    });

    h1.join().unwrap();
    h2.join().unwrap();

    // Final state should have last written values
    let (video_data, video_ts) = cache.get_video();
    assert_eq!(video_data.len(), 100);
    assert!(video_data.iter().all(|&b| b == 0xAA));
    assert_eq!(video_ts, 99);

    let (audio_data, audio_ts) = cache.get_audio();
    assert_eq!(audio_data.len(), 100);
    assert!(audio_data.iter().all(|&b| b == 0xBB));
    assert_eq!(audio_ts, 99);
}

/// Test parking_lot RwLock read/write behavior
#[test]
fn test_parking_lot_rwlock_basic() {
    let lock = RwLock::new(0u32);

    // Multiple readers
    {
        let r1 = lock.read();
        let r2 = lock.read();
        assert_eq!(*r1, 0);
        assert_eq!(*r2, 0);
    }

    // Writer
    {
        let mut w = lock.write();
        *w = 42;
    }

    // Verify write
    {
        let r = lock.read();
        assert_eq!(*r, 42);
    }
}

/// Test RwLock upgrade/downgrade semantics
#[test]
fn test_rwlock_read_write交替() {
    let cache = SplitCacheSim::new();

    // Write then read
    cache.save_video(&[1, 2, 3], 100);
    let (data, ts) = cache.get_video();
    assert_eq!(data, vec![1, 2, 3]);
    assert_eq!(ts, 100);

    // Overwrite
    cache.save_video(&[4, 5, 6], 200);
    let (data, ts) = cache.get_video();
    assert_eq!(data, vec![4, 5, 6]);
    assert_eq!(ts, 200);
}

// ==================================================================
// Real SplitCache Tests
// ==================================================================

/// Test SplitCache metadata operations
#[test]
fn test_split_cache_metadata() {
    let cache = SplitCache::new(5, None, None);

    // Initially no metadata
    assert!(cache.get_metadata().is_none());

    // Save valid RTMP metadata (AMF0 format: "onMetaData" + ECMA array)
    // This is a simplified metadata: string marker + length + "onMetaData"
    let mut data = BytesMut::new();
    data.extend_from_slice(&[0x02]); // AMF0 string marker
    data.extend_from_slice(&[0x00, 0x0a]); // length 10
    data.extend_from_slice(b"onMetaData");
    cache.save_metadata(&data, 1000);

    // Retrieve metadata
    let meta = cache.get_metadata();
    assert!(
        meta.is_some(),
        "Metadata should be saved for valid onMetaData"
    );
    if let Some(FrameData::MetaData { timestamp, .. }) = meta {
        assert_eq!(timestamp, 1000);
    } else {
        panic!("Expected MetaData frame");
    }
}

/// Test SplitCache video sequence header operations
#[test]
fn test_split_cache_video_seq() {
    let cache = SplitCache::new(5, None, None);

    // Initially no video sequence
    assert!(cache.get_video_seq().is_none());

    // Create a simple video sequence header
    // Note: This is a minimal test - real H264 sequence headers are more complex
    let data = BytesMut::new();
    cache.save_video_data(&data, 0).ok(); // Empty data, should be ok

    // After saving video data, video_seq should still be None for non-sequence data
    assert!(cache.get_video_seq().is_none());
}

/// Test SplitCache audio sequence header operations
#[test]
fn test_split_cache_audio_seq() {
    let cache = SplitCache::new(5, None, None);

    // Initially no audio sequence
    assert!(cache.get_audio_seq().is_none());

    // Create a simple audio frame
    let data = BytesMut::new();
    cache.save_audio_data(&data, 0).ok(); // Empty data, should be ok

    // After saving audio data, audio_seq should still be None for non-sequence data
    assert!(cache.get_audio_seq().is_none());
}

/// Test SplitCache GOP operations
#[test]
fn test_split_cache_gops() {
    let cache = SplitCache::new(3, None, None);

    // Initially no GOPs (disabled when gop_num is 0)
    let cache_disabled = SplitCache::new(0, None, None);
    assert!(cache_disabled.get_gops_data().is_none());

    // With GOP enabled, should return some data even if empty
    let gops = cache.get_gops_data();
    assert!(gops.is_some());
}

/// Test concurrent SplitCache video/audio saves
#[test]
fn test_split_cache_concurrent_saves() {
    let cache = Arc::new(SplitCache::new(5, None, None));

    // Writer threads
    let cache_video = Arc::clone(&cache);
    let h1 = thread::spawn(move || {
        for i in 0..SPLIT_CACHE_CONCURRENT_SAVES_ITERATIONS {
            let mut data = BytesMut::new();
            data.extend_from_slice(&[usize_to_u8(i); 64]);
            cache_video.save_video_data(&data, usize_to_u32(i)).ok();
        }
    });

    let cache_audio = Arc::clone(&cache);
    let h2 = thread::spawn(move || {
        for i in 0..SPLIT_CACHE_CONCURRENT_SAVES_ITERATIONS {
            let mut data = BytesMut::new();
            data.extend_from_slice(&[usize_to_u8(i); 64]);
            cache_audio.save_audio_data(&data, usize_to_u32(i)).ok();
        }
    });

    // Reader thread - should not block writers
    let cache_reader = Arc::clone(&cache);
    let h3 = thread::spawn(move || {
        for _ in 0..SPLIT_CACHE_CONCURRENT_SAVES_ITERATIONS {
            // These reads should not block the writers significantly
            let _ = cache_reader.get_metadata();
            let _ = cache_reader.get_video_seq();
            let _ = cache_reader.get_audio_seq();
        }
    });

    h1.join().unwrap();
    h2.join().unwrap();
    h3.join().unwrap();

    // Verify GOPs have data
    let gops = cache.get_gops_data();
    assert!(gops.is_some());
    // Should have frames from both video and audio
    let total_frames: usize = gops
        .unwrap()
        .iter()
        .map(synctv_xiu::rtmp::cache::gop::Gop::len)
        .sum();
    assert!(total_frames > 0, "Should have saved frames");
}

/// Test SplitCache under high contention
#[test]
fn test_split_cache_high_contention() {
    let cache = Arc::new(SplitCache::new(10, None, None));

    let handles: Vec<_> = (0..HIGH_CONTENTION_THREADS)
        .map(|tid| {
            let cache = Arc::clone(&cache);
            thread::spawn(move || {
                for i in 0..HIGH_CONTENTION_ITERATIONS {
                    let mut data = BytesMut::new();
                    data.extend_from_slice(&[usize_to_u8(tid), usize_to_u8(i)]);
                    let ts = usize_to_u32(i);

                    match tid % 3 {
                        0 => cache.save_video_data(&data, ts).ok(),
                        1 => cache.save_audio_data(&data, ts).ok(),
                        _ => {
                            cache.save_metadata(&data, ts);
                            Some(())
                        }
                    };
                }
            })
        })
        .collect();

    // Set a timeout - if there's a deadlock, this will hang
    let timeout = Duration::from_secs(10);
    let start = Instant::now();

    for h in handles {
        while !h.is_finished() {
            assert!(
                start.elapsed() <= timeout,
                "Deadlock detected in SplitCache"
            );
            thread::sleep(Duration::from_millis(10));
        }
        h.join().unwrap();
    }

    // Verify cache is in a consistent state
    let _ = cache.get_metadata();
    let _ = cache.get_video_seq();
    let _ = cache.get_audio_seq();
    let gops = cache.get_gops_data();
    assert!(gops.is_some());
}
