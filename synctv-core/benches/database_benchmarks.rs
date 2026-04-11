//! Database performance benchmarks with real `PostgreSQL`
//!
//! Run with: cargo bench --bench `database_benchmarks`
//!
//! Note: These benchmarks use testcontainers to start a `PostgreSQL` container.
//! They require Docker to be running.
//!
//! Performance targets:
//! - Single row query: < 1ms
//! - Paginated list query (100 items): < 10ms
//! - Batch insert (100 items): < 100ms
//! - Index lookup: < 1ms

#![allow(clippy::unwrap_used)]
use std::hint::black_box;
use std::time::{Duration, Instant};

use chrono::Utc;
use criterion::{criterion_group, BenchmarkId, Criterion, Throughput};
use sqlx::PgPool;
use synctv_core::{
    bench_support,
    models::{
        id::PlaylistId, media::Media, playlist::Playlist, MediaId, PageParams, Room, RoomId,
        RoomListQuery, RoomMember, RoomRole, RoomStatus, User, UserId, UserRole, UserStatus,
    },
    repository::{
        MediaRepository, PlaylistRepository, RoomMemberRepository, RoomRepository, UserRepository,
    },
};
use testcontainers::core::ImageExt;
use testcontainers::runners::AsyncRunner;
use testcontainers::ContainerAsync;
use testcontainers_modules::postgres::Postgres;

const POSTGRES_VERSION: &str = "18";

struct BenchmarkDatabase {
    container: ContainerAsync<Postgres>,
    pool: PgPool,
}

impl BenchmarkDatabase {
    fn pool(&self) -> &PgPool {
        &self.pool
    }

    fn shutdown(self, rt: &tokio::runtime::Runtime) {
        rt.block_on(async move {
            self.pool.close().await;
            drop(self.container);
        });
    }
}

fn print_benchmark_list() {
    for room_count in [100, 500, 1000] {
        println!("list_rooms/paginated/{room_count}: benchmark");
    }

    for media_count in [100, 500, 1000] {
        println!("list_media/paginated/{media_count}: benchmark");
    }

    for batch_size in [10, 50, 100] {
        println!("batch_insert/media/{batch_size}: benchmark");
    }

    println!("index_effectiveness/get_by_room_indexed: benchmark");
    println!("single_row_operations/create_room: benchmark");
    println!("single_row_operations/get_room_by_id: benchmark");

    for page in [1u32, 5, 25, 45] {
        println!("pagination_offset/page/{page}: benchmark");
    }
}

fn main() {
    if bench_support::is_nextest_list_mode() {
        print_benchmark_list();
        return;
    }

    benches();
    Criterion::default().configure_from_args().final_summary();
}

/// Setup test database with testcontainers
async fn setup_test_db() -> BenchmarkDatabase {
    let postgres = Postgres::default()
        .with_db_name("synctv_bench")
        .with_user("synctv")
        .with_password("synctv_bench")
        .with_tag(POSTGRES_VERSION)
        .start()
        .await
        .expect("Failed to start Postgres container");

    let connection_string = format!(
        "postgresql://synctv:synctv_bench@127.0.0.1:{}/synctv_bench",
        postgres
            .get_host_port_ipv4(5432)
            .await
            .expect("Failed to get port")
    );

    let pool = PgPool::connect(&connection_string)
        .await
        .expect("Failed to create pool");

    sqlx::migrate!("../migrations")
        .run(&pool)
        .await
        .expect("Failed to run migrations");

    BenchmarkDatabase {
        container: postgres,
        pool,
    }
}

/// Create test user
fn make_user(username: &str) -> User {
    let now = Utc::now();
    User {
        id: UserId::new(),
        username: username.to_string(),
        email: Some(format!("{username}@test.com")),
        password_hash: "hash".to_string(),
        role: UserRole::User,
        status: UserStatus::Active,
        email_verified: true,
        signup_method: synctv_core::models::SignupMethod::Email,
        created_at: now,
        updated_at: now,
        password_changed_at: now,
        password_version: 0,
        version: 0,
        deleted_at: None,
    }
}

/// Create test room
fn make_room(name: &str, owner_id: &UserId) -> Room {
    let now = Utc::now();
    Room {
        id: RoomId::new(),
        name: name.to_string(),
        description: String::new(),
        created_by: owner_id.clone(),
        status: RoomStatus::Active,
        is_banned: false,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        version: 0,
        last_activity_at: now,
    }
}

/// Create test playlist
fn make_playlist(room_id: &RoomId, parent_id: Option<&PlaylistId>, name: &str) -> Playlist {
    let now = Utc::now();
    Playlist {
        id: PlaylistId::new(),
        room_id: room_id.clone(),
        creator_id: None,
        name: name.to_string(),
        parent_id: parent_id.cloned(),
        position: 0.0,
        source_provider: None,
        source_config: None,
        provider_instance_name: None,
        created_at: now,
        updated_at: now,
        version: 0,
    }
}

/// Benchmark: Room list queries with different data sizes
fn bench_list_rooms_with_data(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(setup_test_db());
    let pool = db.pool().clone();

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());

    // Create test user
    let owner = rt
        .block_on(user_repo.create(&make_user("bench_owner")))
        .unwrap();

    let mut group = c.benchmark_group("list_rooms");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(50);

    // Test different data sizes
    for &room_count in &[100, 500, 1000] {
        // Seed data
        for i in 0..room_count {
            let room = make_room(&format!("bench_room_{room_count}_{i}"), &owner.id);
            rt.block_on(room_repo.create(&room)).unwrap();
        }

        let bench_id = BenchmarkId::new("paginated", room_count);
        group.throughput(Throughput::Elements(20)); // page size

        group.bench_with_input(bench_id, &room_count, |b, _| {
            b.to_async(&rt).iter(|| async {
                let query = RoomListQuery {
                    pagination: PageParams::new(Some(1), Some(20)),
                    ..Default::default()
                };
                let rooms = room_repo.list(&query).await.unwrap();
                black_box(rooms);
            });
        });

        // Cleanup for next iteration
        rt.block_on(sqlx::query("DELETE FROM rooms").execute(&pool))
            .unwrap();
    }

    group.finish();
    db.shutdown(&rt);
}

/// Benchmark: Media list queries with different playlist sizes
fn bench_list_media_with_data(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(setup_test_db());
    let pool = db.pool().clone();

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool.clone());

    // Create test data
    let owner = rt
        .block_on(user_repo.create(&make_user("media_bench_owner")))
        .unwrap();
    let room = make_room("media_bench_room", &owner.id);
    let room = rt.block_on(room_repo.create(&room)).unwrap();

    let playlist = make_playlist(&room.id, None, "bench_playlist");
    let playlist = rt.block_on(playlist_repo.create(&playlist)).unwrap();

    let mut group = c.benchmark_group("list_media");
    group.measurement_time(Duration::from_secs(10));
    group.sample_size(50);

    // Test different data sizes
    for &media_count in &[100, 500, 1000] {
        // Seed media
        for i in 0..media_count {
            let media = Media {
                id: MediaId::new(),
                playlist_id: Some(playlist.id.clone()),
                room_id: room.id.clone(),
                creator_id: None,
                name: format!("media_{media_count}_{i}"),
                position: f64::from(i),
                source_provider: "direct_url".to_string(),
                source_config: serde_json::json!({"url": format!("https://example.com/{}.mp4", i)}),
                provider_instance_name: String::new(),
                added_at: Utc::now(),
                updated_at: Utc::now(),
                version: 0,
            };
            rt.block_on(media_repo.create(&media)).unwrap();
        }

        let bench_id = BenchmarkId::new("paginated", media_count);
        group.throughput(Throughput::Elements(20)); // page size

        group.bench_with_input(bench_id, &media_count, |b, _| {
            b.to_async(&rt).iter(|| async {
                let media = media_repo
                    .get_by_playlist_limit_offset(&playlist.id, 20, 0)
                    .await
                    .unwrap();
                black_box(media);
            });
        });

        // Cleanup
        rt.block_on(sqlx::query("DELETE FROM media").execute(&pool))
            .unwrap();
    }

    group.finish();
    db.shutdown(&rt);
}

/// Benchmark: Batch insert operations
fn bench_batch_insert_operations(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(setup_test_db());
    let pool = db.pool().clone();

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let playlist_repo = PlaylistRepository::new(pool.clone());
    let media_repo = MediaRepository::new(pool);

    // Create test data
    let owner = rt
        .block_on(user_repo.create(&make_user("batch_insert_owner")))
        .unwrap();
    let room = make_room("batch_insert_room", &owner.id);
    let room = rt.block_on(room_repo.create(&room)).unwrap();

    let mut group = c.benchmark_group("batch_insert");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(20);

    // Test different batch sizes
    for &batch_size in &[10, 50, 100] {
        let playlist = make_playlist(&room.id, None, &format!("batch_playlist_{batch_size}"));
        let _playlist = rt.block_on(playlist_repo.create(&playlist)).unwrap();

        let bench_id = BenchmarkId::new("media", batch_size);
        group.throughput(Throughput::Elements(
            u64::try_from(batch_size).unwrap_or_default(),
        ));

        group.bench_with_input(bench_id, &batch_size, |b, &batch_size| {
            b.iter_custom(|iters| {
                let room_id = room.id.clone();

                rt.block_on(async {
                    let start = Instant::now();
                    for _ in 0..iters {
                        // Create a new playlist for each iteration
                        let new_playlist = make_playlist(&room_id, None, &format!("batch_{}", uuid::Uuid::new_v4()));
                        let new_playlist = playlist_repo.create(&new_playlist).await.unwrap();

                        for i in 0..batch_size {
                            let media = Media {
                                id: MediaId::new(),
                                playlist_id: Some(new_playlist.id.clone()),
                                room_id: room_id.clone(),
                                creator_id: None,
                                name: format!("batch_media_{i}"),
                                position: f64::from(i),
                                source_provider: "direct_url".to_string(),
                                source_config: serde_json::json!({"url": "https://example.com/video.mp4"}),
                                provider_instance_name: String::new(),
                                added_at: Utc::now(),
                                updated_at: Utc::now(),
                                version: 0,
                            };
                            media_repo.create(&media).await.unwrap();
                        }
                    }
                    start.elapsed()
                })
            });
        });
    }

    group.finish();
    db.shutdown(&rt);
}

/// Benchmark: Index effectiveness comparison
fn bench_index_effectiveness(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(setup_test_db());
    let pool = db.pool().clone();

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool.clone());
    let member_repo = RoomMemberRepository::new(pool);

    // Create test data
    let owner = rt
        .block_on(user_repo.create(&make_user("index_owner")))
        .unwrap();
    let room = make_room("index_room", &owner.id);
    let room = rt.block_on(room_repo.create(&room)).unwrap();

    // Create members
    for i in 0..500 {
        let user = rt
            .block_on(user_repo.create(&make_user(&format!("member_{i}"))))
            .unwrap();
        let member = RoomMember::new(room.id.clone(), user.id.clone(), RoomRole::Member);
        rt.block_on(member_repo.add(&member)).unwrap();
    }

    let mut group = c.benchmark_group("index_effectiveness");
    group.sample_size(100);

    // Test indexed lookup (room_id is indexed)
    group.bench_function("get_by_room_indexed", |b| {
        b.to_async(&rt).iter(|| async {
            let members = member_repo.list_by_room(&room.id).await.unwrap();
            black_box(members);
        });
    });

    group.finish();
    db.shutdown(&rt);
}

/// Benchmark: Single row CRUD operations
fn bench_single_row_operations(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(setup_test_db());
    let pool = db.pool().clone();

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool);

    let owner = rt
        .block_on(user_repo.create(&make_user("crud_owner")))
        .unwrap();

    let mut group = c.benchmark_group("single_row_operations");
    group.sample_size(100);

    // Benchmark: Single room creation
    group.bench_function("create_room", |b| {
        b.to_async(&rt).iter(|| async {
            let room = make_room(&format!("crud_room_{}", uuid::Uuid::new_v4()), &owner.id);
            let created = room_repo.create(&room).await.unwrap();
            black_box(created);
        });
    });

    // Pre-create a room for get tests
    let room = make_room("crud_test_room", &owner.id);
    let created_room = rt.block_on(room_repo.create(&room)).unwrap();

    // Benchmark: Single room get
    group.bench_function("get_room_by_id", |b| {
        b.to_async(&rt).iter(|| async {
            let room = room_repo.get_by_id(&created_room.id).await.unwrap();
            black_box(room);
        });
    });

    group.finish();
    db.shutdown(&rt);
}

/// Benchmark: Pagination performance at different offsets
fn bench_pagination_offset_performance(c: &mut Criterion) {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let db = rt.block_on(setup_test_db());
    let pool = db.pool().clone();

    let user_repo = UserRepository::new(pool.clone());
    let room_repo = RoomRepository::new(pool);

    let owner = rt
        .block_on(user_repo.create(&make_user("pagination_owner")))
        .unwrap();

    // Create 1000 rooms
    for i in 0..1000 {
        let room = make_room(&format!("page_room_{i:04}"), &owner.id);
        rt.block_on(room_repo.create(&room)).unwrap();
    }

    let mut group = c.benchmark_group("pagination_offset");
    group.sample_size(50);

    // Test different page numbers (simulating different offsets)
    for &page in &[1u32, 5, 25, 45] {
        let bench_id = BenchmarkId::new("page", page);

        group.bench_with_input(bench_id, &page, |b, &page| {
            b.to_async(&rt).iter(|| async {
                let query = RoomListQuery {
                    pagination: PageParams::new(Some(page), Some(20)),
                    ..Default::default()
                };
                let rooms = room_repo.list(&query).await.unwrap();
                black_box(rooms);
            });
        });
    }

    group.finish();
    db.shutdown(&rt);
}

criterion_group!(
    benches,
    bench_list_rooms_with_data,
    bench_list_media_with_data,
    bench_batch_insert_operations,
    bench_index_effectiveness,
    bench_single_row_operations,
    bench_pagination_offset_performance,
);
