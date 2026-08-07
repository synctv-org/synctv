use super::*;
use std::thread;

type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[test]
fn half_open_allows_only_one_probe_request() -> TestResult {
    let breaker = CircuitBreaker::new();
    breaker
        .consecutive_failures
        .store(CIRCUIT_BREAKER_THRESHOLD, Ordering::SeqCst);
    breaker.opened_at.store(
        unix_now() - CIRCUIT_BREAKER_TIMEOUT_SECS - 1,
        Ordering::SeqCst,
    );

    let mut handles = Vec::new();
    for _ in 0..8 {
        let breaker = Arc::clone(&breaker);
        handles.push(thread::spawn(move || breaker.allow_request()));
    }

    let allowed = handles
        .into_iter()
        .map(std::thread::JoinHandle::join)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| anyhow::anyhow!("probe thread panicked"))?
        .into_iter()
        .filter(|allowed| *allowed)
        .count();

    assert_eq!(
        allowed, 1,
        "half-open state must allow exactly one probe request"
    );
    Ok(())
}

#[test]
fn failed_half_open_probe_reopens_circuit_immediately() {
    let breaker = CircuitBreaker::new();
    breaker
        .consecutive_failures
        .store(CIRCUIT_BREAKER_THRESHOLD, Ordering::SeqCst);
    breaker.opened_at.store(
        unix_now() - CIRCUIT_BREAKER_TIMEOUT_SECS - 1,
        Ordering::SeqCst,
    );

    assert!(breaker.allow_request(), "probe request should be allowed");
    breaker.record_failure("alist");

    assert!(
        !breaker.allow_request(),
        "failed probe must reopen the circuit immediately"
    );
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs().cast_signed())
}
