//! Core error helper tests (no Docker needed).

use synctv_core::Error;

fn ok<T, E: std::fmt::Debug>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => std::panic::panic_any(format!("{context}: {error:?}")),
    }
}

fn err<T: std::fmt::Debug, E>(result: Result<T, E>, context: &str) -> E {
    match result {
        Ok(value) => std::panic::panic_any(format!("{context}: {value:?}")),
        Err(error) => error,
    }
}

#[test]
fn test_internal_ext_maps_error() {
    use synctv_core::error::InternalExt;

    let result: Result<(), std::io::Error> = Err(std::io::Error::other("disk full"));

    let mapped = result.internal("Failed to write file");
    assert!(mapped.is_err());
    match err(mapped, "internal mapping should fail") {
        Error::Internal(msg) => assert_eq!(msg, "Failed to write file"),
        other => std::panic::panic_any(format!("Expected Internal, got: {other:?}")),
    }
}

#[test]
fn test_internal_ext_with_err_includes_cause() {
    use synctv_core::error::InternalExt;

    let result: Result<(), std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "access denied",
    ));

    let mapped = result.internal_with_err("Failed to read config");
    assert!(mapped.is_err());
    match err(mapped, "internal_with_err mapping should fail") {
        Error::Internal(msg) => {
            assert!(msg.contains("Failed to read config"));
            assert!(msg.contains("access denied"));
        }
        other => std::panic::panic_any(format!("Expected Internal, got: {other:?}")),
    }
}

#[test]
fn test_internal_ext_preserves_ok() {
    use synctv_core::error::InternalExt;

    let result: Result<i32, std::io::Error> = Ok(42);
    let mapped = result.internal("should not happen");
    assert_eq!(ok(mapped, "internal mapping should preserve Ok"), 42);
}

// From<anyhow::Error> preserves error chain

#[test]
fn test_anyhow_error_preserves_chain() {
    let inner = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
    let anyhow_err = anyhow::anyhow!(inner).context("loading config");

    let core_err: Error = anyhow_err.into();
    match core_err {
        Error::Internal(msg) => {
            assert!(
                msg.contains("loading config"),
                "Should contain context: {msg}"
            );
            assert!(
                msg.contains("file missing"),
                "Should contain root cause: {msg}"
            );
        }
        other => std::panic::panic_any(format!("Expected Internal, got: {other:?}")),
    }
}
