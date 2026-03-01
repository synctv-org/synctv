//! Service benchmarks for authentication operations
//!
//! Run with: cargo bench -p synctv-core --bench `auth_service`

#![allow(clippy::unwrap_used)]
use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use synctv_core::models::UserId;
use synctv_core::service::auth::{hash_password, verify_password, JwtService, TokenType};
use std::time::Duration;

/// Benchmark: JWT token generation
fn bench_jwt_sign(c: &mut Criterion) {
    let jwt_service = JwtService::new("benchmark-secret-key-long-enough-for-entropy-1234567890").expect("Failed to create JwtService");
    let user_id = UserId::from_string("bench_user_001".to_string());

    c.bench_function("jwt_sign_access_token", |b| {
        b.iter(|| {
            let token = jwt_service
                .sign_token(black_box(&user_id), TokenType::Access, 0)
                .expect("sign failed");
            black_box(token);
        });
    });
}

/// Benchmark: JWT token verification
fn bench_jwt_verify(c: &mut Criterion) {
    let jwt_service = JwtService::new("benchmark-secret-key-long-enough-for-entropy-1234567890").expect("Failed to create JwtService");
    let user_id = UserId::from_string("bench_user_001".to_string());

    let token = jwt_service
        .sign_token(&user_id, TokenType::Access, 0)
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

/// Benchmark: Password hashing (argon2)
fn bench_password_hash(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let mut group = c.benchmark_group("password_hash");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    group.bench_function("hash_password", |b| {
        b.iter(|| {
            let hash = rt.block_on(async {
                hash_password(black_box("bench_password_123!"))
                    .await
                    .expect("hash failed")
            });
            black_box(hash);
        });
    });

    group.finish();
}

/// Benchmark: Password verification
fn bench_password_verify(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let hash = rt.block_on(async {
        hash_password("bench_password_123!")
            .await
            .expect("hash failed")
    });

    let mut group = c.benchmark_group("password_verify");
    group.sample_size(10);
    group.measurement_time(Duration::from_secs(20));

    group.bench_function("verify_password", |b| {
        b.iter(|| {
            let result = rt.block_on(async {
                verify_password(black_box("bench_password_123!"), black_box(&hash))
                    .await
                    .expect("verify failed")
            });
            black_box(result);
        });
    });

    group.finish();
}

/// Benchmark: Concurrent token generation
fn bench_concurrent_token_generation(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    let jwt_service = JwtService::new("benchmark-secret-key-long-enough-for-entropy-1234567890").expect("Failed to create JwtService");
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
                                let user_id =
                                    UserId::from_string(format!("bench_user_{i:03}"));
                                let token = jwt_service
                                    .sign_token(&user_id, TokenType::Access, 0)
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
    bench_password_hash,
    bench_password_verify,
    bench_concurrent_token_generation
);
criterion_main!(benches);
