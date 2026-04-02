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
docker compose up -d
```

This variant uses `synctvorg/synctv:v1` from [docker-compose.yml](/Volumes/workspace/rust/synctv/docker-compose.yml) and intentionally keeps the environment surface small.

### 2. Manual Environment Variables

```bash
export SYNCTV_DATABASE_URL="postgresql://synctv:synctv@localhost:5432/synctv"
export SYNCTV_REDIS_URL="redis://localhost:6379"
export SYNCTV_JWT_SECRET="your-secure-RANDOM-string-WITH-mixed-CASE-123-and-SPECIAL!@#$%"
export SYNCTV_SERVER_PORT=8080
```

Optional diagnostics:
- `SYNCTV_LOGGING_FILTER` for target-specific tracing filters
- `SYNCTV_LOGGING_BACKTRACE=true` for panic backtraces

### 3. Validate Configuration (Optional but Recommended)

```bash
# Validate your configuration before deployment
cargo run --bin validate-config

# Or use the shell script
./scripts/validate-config.sh config.yaml
```

### 4. Run Database Migrations

```bash
cargo install sqlx-cli --no-default-features --features postgres
sqlx migrate run --database-url $SYNCTV_DATABASE_URL
```

### 5. Start the Server

```bash
# Set JWT secret (required for production, min 32 chars)
export SYNCTV_JWT_SECRET="your-secure-random-string-at-least-32-chars"

cargo run --bin synctv
```

HTTP/REST and gRPC share a single API port, defaulting to `0.0.0.0:8080`.

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
SYNCTV_LOGGING_LEVEL=debug cargo run --bin synctv
```

For fine-grained target filters:

```bash
SYNCTV_LOGGING_FILTER=info,synctv=debug,synctv_core::service=trace cargo run --bin synctv
```

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
2. Config file: `config.yaml` (YAML only)
3. Defaults (lowest priority)

**Comprehensive Configuration File**

A complete `config.yaml` with all options documented is provided in the repository. It includes:
- All server, database, and Redis settings
- WebRTC configuration for audio/video calls
- OAuth2 provider examples (GitHub, Google, OIDC)
- Livestream RTMP/HLS/FLV settings
- Connection limits and security options
- Production vs development guidance
- 417 lines of documented configuration

View the complete file: [`config.yaml`](config.yaml)

**Quick Example** (minimal configuration):

```yaml
server:
  host: "0.0.0.0"
  port: 8080

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
# Validate config.yaml
cargo run --bin validate-config

# Validate specific file
SYNCTV_CONFIG_PATH=/path/to/config.yaml cargo run --bin validate-config

# Use the shell script wrapper
./scripts/validate-config.sh config.yaml
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
  run: cargo run --bin validate-config
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
