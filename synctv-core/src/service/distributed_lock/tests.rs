use super::{
    run_distributed_lock_client_op, run_distributed_lock_redis_op, DistributedLock, Error,
};
use crate::test_helpers::failing_redis_runtime;
use std::sync::Arc;

#[tokio::test]
async fn test_distributed_lock_accepts_trait_object_runtime() {
    let runtime = failing_redis_runtime();
    let lock = DistributedLock::from_runtime(runtime.clone());

    assert!(
        Arc::ptr_eq(&lock.redis_runtime, &runtime),
        "distributed lock should retain the injected Redis runtime object"
    );
}

#[test]
fn test_distributed_lock_from_runtime_with_mode_retains_injected_runtime() {
    let runtime = failing_redis_runtime();
    let lock = DistributedLock::from_runtime_with_mode(runtime.clone(), false);

    assert!(
        Arc::ptr_eq(&lock.redis_runtime, &runtime),
        "distributed lock should retain the injected runtime even in deployment-aware mode"
    );
}

#[tokio::test(start_paused = true)]
async fn test_distributed_lock_redis_timeout_maps_to_timeout_error() {
    let timeout_future = run_distributed_lock_redis_op(
        crate::resilience::timeout::REDIS_OPERATION_TIMEOUT,
        "acquire lock",
        async { std::future::pending::<std::result::Result<(), redis::RedisError>>().await },
    );

    tokio::pin!(timeout_future);
    tokio::task::yield_now().await;
    tokio::time::advance(crate::resilience::timeout::REDIS_OPERATION_TIMEOUT).await;

    let err = timeout_future.await.expect_err("operation should time out");
    assert!(matches!(
        err,
        Error::Timeout(ref msg) if msg == "Redis timeout: acquire lock"
    ));
}

#[tokio::test(start_paused = true)]
async fn test_distributed_lock_client_timeout_maps_to_timeout_error() {
    let timeout_future =
        run_distributed_lock_client_op("test-key", std::time::Duration::from_secs(15), async {
            std::future::pending::<Result<(), Error>>().await
        });

    tokio::pin!(timeout_future);
    tokio::task::yield_now().await;
    tokio::time::advance(std::time::Duration::from_secs(15)).await;

    let err = timeout_future.await.expect_err("operation should time out");
    assert!(matches!(
        err,
        Error::Timeout(ref msg) if msg == "Lock operation timed out for key: test-key"
    ));
}
