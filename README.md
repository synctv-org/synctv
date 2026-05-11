<p align="center">
  <img src="./docs/public/logo.svg" alt="SyncTV" width="180">
</p>

# SyncTV

[简体中文](./README.zh-CN.md)

![Rust](https://img.shields.io/badge/Rust-2021-b7410e?logo=rust&logoColor=white)
![Tokio](https://img.shields.io/badge/runtime-Tokio-2f80ed)
![Axum](https://img.shields.io/badge/HTTP-Axum-00a8a8)
![gRPC](https://img.shields.io/badge/API-gRPC-244c5a?logo=grpc&logoColor=white)
![PostgreSQL](https://img.shields.io/badge/database-PostgreSQL-4169e1?logo=postgresql&logoColor=white)
![Redis](https://img.shields.io/badge/cache%20%26%20coordination-Redis-dc382d?logo=redis&logoColor=white)
![Docker](https://img.shields.io/badge/deploy-Docker-2496ed?logo=docker&logoColor=white)
![Helm](https://img.shields.io/badge/deploy-Helm-0f1689?logo=helm&logoColor=white)
![Kubernetes](https://img.shields.io/badge/orchestration-Kubernetes-326ce5?logo=kubernetes&logoColor=white)
![OpenAPI](https://img.shields.io/badge/docs-OpenAPI-6ba539?logo=openapiinitiative&logoColor=white)
![Astro Starlight](https://img.shields.io/badge/docs-Astro%20Starlight-bc52ee?logo=astro&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-green)

SyncTV is a Rust implementation of a real-time synchronized video watching platform with media provider integration, livestreaming, HTTP/gRPC APIs, and Kubernetes-ready horizontal scaling.

## Highlights

- Synchronized room playback with real-time state updates.
- Media providers including Bilibili, Alist, Emby-compatible servers (Emby/Jellyfin), and direct URLs.
- RTMP push/pull, HLS, and HTTP-FLV livestream support.
- HTTP REST, public gRPC, WebSocket, management gRPC, metrics, RTMP, and STUN runtime surfaces.
- PostgreSQL-backed durable storage with optional Redis shared state, cache, rate limiting, and cluster coordination.
- Docker Compose and Helm deployment templates.
- Built-in management CLI and optional OpenAPI/Swagger UI.
- Astro Starlight documentation site with English and Simplified Chinese content.

## Quick Start

Development environment from a full repository checkout:

```bash
docker compose -f docker-compose.dev.yml up -d
```

Production Compose can run from the repository root or from a directory containing `docker-compose.yml`, `.env.postgres.example`, `.env.synctv.example`, and `scripts/init-compose-env.sh`. It requires explicit secrets:

```bash
./scripts/init-compose-env.sh

# Edit SYNCTV_BOOTSTRAP_ROOT_PASSWORD in .env.synctv before starting.
docker compose config
docker compose up -d
```

Validate configuration:

```bash
cargo run -p synctv --bin synctv -- config validate
```

Optional migration preflight. The server also runs embedded SQLx migrations automatically during startup:

```bash
cargo run -p synctv --bin synctv -- db migrate
```

Start locally:

```bash
cargo run -p synctv --bin synctv -- serve
```

## Documentation

The main documentation site lives in [`docs/`](./docs). It contains detailed configuration reference, deployment guides, operations runbooks, CLI reference, development guide, and OpenAPI access instructions.

```bash
cd docs
npm install
npm run dev
```

Build the static docs site:

```bash
cd docs
npm run build
```

If the generated site is deployed below a subpath, set `SYNCTV_DOCS_BASE` at build time. Set `SYNCTV_DOCS_SITE` to the public origin used for canonical URLs and sitemaps.

```bash
cd docs
SYNCTV_DOCS_SITE=https://example.com SYNCTV_DOCS_BASE=/synctv npm run build
```

Important entry points:

- [Quick Start](./docs/src/content/docs/en/install/quick-start.mdx)
- [Documentation Map](./docs/src/content/docs/en/overview/documentation-map.mdx)
- [Architecture Overview](./docs/src/content/docs/en/overview/architecture.mdx)
- [Authentication and Security Model](./docs/src/content/docs/en/admin/authentication-security.mdx)
- [Administration Runbook](./docs/src/content/docs/en/admin/index.mdx)
- [Rooms, Permissions, and Preferences](./docs/src/content/docs/en/use/rooms-permissions.mdx)
- [Client Integration Guide](./docs/src/content/docs/en/develop/client-integration.mdx)
- [How Configuration Works](./docs/src/content/docs/en/configuration/how-configuration-works.mdx)
- [Full Configuration Example](./docs/src/content/docs/en/configuration/full-example.mdx)
- [Configuration Index](./docs/src/content/docs/en/reference/configuration-index.mdx)
- [Environment Variables](./docs/src/content/docs/en/reference/environment-variables.mdx)
- [Runtime Settings Reference](./docs/src/content/docs/en/reference/runtime-settings.mdx)
- [Docker Compose Deployment](./docs/src/content/docs/en/install/docker-compose.mdx)
- [Helm Deployment](./docs/src/content/docs/en/install/helm.mdx)
- [Production Checklist](./docs/src/content/docs/en/install/production-checklist.mdx)
- [Backup and Restore](./docs/src/content/docs/en/operations/backup-restore.mdx)
- [Upgrades and Migrations](./docs/src/content/docs/en/operations/upgrades.mdx)
- [Data, Privacy, and Retention](./docs/src/content/docs/en/operations/data-retention.mdx)
- [Observability Runbook](./docs/src/content/docs/en/operations/observability.mdx)
- [Troubleshooting](./docs/src/content/docs/en/operations/troubleshooting.mdx)
- [CLI Reference](./docs/src/content/docs/en/reference/cli.mdx)
- [OpenAPI Access](./docs/src/content/docs/en/reference/openapi.mdx)
- [gRPC Debugging](./docs/src/content/docs/en/reference/grpc.mdx)
- [Development Guide](./docs/src/content/docs/en/develop/local-development.mdx)

Repository process documents:

- [Security Policy](./SECURITY.md)
- [Contributing Guide](./CONTRIBUTING.md)

## Workspace Layout

- `synctv`: application binary and CLI.
- `synctv-core`: core business logic, configuration, services, and repositories.
- `synctv-core/testing`: reusable integration-test fixtures and service helpers.
- `synctv-api`: HTTP/gRPC API layer.
- `synctv-livestream`: RTMP/HLS/HTTP-FLV livestream support.
- `synctv-cluster`: cluster coordination.
- `synctv-proxy`: media proxy and slice cache.
- `synctv-proto`: protobuf definitions.
- `synctv-media-providers`: provider integration support.
- `synctv-management`: management client/control-plane support.
- `synctv-common`: shared utilities.
- `synctv-xiu`: consolidated livestreaming components.
- `helm/synctv`: Kubernetes Helm chart.
- `docs`: Astro Starlight documentation site.

## License

MIT. See [LICENSE](./LICENSE).
