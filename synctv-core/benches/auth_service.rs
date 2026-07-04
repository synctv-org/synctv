//! Service benchmarks for authentication operations
//!
//! Run with: cargo bench -p synctv-core --bench `auth_service`

#![allow(clippy::unwrap_used)]
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;
use synctv_core::models::UserId;
use synctv_core::service::{JwtService, OpaquePasswordService};

/// Benchmark: JWT token generation
fn bench_jwt_sign(c: &mut Criterion) {
    let jwt_service = JwtService::new("benchmark-secret-key-long-enough-for-entropy-1234567890")
        .expect("Failed to create JwtService");
    let user_id = UserId::expect_positive(1_000_000_001);

    c.bench_function("jwt_sign_access_token", |b| {
        b.iter(|| {
            let token = jwt_service
                .sign_access_token(black_box(&user_id), 0)
                .expect("sign failed");
            black_box(token);
        });
    });
}

/// Benchmark: JWT token verification
fn bench_jwt_verify(c: &mut Criterion) {
    let jwt_service = JwtService::new("benchmark-secret-key-long-enough-for-entropy-1234567890")
        .expect("Failed to create JwtService");
    let user_id = UserId::expect_positive(1_000_000_001);

    let token = jwt_service
        .sign_access_token(&user_id, 0)
        .expect("sign failed");

    c.bench_function("jwt_verify_access_token", |b| {
        b.iter(|| {
            let claims = jwt_service
                .verify_access_token(black_box(&token))
                .expect("verify failed");
            black_box(claims);
        });
    });
}

/// Benchmark: OPAQUE password registration
fn bench_opaque_password_registration(c: &mut Criterion) {
    let opaque_password = OpaquePasswordService::derive_from_secret(b"benchmark-opaque-secret");

    let mut group = c.benchmark_group("opaque_password_registration");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    group.bench_function("register_password", |b| {
        b.iter(|| {
            let record = opaque_password
                .register_password(
                    black_box(b"bench:user:password"),
                    black_box("bench_password_123!"),
                )
                .expect("OPAQUE registration failed");
            black_box(record);
        });
    });

    group.finish();
}

/// Benchmark: OPAQUE password verification
fn bench_opaque_password_verification(c: &mut Criterion) {
    let opaque_password = OpaquePasswordService::derive_from_secret(b"benchmark-opaque-secret");
    let record = opaque_password
        .register_password(b"bench:user:password", "bench_password_123!")
        .expect("OPAQUE registration failed");

    let mut group = c.benchmark_group("opaque_password_verification");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    group.bench_function("verify_password", |b| {
        b.iter(|| {
            let result = opaque_password
                .verify_password(black_box(&record), black_box("bench_password_123!"))
                .expect("OPAQUE verification failed");
            black_box(result);
        });
    });

    group.finish();
}

/// Benchmark: Concurrent token generation
fn bench_concurrent_token_generation(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let jwt_service = JwtService::new("benchmark-secret-key-long-enough-for-entropy-1234567890")
        .expect("Failed to create JwtService");
    let jwt_service = std::sync::Arc::new(jwt_service);

    let mut group = c.benchmark_group("concurrent_token_generation");
    group.measurement_time(Duration::from_secs(5));

    for num_concurrent in &[10, 50, 100] {
        group.bench_with_input(
            BenchmarkId::from_parameter(num_concurrent),
            num_concurrent,
            |b, &num_concurrent| {
                b.iter(|| {
                    rt.block_on(async {
                        let mut tasks = Vec::new();
                        for i in 0..num_concurrent {
                            let jwt_service = jwt_service.clone();
                            tasks.push(tokio::spawn(async move {
                                let user_id = UserId::expect_positive(1_000_000_000 + i);
                                let token = jwt_service
                                    .sign_access_token(&user_id, 0)
                                    .expect("sign failed");
                                black_box(token);
                            }));
                        }
                        for task in tasks {
                            task.await.unwrap();
                        }
                    });
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_jwt_sign,
    bench_jwt_verify,
    bench_opaque_password_registration,
    bench_opaque_password_verification,
    bench_concurrent_token_generation
);
criterion_main!(benches);
