# Contributing to SyncTV

## Development Setup

SyncTV is a Rust workspace with PostgreSQL, optional Redis, protobuf APIs, deployment templates, and an Astro Starlight documentation site.

Typical local setup:

```bash
docker compose -f docker-compose.dev.yml up -d
cargo check --workspace --all-targets
cargo test --workspace
```

Run the service locally:

```bash
cargo run -p synctv --bin synctv -- serve
```

Run with OpenAPI enabled:

```bash
cargo run -p synctv --features openapi --bin synctv -- serve
```

## Before Submitting Changes

Use the narrowest relevant test first, then run broader checks before handing off:

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --workspace --all-targets --features openapi
```

If your change touches SQLx queries, migrations, or query result shapes, refresh and verify SQLx metadata according to the project workflow before submitting.

## Documentation

The documentation site lives in `docs/`.

```bash
cd docs
npm install
npm run validate
```

Documentation changes should keep Chinese and English pages aligned when a page exists in both locales.

Update docs when changing:

- Static configuration fields: update the configuration index, full configuration example, environment variable reference, and the relevant topic page.
- Runtime settings: update the runtime settings reference.
- CLI commands: update the CLI reference.
- HTTP/OpenAPI paths: update OpenAPI and client integration docs if behavior changes.
- gRPC/protobuf APIs: update gRPC and client integration docs if behavior changes.
- Authentication, MFA, OAuth2, token, permission, or management behavior: update the security model and relevant runbooks.
- Docker Compose, Helm, Service, Ingress, metrics, or storage templates: update deployment docs and the production checklist.

## API and Compatibility

This project is still under active design. Do not preserve obsolete paths, protobuf fields, or configuration shapes unless maintainers explicitly require compatibility. Prefer clear current architecture over compatibility shims for old experimental designs.

## Security-Sensitive Changes

Treat these areas as high-risk:

- Authentication, token issuance, refresh, logout, MFA, OAuth2, passkeys, and account recovery.
- Provider credentials, encryption keys, media proxying, Range handling, redirects, and request headers.
- Management gRPC, role hierarchy, admin operations, runtime settings, and audit logs.
- Rate limits, brute-force protection, Redis state, and cache invalidation.

Add focused tests for bypass, privilege, stale-token, replay, and concurrency cases when touching these areas.

## Pull Request Hygiene

- Keep changes scoped and explain behavior changes directly.
- Do not commit generated temporary files, local build output, credentials, or personal environment files.
- Include migration and rollback notes for database or deployment changes.
- Include docs updates in the same change when user-visible behavior changes.
