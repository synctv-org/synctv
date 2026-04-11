# SyncTV - Rust Implementation

A production-grade real-time synchronized video watching platform built in Rust.

## Features

- **Real-time Synchronization**: Watch videos together with friends in perfect sync
- **Multi-Provider Support**: Bilibili, Alist, Emby, and direct URLs
- **Live Streaming**: RTMP push/pull with HLS and FLV support
- **Horizontal Scalability**: Kubernetes-ready multi-replica deployment
- **High Performance**: Built with Rust for maximum efficiency
- **Type Safety**: Compile-time guarantees and zero-cost abstractions

## Architecture

- **synctv-core**: Core business logic library
- **synctv-api**: gRPC + HTTP API service
- **synctv-livestream**: Live streaming service (RTMP/HLS/FLV)
- **synctv-cluster**: Cluster coordination library
- **synctv-xiu**: Consolidated streaming library (RTMP/HLS/HTTP-FLV protocols)

## Quick Start

### Prerequisites

- Rust 1.75+ (2021 edition)
- PostgreSQL 14+
- Redis 7+
- OpenSSL

### 1. Start with Docker Compose

Development build from the local source tree:

```bash
docker compose -f docker-compose.dev.yml up -d
```

This variant builds with the local [Dockerfile](/Volumes/workspace/rust/synctv/Dockerfile) and ships fixed working development defaults for JWT and cluster secrets.

Prebuilt image deployment:

```bash
export SYNCTV_JWT_SECRET="your-secure-random-string-at-least-32-chars"
export SYNCTV_BOOTSTRAP_ROOT_PASSWORD="StrongRootPass12345"
docker compose up -d
```

This variant uses `synctvorg/synctv:v1` from [docker-compose.yml](/Volumes/workspace/rust/synctv/docker-compose.yml) and intentionally keeps the environment surface small.

Once the daemon is up, management CLI commands can run inside the container and use the
default Unix socket without extra flags:

```bash
docker compose exec synctv synctv system stats
```

### 2. Manual Environment Variables

```bash
export SYNCTV_DATABASE_URL="postgresql://synctv:synctv@localhost:5432/synctv"
export SYNCTV_REDIS_URL="redis://localhost:6379"
export SYNCTV_JWT_SECRET="your-secure-RANDOM-string-WITH-mixed-CASE-123-and-SPECIAL!@#$%"
export SYNCTV_BOOTSTRAP_CREATE_ROOT_USER=true
export SYNCTV_BOOTSTRAP_ROOT_PASSWORD="StrongRootPass12345"
export SYNCTV_SERVER_PORT=8080
```

### 3. Validate Configuration (Optional but Recommended)

```bash
# Validate your configuration before deployment
cargo run --bin synctv -- config validate

# Validate a specific config file
cargo run --bin synctv -- config --config /path/to/synctv.yaml validate
```

### 4. Run Database Migrations

```bash
# Run embedded migrations with the same config resolution as the server
cargo run --bin synctv -- db migrate
```

### 5. Start the Server

```bash
# Set JWT secret (required for production, min 32 chars)
export SYNCTV_JWT_SECRET="your-secure-random-string-at-least-32-chars"
export SYNCTV_BOOTSTRAP_ROOT_PASSWORD="StrongRootPass12345"

cargo run --bin synctv -- serve
```

HTTP/REST and public gRPC share a single API port, defaulting to `0.0.0.0:8080`.
The management daemon default endpoint is platform-specific:
- Linux / other Unix: `unix://$XDG_STATE_HOME/synctv/run/synctv.sock` when `XDG_STATE_HOME` is set, otherwise `unix://$HOME/.local/state/synctv/run/synctv.sock`
- macOS: `unix://$HOME/.synctv/run/synctv.sock`
- Windows: `http://127.0.0.1:50052`

Runtime-owned local files can be relocated with `--data-dir`, `SYNCTV_DATA_DIR`, or
top-level config `data_dir`.

`data_dir` applies to runtime-owned local paths:
- default management Unix socket path and relative `management.unix_socket_path`
- relative `logging.file_path`
- relative `livestream.hls_storage_path`
- relative `cache.proxy_slice_file_cache_dir`

`data_dir` does not rebase static input files:
- `*_file` secret references such as `jwt.secret_file`, `management.auth_token_file`,
  `oauth2.providers.*.client_secret_file`, or provider credential `_file` fields
- `metrics.tls.cert_path` and `metrics.tls.key_path`

Absolute paths are always used as-is. Relative `data_dir` from config files is resolved
relative to the config file directory; `--data-dir` and `SYNCTV_DATA_DIR` are resolved
relative to the current working directory.

On platforms without Unix Domain Socket support, `management.transport: unix` is rejected during
configuration validation instead of silently falling back.

When a Unix socket is not available, the CLI can be pointed at an explicit endpoint with
`--endpoint` or `SYNCTV_MANAGEMENT_ENDPOINT`, and the server can be configured to listen on
TCP at `127.0.0.1:50052`. In TCP mode the management listener is always forced to loopback;
there is no configurable management host.

### 6. Remote CLI Operations

All operational CLI commands talk to a running SyncTV server. There is no offline admin mode.

```bash
# List users through the local management daemon endpoint
cargo run --bin synctv -- user list

# Inspect effective runtime settings
cargo run --bin synctv -- settings get server

# Update runtime settings through the management daemon
cargo run --bin synctv -- settings update server \
  --set signup_enabled=false \
  --set max_rooms_per_user=42

# Inspect cluster/system stats
cargo run --bin synctv -- system stats

# Inspect or manage remote provider instances
cargo run --bin synctv -- provider list

# List playlists inside a room
cargo run --bin synctv -- playlist list --room-id room-123

# Add a direct media URL into a room playlist
cargo run --bin synctv -- media add-url 'https://cdn.example.com/video.mp4' \
  --room-id room-123 \
  --playlist-id playlist-123
```

The management daemon executes local CLI requests with built-in god-mode privileges. The compose
files and development scripts create a bootstrap administrator automatically; for manual
deployments, set
`SYNCTV_BOOTSTRAP_CREATE_ROOT_USER=true` together with a strong
`SYNCTV_BOOTSTRAP_ROOT_PASSWORD` before first startup.

Remote CLI commands do not load SyncTV config files. Endpoint resolution is:
1. `--endpoint`
2. `SYNCTV_MANAGEMENT_ENDPOINT`
3. platform default Unix socket path
4. default TCP endpoint `http://127.0.0.1:50052`

In containerized deployments, `docker compose exec synctv synctv ...` uses the same default
Unix socket path inside the container. Use `--endpoint` only when intentionally targeting a
non-default TCP or Unix socket listener.

## Development

### Docker Compose

```bash
# Local development build with fixed working defaults
docker compose -f docker-compose.dev.yml up -d

# Prebuilt image deployment
docker compose up -d
```

### Run Tests

```bash
cargo test --workspace
```

### Run with Logging

```bash
SYNCTV_LOGGING_LEVEL=debug cargo run --bin synctv -- serve
```

For advanced tracing filters, prefer setting `logging.filter` in `synctv.yaml` or exporting
`SYNCTV_LOGGING_FILTER` only for one-off diagnostics.

### Build Release

```bash
cargo build --release --workspace
```

## API

### gRPC API

Use gRPC reflection to explore the API:

```bash
grpcurl -plaintext localhost:8080 list
grpcurl -plaintext localhost:8080 list synctv.client.ClientService
```

### Example: Register User

```bash
grpcurl -plaintext -d '{
  "username": "alice",
  "email": "alice@example.com",
  "password": "securepassword123"
}' localhost:8080 synctv.client.ClientService/Register
```

### Example: Login

```bash
grpcurl -plaintext -d '{
  "username": "alice",
  "password": "securepassword123"
}' localhost:8080 synctv.client.ClientService/Login
```

## Configuration

Configuration can be provided via:
1. Environment variables (highest priority): `SYNCTV_SECTION_KEY`
2. Config file (`.yaml`, `.yml`, `.json`, `.toml`), searched in platform-aware default locations such as:
   `./synctv.yaml`, Linux `$XDG_CONFIG_HOME/synctv/synctv.yaml` or `~/.config/synctv/synctv.yaml`,
   macOS `~/.synctv/synctv.yaml`, Linux `/etc/synctv/synctv.yaml`,
   `/config/synctv.yaml`
3. Defaults (lowest priority)

`data_dir` can also be set via CLI `--data-dir`, environment `SYNCTV_DATA_DIR`, or
top-level config `data_dir`.

It affects only runtime-owned local paths:
- `management.unix_socket_path`
- `logging.file_path`
- `livestream.hls_storage_path`
- `cache.proxy_slice_file_cache_dir`

It does not affect static config inputs:
- `*_file` secrets remain relative to the config file directory
- `metrics.tls.cert_path` and `metrics.tls.key_path` remain relative to the config file directory

Absolute paths are preserved. Relative runtime-owned paths are resolved against the
effective data directory.

**Comprehensive Configuration File**

A complete example config with documented options is provided in the repository. It includes:
- All server, database, and Redis settings
- WebRTC configuration for audio/video calls
- OAuth2 provider examples (GitHub, Google, OIDC)
- Livestream RTMP/HLS/FLV settings
- Connection limits and security options
- Production vs development guidance
- 417 lines of documented configuration

View the complete file: [`synctv.example.yaml`](/Volumes/workspace/rust/synctv/synctv.example.yaml)

**Quick Example** (minimal configuration):

```yaml
data_dir: "/var/lib/synctv"

server:
  host: "0.0.0.0"
  port: 8080

management:
  enabled: true
  transport: "unix"
  unix_socket_path: "/run/synctv/synctv.sock"  # Linux/container example

database:
  url: "postgresql://synctv:synctv@localhost:5432/synctv"
  max_connections: 100  # Increased for better performance

redis:
  url: "redis://localhost:6379"

jwt:
  secret: ""  # REQUIRED: Set via SYNCTV_JWT_SECRET env var

logging:
  level: "info"
  format: "pretty"  # Use "json" in production
  # filter: "info,synctv=debug"
  backtrace: false
```

📚 **See [docs/ENVIRONMENT_VARIABLES.md](docs/ENVIRONMENT_VARIABLES.md)** for environment variable reference

### Configuration Validation

Use the built-in validation tool to catch configuration errors before deployment:

```bash
# Validate synctv.yaml
cargo run --bin synctv -- config validate

# Validate specific file
cargo run --bin synctv -- config --config /path/to/synctv.yaml validate
```

**What gets validated:**
- Syntax and structure (YAML parsing)
- Required fields presence
- JWT secret strength (minimum 256-bit entropy)
- OAuth2 + Redis dependency
- WebRTC cluster mode requirements
- Permission hierarchy correctness
- Network configuration validity

**Use in CI/CD:**
```yaml
# GitHub Actions example
- name: Validate Configuration
  run: cargo run --bin synctv -- config validate
```

See [docs/config-validation.md](docs/config-validation.md) for detailed documentation.

## Security

- **Password Hashing**: Argon2id (PHC 2023 winner)
- **JWT**: HS256 symmetric HMAC
- **Permissions**: 64-bit bitmask system
- **TLS**: Recommended for production

## License

MIT OR Apache-2.0

## Contributing

Contributions are welcome! Please read CONTRIBUTING.md for guidelines.

## Status

**Current Status**: Production-ready core features

### Completed Features
- [x] User authentication (registration, login, JWT tokens)
- [x] Room management and real-time synchronization
- [x] Multi-provider media support (Bilibili, Alist, Emby)
- [x] Live streaming (RTMP push, HLS/FLV playback)
- [x] Multi-replica cluster support
- [x] OAuth2 integration (GitHub, Google, OIDC)
- [x] Permission system with 64-bit bitmask
- [x] WebSocket real-time communication

### Completed Infrastructure
- [x] Cross-replica cache invalidation via Redis Streams (durable delivery with catch-up on reconnection)
- [x] Configuration validation tool with CI/CD integration

**Next Milestone**: Production hardening and performance optimization
