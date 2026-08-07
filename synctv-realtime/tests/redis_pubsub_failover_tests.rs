//! Redis Pub/Sub Sentinel failover classification tests.

#![allow(clippy::unwrap_used)]

use synctv_realtime::sync::is_sentinel_failover_error;

#[test]
fn detects_readonly_failover_error() {
    let err = anyhow::anyhow!("READONLY You can't write against a read only replica.");
    assert!(is_sentinel_failover_error(&err));
}

#[test]
fn detects_loading_failover_error() {
    let err = anyhow::anyhow!("LOADING Redis is loading the dataset in memory");
    assert!(is_sentinel_failover_error(&err));
}

#[test]
fn ignores_unrelated_redis_errors() {
    for message in [
        "Connection refused",
        "ERR unknown command 'foo'",
        "NOSCRIPT No matching script",
        "",
    ] {
        let err = anyhow::anyhow!(message);
        assert!(!is_sentinel_failover_error(&err), "{message}");
    }
}

#[test]
fn scans_anyhow_error_chain() {
    let err = anyhow::Context::context(
        Err::<(), _>(anyhow::anyhow!(
            "READONLY You can't write against a read only replica."
        )),
        "Failed to publish event",
    )
    .unwrap_err();

    assert!(is_sentinel_failover_error(&err));
}

#[test]
fn keeps_redis_error_markers_case_sensitive() {
    for message in ["readonly you can't write", "ReadOnly mode active"] {
        let err = anyhow::anyhow!(message);
        assert!(!is_sentinel_failover_error(&err), "{message}");
    }
}
