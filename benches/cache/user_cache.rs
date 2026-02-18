//! Cache benchmarks for user caching operations
//!
//! Run with: cargo bench --bench user_cache

use criterion::{black_box, criterion_group, criterion_main, Criterion, BenchmarkId};
use synctv_core::cache::user_cache::{UserCache, CachedUser};
use synctv_core::models::{UserId, UserRole, UserStatus};
use std::time::Duration;
use chrono::Utc;

fn create_test_user(id: &str, username: &str) -> CachedUser {
    CachedUser::new(
        id.to_string(),
        username.to_string(),
        UserRole::User,
        UserStatus::Active,
        Utc::now(),
        0,
    )
}

/// Benchmark: L1 cache hit (in-memory)
fn bench_l1_cache_hit(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let cache = rt.block_on(async {
        let cache = UserCache::new(None, 10000, 5, 0, "test:".to_string()).unwrap();
        let user_id = UserId::from_string("user1".to_string());
        let user = create_test_user("user1", "alice");
        cache.set(&user_id, user).await.unwrap();
        cache
    });

    let user_id = UserId::from_string("user1".to_string());

    c.bench_function("l1_cache_hit", |b| {
        b.to_async(&rt).iter(|| {
            let uid = user_id.clone();
            let c = cache.clone();
            async move {
                let result = c.get(&uid).await.unwrap();
                black_box(result);
            }
        })
    });
}

/// Benchmark: L1 cache miss
fn bench_l1_cache_miss(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let cache = rt.block_on(async {
        UserCache::new(None, 10000, 5, 0, "test:".to_string()).unwrap()
    });

    let user_id = UserId::from_string("nonexistent".to_string());

    c.bench_function("l1_cache_miss", |b| {
        b.to_async(&rt).iter(|| {
            let uid = user_id.clone();
            let c = cache.clone();
            async move {
                let result = c.get(&uid).await.unwrap();
                black_box(result);
            }
        })
    });
}

/// Benchmark: Batch lookup with varying sizes
fn bench_batch_lookup(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let cache = rt.block_on(async {
        let cache = UserCache::new(None, 10000, 5, 0, "test:".to_string()).unwrap();
        for i in 0..200 {
            let user_id = UserId::from_string(format!("user{}", i));
            let user = create_test_user(&format!("user{}", i), &format!("user{}", i));
            cache.set(&user_id, user).await.unwrap();
        }
        cache
    });

    let mut group = c.benchmark_group("batch_lookup");
    group.measurement_time(Duration::from_secs(10));

    for batch_size in [10, 50, 100, 200].iter() {
        let user_ids: Vec<UserId> = (0..*batch_size)
            .map(|i| UserId::from_string(format!("user{}", i)))
            .collect();

        group.bench_with_input(BenchmarkId::from_parameter(batch_size), batch_size, |b, &_batch_size| {
            b.to_async(&rt).iter(|| {
                let ids = user_ids.clone();
                let c = cache.clone();
                async move {
                    let result = c.get_batch(&ids).await.unwrap();
                    black_box(result);
                }
            })
        });
    }

    group.finish();
}

/// Benchmark: Cache set
fn bench_cache_set(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let cache = rt.block_on(async {
        UserCache::new(None, 10000, 5, 0, "test:".to_string()).unwrap()
    });

    let mut group = c.benchmark_group("cache_set");
    group.measurement_time(Duration::from_secs(5));

    for (i, (user_id, username)) in [
        ("user1", "alice"),
        ("user2", "bob"),
        ("user3", "charlie"),
    ].iter().enumerate()
    {
        group.bench_with_input(BenchmarkId::from_parameter(i), &i, |b, &_i| {
            let user_id = UserId::from_string(user_id.to_string());
            let user = create_test_user(user_id.as_str(), username);

            b.to_async(&rt).iter(|| {
                let uid = user_id.clone();
                let u = user.clone();
                let c = cache.clone();
                async move {
                    c.set(&uid, u).await.unwrap();
                }
            })
        });
    }

    group.finish();
}

/// Benchmark: Cache invalidate
fn bench_cache_invalidate(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let cache = rt.block_on(async {
        let cache = UserCache::new(None, 10000, 5, 0, "test:".to_string()).unwrap();
        let user_id = UserId::from_string("user1".to_string());
        let user = create_test_user("user1", "alice");
        cache.set(&user_id, user).await.unwrap();
        cache
    });

    let user_id = UserId::from_string("user1".to_string());

    c.bench_function("cache_invalidate", |b| {
        b.to_async(&rt).iter(|| {
            let uid = user_id.clone();
            let c = cache.clone();
            async move {
                c.invalidate(&uid).await.unwrap();
                // Re-populate for next iteration
                let user = create_test_user("user1", "alice");
                c.set(&uid, user).await.unwrap();
            }
        })
    });
}

/// Benchmark: Concurrent access
fn bench_concurrent_access(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let cache = rt.block_on(async {
        let cache = UserCache::new(None, 10000, 5, 0, "test:".to_string()).unwrap();
        for i in 0..100 {
            let user_id = UserId::from_string(format!("user{}", i));
            let user = create_test_user(&format!("user{}", i), &format!("user{}", i));
            cache.set(&user_id, user).await.unwrap();
        }
        cache
    });
    let cache = std::sync::Arc::new(cache);

    let mut group = c.benchmark_group("concurrent_access");
    group.measurement_time(Duration::from_secs(5));

    for num_tasks in [10, 50, 100].iter() {
        group.bench_with_input(BenchmarkId::from_parameter(num_tasks), num_tasks, |b, &num_tasks| {
            b.to_async(&rt).iter(|| {
                let cache = cache.clone();
                async move {
                    let mut tasks = Vec::new();
                    for i in 0..num_tasks {
                        let cache = cache.clone();
                        let user_id = UserId::from_string(format!("user{}", i % 100));
                        tasks.push(tokio::spawn(async move {
                            let result = cache.get(&user_id).await.unwrap();
                            black_box(result);
                        }));
                    }
                    for task in tasks {
                        task.await.unwrap();
                    }
                }
            })
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_l1_cache_hit,
    bench_l1_cache_miss,
    bench_batch_lookup,
    bench_cache_set,
    bench_cache_invalidate,
    bench_concurrent_access
);
criterion_main!(benches);
