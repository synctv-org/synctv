# Performance Benchmarks

This directory contains performance benchmarks for the SyncTV Rust implementation.

## Running Benchmarks

Run all registered workspace benchmarks:
```bash
cargo bench
```

Run registered `synctv-core` benchmarks:
```bash
cargo bench -p synctv-core --bench auth_service
cargo bench -p synctv-core --bench database_benchmarks
```

## Benchmark Structure

```
benches/
├── database/
│   └── room_queries.rs     # Historical room query Criterion benchmark
├── cache/
│   └── user_cache.rs       # Historical user cache Criterion benchmark
├── service/
│   └── auth_service.rs     # Stub; moved to synctv-core/benches/auth_service.rs
└── README.md

synctv-core/benches/
├── auth_service.rs         # Registered auth service benchmark
└── database_benchmarks.rs  # Registered database benchmark
```

The workspace currently registers `synctv-core` benchmarks through
`synctv-core/Cargo.toml`. The files under the repository-level `benches/`
directory are not registered by the virtual workspace manifest.

## Understanding Results

Benchmark results are saved to `target/criterion/`. Open `target/criterion/report/index.html` in a web browser to view detailed results.

## Key Metrics

Target performance metrics:
- **API response time P99**: < 200ms
- **Database query time P99**: < 50ms
- **Cache hit rate**: > 80%
- **WebSocket message latency**: < 100ms
- **Concurrent connections**: > 10,000

## Adding New Benchmarks

When adding new benchmarks:

1. Follow the existing structure
2. Use Criterion's benchmark groups for related benchmarks
3. Add appropriate measurement times
4. Include both cold and warm cache scenarios
5. Test with varying data sizes
6. Include concurrent access patterns

Example:
```rust
use criterion::{black_box, criterion_group, criterion_main, Criterion};

fn bench_my_function(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();

    c.bench_function("my_function", |b| {
        b.to_async(&rt).iter(|| {
            async {
                // Your benchmark code here
                my_function().await
            }
        })
    });
}

criterion_group!(benches, bench_my_function);
criterion_main!(benches);
```

## Performance Tips

Based on benchmark results, focus optimization efforts on:

1. **Hot paths**: Functions called frequently (e.g., message routing, cache lookups)
2. **I/O bottlenecks**: Database queries, network calls
3. **Memory allocations**: Reduce allocations in hot paths
4. **Lock contention**: Use concurrent data structures where appropriate
5. **Batch operations**: Aggregate operations when possible
