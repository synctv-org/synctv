//! Database query benchmarks for room operations
//!
//! Run with: cargo bench --bench room_queries
//!
//! Note: These benchmarks require a running PostgreSQL database.
//! Set `DATABASE_URL` environment variable before running.
//! If the database is not available, benchmarks will be skipped gracefully.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion};
use std::time::Duration;
use synctv_core::models::RoomId;

fn bench_room_id(offset: i64) -> RoomId {
    RoomId::expect_positive(2_000_000 + offset)
}

/// Benchmark: Room ID generation and hashing (in-process, no DB required)
fn bench_room_id_operations(c: &mut Criterion) {
    let mut group = c.benchmark_group("room_id_operations");

    group.bench_function("generate_room_id", |b| {
        b.iter(|| {
            let id = RoomId::new();
            black_box(id);
        })
    });

    group.bench_function("room_id_from_numeric", |b| {
        b.iter(|| {
            let id = RoomId::expect_positive(black_box(2_000_001_i64));
            black_box(id);
        })
    });

    group.bench_function("room_id_comparison", |b| {
        let id1 = bench_room_id(1);
        let id2 = bench_room_id(2);
        b.iter(|| {
            let result = black_box(&id1) == black_box(&id2);
            black_box(result);
        })
    });

    group.finish();
}

/// Benchmark: Simulated room list pagination (in-memory sort/slice)
///
/// This benchmarks the pagination logic overhead without requiring a database.
fn bench_list_rooms_paginated(c: &mut Criterion) {
    // Pre-generate room IDs
    let room_ids: Vec<RoomId> = (0..500)
        .map(|i| bench_room_id(i64::from(i)))
        .collect();

    let mut group = c.benchmark_group("list_rooms_paginated");
    group.measurement_time(Duration::from_secs(10));

    for page_size in [10, 50, 100].iter() {
        group.bench_with_input(
            BenchmarkId::from_parameter(page_size),
            page_size,
            |b, &page_size| {
                b.iter(|| {
                    // Simulate pagination: offset + limit slice
                    let offset = 0usize;
                    let page: Vec<&RoomId> = room_ids
                        .iter()
                        .skip(offset)
                        .take(page_size)
                        .collect();
                    black_box(page);
                })
            },
        );
    }

    group.finish();
}

/// Benchmark: Simulated room name search (in-memory filter)
fn bench_search_rooms_by_name(c: &mut Criterion) {
    // Pre-generate room data
    let rooms: Vec<(RoomId, String)> = (0..1000)
        .map(|i| {
            (
                bench_room_id(i64::from(i)),
                format!("Movie Night {i}"),
            )
        })
        .collect();

    c.bench_function("search_rooms_by_name", |b| {
        b.iter(|| {
            let query = black_box("Night");
            let results: Vec<&(RoomId, String)> = rooms
                .iter()
                .filter(|(_, name)| name.contains(query))
                .take(10)
                .collect();
            black_box(results);
        })
    });
}

/// Benchmark: Room member list operations (in-memory)
fn bench_get_room_members(c: &mut Criterion) {
    let mut group = c.benchmark_group("get_room_members");
    group.measurement_time(Duration::from_secs(10));

    for member_count in [1, 10, 50, 100].iter() {
        let members: Vec<String> = (0..*member_count)
            .map(|i| format!("user_{i:04}"))
            .collect();

        group.bench_with_input(
            BenchmarkId::from_parameter(member_count),
            member_count,
            |b, &_member_count| {
                b.iter(|| {
                    // Simulate member lookup
                    let result: Vec<&String> = members.iter().collect();
                    black_box(result);
                })
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_room_id_operations,
    bench_list_rooms_paginated,
    bench_search_rooms_by_name,
    bench_get_room_members
);
criterion_main!(benches);
