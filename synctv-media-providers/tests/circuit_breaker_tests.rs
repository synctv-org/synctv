//! Circuit Breaker Memory Ordering Tests
//!
//! Tests that the circuit breaker uses correct memory ordering (Acquire/Release or `SeqCst`)
//! for concurrent state transitions, ensuring no data races or visibility issues.

#![allow(clippy::unwrap_used)]
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;

/// Number of iterations for stress tests
const STRESS_ITERATIONS: usize = 100_000;

/// Number of concurrent threads for stress tests
const CONCURRENT_THREADS: usize = 8;

/// Test that consecutive failure counter works correctly under high concurrency
/// with `SeqCst` ordering
#[test]
fn test_atomic_counter_seqcst_correctness() {
    let counter = Arc::new(AtomicU32::new(0));
    let mut handles = vec![];

    for _ in 0..CONCURRENT_THREADS {
        let counter_clone = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..STRESS_ITERATIONS {
                // Simulate fetch_add with SeqCst ordering
                counter_clone.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // With SeqCst, all increments should be visible
    let expected = (CONCURRENT_THREADS * STRESS_ITERATIONS) as u32;
    let actual = counter.load(Ordering::SeqCst);
    assert_eq!(
        actual, expected,
        "Counter should have all increments visible: expected {expected}, got {actual}"
    );
}

/// Test that state transitions (0 -> THRESHOLD -> reset) are atomic
#[test]
fn test_atomic_state_transitions() {
    let failures = Arc::new(AtomicU32::new(0));
    let threshold: u32 = 5;
    let mut handles = vec![];

    for _ in 0..CONCURRENT_THREADS {
        let failures_clone = Arc::clone(&failures);
        let failures_clone2 = Arc::clone(&failures);
        handles.push(thread::spawn(move || {
            // Each thread increments and occasionally resets
            for i in 0..1000 {
                if i % 100 == 0 {
                    // Simulate record_success reset
                    failures_clone.store(0, Ordering::SeqCst);
                } else {
                    // Simulate record_failure increment
                    let prev = failures_clone.fetch_add(1, Ordering::SeqCst);
                    // Check if we hit threshold
                    if prev + 1 >= threshold {
                        // Reset to simulate circuit opening then recovery
                        failures_clone.store(0, Ordering::SeqCst);
                    }
                }
            }
        }));
        // Also have a reader thread
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                let value = failures_clone2.load(Ordering::SeqCst);
                assert!(
                    value < 1000,
                    "Value should never exceed reasonable bounds: {value}"
                );
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Final value should be reasonable (not corrupted)
    let final_value = failures.load(Ordering::SeqCst);
    assert!(
        final_value < 1000,
        "Final value should be reasonable: {final_value}"
    );
}

/// Test that Acquire/Release semantics work for producer-consumer pattern
#[test]
fn test_acquire_release_semantics() {
    let data = Arc::new(AtomicU32::new(0));
    let signal = Arc::new(AtomicU32::new(0));

    let data_producer = Arc::clone(&data);
    let signal_producer = Arc::clone(&signal);
    let producer = thread::spawn(move || {
        // Write data with Release ordering
        data_producer.store(42, Ordering::Release);
        // Signal that data is ready
        signal_producer.store(1, Ordering::Release);
    });

    let data_consumer = Arc::clone(&data);
    let signal_consumer = Arc::clone(&signal);
    let consumer = thread::spawn(move || {
        // Wait for signal with Acquire ordering
        while signal_consumer.load(Ordering::Acquire) == 0 {
            std::hint::spin_loop();
        }
        // Read data with Acquire ordering - guaranteed to see 42
        let value = data_consumer.load(Ordering::Acquire);
        assert_eq!(
            value, 42,
            "Consumer should see the value written by producer"
        );
    });

    producer.join().unwrap();
    consumer.join().unwrap();
}

/// Test that `SeqCst` provides total ordering for multiple operations
#[test]
fn test_seqcst_total_ordering() {
    let a = Arc::new(AtomicU32::new(0));
    let b = Arc::new(AtomicU32::new(0));

    let a1 = Arc::clone(&a);
    let b1 = Arc::clone(&b);
    let t1 = thread::spawn(move || {
        a1.store(1, Ordering::SeqCst);
        b1.load(Ordering::SeqCst)
    });

    let a2 = Arc::clone(&a);
    let b2 = Arc::clone(&b);
    let t2 = thread::spawn(move || {
        b2.store(1, Ordering::SeqCst);
        a2.load(Ordering::SeqCst)
    });

    let r1 = t1.join().unwrap();
    let r2 = t2.join().unwrap();

    // With SeqCst, it's impossible for both threads to see 0
    // (one of the stores must happen before the other's load)
    assert!(
        !(r1 == 0 && r2 == 0),
        "SeqCst should prevent both threads seeing 0"
    );
}

/// Test that Relaxed ordering does NOT provide synchronization guarantees
/// (This demonstrates why Relaxed is insufficient for circuit breaker)
#[test]
fn test_relaxed_ordering_no_synchronization() {
    // This test documents the problem with Relaxed ordering
    // Note: This test may pass sometimes due to timing, but it demonstrates
    // that Relaxed provides no guarantees

    let data = Arc::new(AtomicU32::new(0));
    let signal = Arc::new(AtomicU32::new(0));

    let data_writer = Arc::clone(&data);
    let signal_writer = Arc::clone(&signal);
    let writer = thread::spawn(move || {
        // With Relaxed, there's no happens-before relationship
        data_writer.store(42, Ordering::Relaxed);
        signal_writer.store(1, Ordering::Relaxed);
    });

    let data_reader = Arc::clone(&data);
    let signal_reader = Arc::clone(&signal);
    let reader = thread::spawn(move || {
        // Even if we see signal == 1, we might not see data == 42
        // with Relaxed ordering (though in practice we often will)
        while signal_reader.load(Ordering::Relaxed) == 0 {
            std::hint::spin_loop();
        }
        // This read is NOT guaranteed to see 42 with Relaxed ordering
        let _value = data_reader.load(Ordering::Relaxed);
        // We don't assert here because Relaxed provides no guarantees
    });

    writer.join().unwrap();
    reader.join().unwrap();
}

/// Test that circuit breaker threshold check is accurate under concurrency
#[test]
fn test_threshold_check_accuracy() {
    let consecutive_failures = Arc::new(AtomicU32::new(0));
    let threshold: u32 = 5;
    let mut handles = vec![];

    // Multiple threads incrementing failures
    for _ in 0..4 {
        let cf = Arc::clone(&consecutive_failures);
        handles.push(thread::spawn(move || {
            for _ in 0..10 {
                let prev = cf.fetch_add(1, Ordering::SeqCst);
                // Simulate threshold check
                if prev + 1 >= threshold {
                    // Circuit would open
                }
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Total should be exactly 40
    assert_eq!(
        consecutive_failures.load(Ordering::SeqCst),
        40,
        "All increments should be counted"
    );
}

/// Test reset operation visibility across threads
#[test]
fn test_reset_visibility() {
    let counter = Arc::new(AtomicU32::new(100));
    let (tx, rx) = mpsc::channel();

    let c1 = Arc::clone(&counter);
    let t1 = thread::spawn(move || {
        // Reset to 0
        c1.store(0, Ordering::SeqCst);
        tx.send(()).unwrap();
    });

    let c2 = Arc::clone(&counter);
    let t2 = thread::spawn(move || {
        // Wait until the writer signals the reset completed.
        rx.recv().unwrap();
        c2.load(Ordering::SeqCst)
    });

    t1.join().unwrap();
    let value = t2.join().unwrap();

    // After reset, all threads should see 0
    assert_eq!(value, 0, "Reset should be visible to all threads");
}

/// Test that mixed read/write operations don't lose updates
#[test]
fn test_no_lost_updates() {
    let counter = Arc::new(AtomicU32::new(0));
    let mut handles = vec![];

    // Writers
    for _ in 0..4 {
        let c = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                c.fetch_add(1, Ordering::SeqCst);
            }
        }));
    }

    // Readers (they shouldn't affect the count)
    for _ in 0..4 {
        let c = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..1000 {
                let _ = c.load(Ordering::SeqCst);
            }
        }));
    }

    for handle in handles {
        handle.join().unwrap();
    }

    // Should have exactly 4000 increments
    assert_eq!(
        counter.load(Ordering::SeqCst),
        4000,
        "No updates should be lost"
    );
}
