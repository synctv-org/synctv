# SyncTV Media Providers

HTTP clients and gRPC services for SyncTV media providers, separated from core
so provider backends can also run as a remote service.

## Architecture

```
synctv/
├── synctv-core/          # Core provider adapters and trait definitions
│   └── src/provider/
│       ├── traits.rs     # MediaProvider trait
│       ├── registry.rs   # ProviderRegistry
│       ├── context.rs    # ProviderContext
│       ├── config.rs     # Source config types
│       ├── direct_url.rs # Direct URL adapter
│       ├── rtmp.rs       # RTMP adapter
│       └── error.rs      # ProviderError
│
├── synctv-media-providers/     # HTTP clients and gRPC provider services
│   └── src/
│       ├── alist/        # Alist HTTP client
│       ├── bilibili/     # Bilibili HTTP client
│       ├── emby/         # Emby HTTP client
│       ├── grpc/         # Provider gRPC server implementations
│       └── bin/
│           └── media-provider-server.rs
│
└── synctv-api/           # API handlers that call local or remote providers
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

This crate exposes the internal remote-provider wire protocol under the
`synctv.media.*` protobuf packages in `synctv-media-providers/proto/`. That
protocol is used by `media-provider-server` and SyncTV remote provider
instances. It is separate from the public SyncTV provider API in
`synctv-proto/proto/providers`, which is exposed by the main SyncTV API server.

### As Library

```rust
use synctv_media_providers::{AlistClient, BilibiliClient, EmbyClient};

let alist = AlistClient::new("https://alist.example.com")?;
let bilibili = BilibiliClient::new()?;
let emby = EmbyClient::new("https://emby.example.com")?;
```

Core `MediaProvider` adapters live under `synctv-core/src/provider/`. Direct
URL and RTMP are implemented there, not in this crate.

### As Standalone Service

```bash
# Build provider server
cargo build --release -p synctv-media-providers --bin media-provider-server

# Run with configuration
PROVIDER_AUTH_SECRET="$(openssl rand -hex 32)" \
PROVIDER_LISTEN_ADDR="0.0.0.0:50051" \
./target/release/media-provider-server
```

## Provider Status

| Provider | HTTP Client | gRPC Server | Features |
|----------|-------------|-------------|----------|
| **Alist** | Complete | Complete | Network storage, video preview |
| **Bilibili** | Complete | Complete | Video/anime metadata, playback helpers, login, subtitles, and live-stream helpers |
| **Emby** | Complete | Complete | Media server browsing, playback, session, and playback-report helpers |

### Implementation Details

**Alist**:
- HTTP client with login, me, fs_get, fs_list, fs_other, and fs_search
- gRPC server wrapping HTTP client with request validation
- Remote gRPC client calls in ProviderClient

**Bilibili**:
- HTTP client with login, video/anime parsing, playback URLs, DASH metadata,
  subtitles, user info, URL matching, live streams, and live danmu info
- gRPC server wrapping HTTP client with request validation and provider-specific
  error mapping

**Emby**:
- HTTP client with authentication, item browsing, playback info, logout,
  session cleanup, and playback reporting
- gRPC server wrapping HTTP client with request validation and provider-specific
  error mapping

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

Core provider adapters run in the same process as the main application. This
crate supplies the HTTP clients used by the Alist, Bilibili, and Emby adapters.

### Pattern 2: Remote Provider Instance

```
┌─────────────────────┐       ┌──────────────────────┐
│   SyncTV Server     │       │  Provider Instance   │
│  ┌───────────────┐  │       │  media-provider-     │
│  │ synctv-core   │──┼──gRPC─┤  server              │
│  │               │  │       │  ┌────────────────┐  │
│  └───────────────┘  │       │  │ BilibiliProvider│ │
└─────────────────────┘       │  │ AlistProvider   │  │
                              │  └────────────────┘  │
                              └──────────────────────┘
```

Alist, Bilibili, and Emby provider services run as a separate gRPC service,
useful for:
- Cross-region deployment (provider instance in China for Bilibili)
- Scaling specific providers
- Credential isolation

### Pattern 3: Hybrid

```
┌─────────────────────┐       ┌──────────────────────┐
│   SyncTV Server     │       │  Provider Instance   │
│  ┌───────────────┐  │       │  (China Region)      │
│  │ synctv-core   │  │       │  ┌────────────────┐  │
│  │ DirectUrl/RTMP│  │       │  │ Bilibili service│  │
│  │ local adapters│  │       │  │ (with CDN)      │  │
│  │               │──┼──gRPC─┤  └────────────────┘  │
│  └───────────────┘  │       └──────────────────────┘
└─────────────────────┘
```

Mix of local and remote providers:
- Local: Direct URL and RTMP adapters in `synctv-core`
- Remote: Bilibili, Alist, or Emby through `media-provider-server`

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
    }
  }
}
```

## Design Benefits

Compared to earlier provider implementations:

1. **Type Safety**: Rust's type system catches errors at compile time
2. **Zero-Cost Abstractions**: No runtime overhead from trait usage
3. **Memory Safety**: No data races or null pointer issues
4. **Better Testing**: Each provider can be tested independently
5. **Clear Boundaries**: Trait-based architecture enforces contracts

## Building

Feature flags are TLS-related: `tls-aws-lc`, `tls-ring`,
`tls-webpki-roots`, and `tls-native-roots`. The crate does not currently define
media-provider-specific feature flags.

```bash
# Build only providers
cargo build -p synctv-media-providers

# Build with TLS provider features
cargo build -p synctv-media-providers --features "tls-aws-lc,tls-webpki-roots"

# Test providers
cargo test -p synctv-media-providers

# Build provider server
cargo build -p synctv-media-providers --bin media-provider-server --release
```

## Adding New Provider

1. Create a new HTTP client module in `synctv-media-providers/src/`:

```rust
// src/my_provider/mod.rs
pub struct MyProviderClient {
    base_url: String,
}
```

2. Add the module and public client export to `lib.rs`:

```rust
pub mod my_provider;
pub use my_provider::MyProviderClient;
```

3. Add gRPC protobuf/service glue if the provider must run remotely.

4. Add a `synctv-core/src/provider/` adapter if it needs to implement the
   core `MediaProvider` trait.

## Reference

- Core Traits: `synctv-core/src/provider/`
