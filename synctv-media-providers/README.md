# SyncTV Media Providers

Media provider implementations for SyncTV, separated from core for independent deployment.

## Architecture

```
synctv/
├── synctv-core/          # Core trait definitions
│   └── src/provider/
│       ├── traits.rs     # MediaProvider trait
│       ├── registry.rs   # ProviderRegistry
│       ├── context.rs    # ProviderContext
│       ├── config.rs     # Source config types
│       └── error.rs      # ProviderError
│
├── synctv-media-providers/     # Media provider implementations (THIS CRATE)
│   └── src/
│       ├── bilibili/     # Bilibili implementation
│       ├── alist/        # Alist implementation
│       ├── emby/         # Emby implementation
│       ├── direct_url/   # DirectUrl implementation
│       └── rtmp/         # RTMP implementation
│
└── synctv-media-provider-server/  # Standalone gRPC server (optional)
    └── src/main.rs
```

## Why Separate?

This separation provides several benefits:

### 1. Independent Compilation
- Providers can be compiled separately from core
- Faster build times when only updating providers
- Reduced dependencies in core library

### 2. Independent Deployment
- Deploy providers as standalone gRPC services
- Cross-region deployment (e.g., China-specific provider instance)
- Scale providers independently

### 3. Version Independence
- Update provider implementations without rebuilding core
- Different provider versions for different regions
- A/B testing new provider implementations

### 4. Security & Isolation
- Provider credentials isolated from main application
- Network policies per provider
- Separate permission boundaries

## Usage

### As Library

```rust
use synctv_media_providers::{BilibiliProvider, DirectUrlProvider, RtmpProvider};
use synctv_core::provider::{ProviderRegistry, ProviderContext};
use serde_json::json;
use std::sync::Arc;

// Create registry
let mut registry = ProviderRegistry::new();

// Register provider factories
registry.register_factory("bilibili", Box::new(|instance_id, config| {
    Ok(Arc::new(BilibiliProvider::from_config(instance_id, config)?))
}));

registry.register_factory("rtmp", Box::new(|instance_id, config| {
    Ok(Arc::new(RtmpProvider::from_config(instance_id, config)?))
}));

// Create instances
registry.create_instance("bilibili", "bilibili_main", json!({
    "base_url": "https://synctv.example.com"
}))?;

registry.create_instance("rtmp", "rtmp_live", json!({
    "base_url": "https://synctv.example.com"
}))?;

// Use provider
let provider = registry.get_instance("bilibili_main").unwrap();
let ctx = ProviderContext::new("synctv");
let source_config = json!({
    "bvid": "BV1xx411c7XZ",
    "cid": 12345,
    "quality": 32
});
let result = provider.generate_playback(&ctx, &source_config).await?;
```

### As Standalone Service

```bash
# Build provider server
cargo build --release -p synctv-media-provider-server

# Run with configuration
./target/release/synctv-media-provider-server \
  --listen 0.0.0.0:50051 \
  --providers bilibili,alist,emby
```

## Provider Status

| Provider | HTTP Client | gRPC Server | Features |
|----------|-------------|-------------|----------|
| **Alist** | ✅ Complete | ✅ Complete | Network storage, video preview |
| **Bilibili** | ✅ Complete | ⚠️ Stub | Video/anime, quality selection |
| **Emby** | ✅ Complete | ⚠️ Stub | Media server, transcoding |
| **RTMP** | N/A | N/A | Live streaming (in synctv-stream) |
| **DirectUrl** | N/A | N/A | Simple HTTP(S) URLs |

### Implementation Details

**Alist**:
- ✅ HTTP client with login, fs_get, fs_list, fs_other
- ✅ gRPC server wrapping HTTP client
- ✅ Remote gRPC client calls in ProviderClient
- ⚠️ Me and FsSearch endpoints stubbed

**Bilibili**:
- ✅ HTTP client with BVID extraction, video info
- ⚠️ gRPC server methods stubbed (ready for implementation)

**Emby**:
- ✅ HTTP client with authentication, playback info
- ⚠️ gRPC server methods stubbed (ready for implementation)

## Deployment Patterns

### Pattern 1: Embedded (Default)

```
┌─────────────────────┐
│   SyncTV Server     │
│  ┌───────────────┐  │
│  │ synctv-core   │  │
│  │ synctv-media-providers│ │
│  └───────────────┘  │
└─────────────────────┘
```

Providers run in the same process as main application.

### Pattern 2: Remote Provider Instance

```
┌─────────────────────┐       ┌──────────────────────┐
│   SyncTV Server     │       │  Provider Instance   │
│  ┌───────────────┐  │       │  (synctv-provider-   │
│  │ synctv-core   │──┼──gRPC─┤   server)            │
│  │               │  │       │  ┌────────────────┐  │
│  └───────────────┘  │       │  │ BilibiliProvider│ │
└─────────────────────┘       │  │ AlistProvider   │  │
                              │  └────────────────┘  │
                              └──────────────────────┘
```

Providers run as separate gRPC service, useful for:
- Cross-region deployment (provider instance in China for Bilibili)
- Scaling specific providers
- Credential isolation

### Pattern 3: Hybrid

```
┌─────────────────────┐       ┌──────────────────────┐
│   SyncTV Server     │       │  Provider Instance   │
│  ┌───────────────┐  │       │  (China Region)      │
│  │ synctv-core   │  │       │  ┌────────────────┐  │
│  │ RtmpProvider  │  │       │  │ BilibiliProvider│  │
│  │ DirectUrlProv │  │       │  │ (with CDN)      │  │
│  │               │──┼──gRPC─┤  └────────────────┘  │
│  └───────────────┘  │       └──────────────────────┘
└─────────────────────┘
```

Mix of local and remote providers:
- Local: RTMP, DirectUrl (no region-specific requirements)
- Remote: Bilibili (better performance in China)

## Configuration

### Provider Instance Configuration

```json
{
  "providers": {
    "bilibili_main": {
      "type": "bilibili",
      "mode": "local",
      "config": {
        "base_url": "https://synctv.example.com"
      }
    },
    "bilibili_china": {
      "type": "bilibili",
      "mode": "remote",
      "config": {
        "grpc_url": "https://provider.cn.example.com:50051",
        "base_url": "https://synctv.cn.example.com"
      }
    },
    "rtmp_live": {
      "type": "rtmp",
      "mode": "local",
      "config": {
        "base_url": "https://synctv.example.com"
      }
    }
  }
}
```

## Benefits vs Go Version

Compared to `/Users/zjr/workspace/go/synctv/vendors`:

1. **Type Safety**: Rust's type system catches errors at compile time
2. **Zero-Cost Abstractions**: No runtime overhead from trait usage
3. **Memory Safety**: No data races or null pointer issues
4. **Better Testing**: Each provider can be tested independently
5. **Clear Boundaries**: Trait-based architecture enforces contracts

## Building

```bash
# Build only providers
cargo build -p synctv-media-providers

# Build with features
cargo build -p synctv-media-providers --features "bilibili,alist"

# Test providers
cargo test -p synctv-media-providers

# Build provider server
cargo build -p synctv-media-provider-server --release
```

## Adding New Provider

1. Create new module in `src/`:

```rust
// src/my_provider/mod.rs
use synctv_core::provider::*;

pub struct MyProvider {
    instance_id: String,
}

#[async_trait]
impl MediaProvider for MyProvider {
    fn name(&self) -> &'static str { "my_provider" }
    fn instance_id(&self) -> &str { &self.instance_id }
    // ... implement other required methods
}
```

2. Add to `lib.rs`:

```rust
pub mod my_provider;
pub use my_provider::MyProvider;
```

3. Register in application:

```rust
registry.register_factory("my_provider", Box::new(|id, cfg| {
    Ok(Arc::new(MyProvider::from_config(id, cfg)?))
}));
```

## Reference

- Design Doc: `08-video-content-management.md` (external project design doc)
- Go Implementation: `/Users/zjr/workspace/go/synctv/vendors/`
- Core Traits: `synctv-core/src/provider/`
