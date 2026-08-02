# Contributing to SyncTV

## Development Setup

SyncTV is a Rust workspace with PostgreSQL, optional Redis, protobuf APIs, deployment templates, and an Astro Starlight documentation site.

Rust builds require `protoc` on `PATH`. Native media builds also require NASM and libclang.

Typical local setup:

```bash
docker compose -f docker-compose.dev.yml up -d postgres redis
cargo +nightly check --workspace --all-targets
make nextest
```

Run the service locally:

```bash
cargo +nightly run -p synctv --bin synctv -- serve
```

Run with OpenAPI enabled:

```bash
cargo +nightly run -p synctv --features openapi --bin synctv -- serve
```

## Before Submitting Changes

Use the narrowest relevant test first, then run broader checks before handing off:

```bash
cargo +nightly fmt --all
cargo +nightly clippy --workspace --all-targets
make nextest
cargo +nightly test --workspace --doc --locked
cargo +nightly check --workspace --all-targets --features openapi
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

Provider work starts with the [Provider Development Guide](docs/src/content/docs/en/develop/provider-development.mdx). Keep the provider's upstream client and DTOs, Core source config and target, public protobuf/API, OpenAPI, management and CLI surfaces, and Flutter App flow aligned. Validate typed parse/preview results, generated playback resources, local and remote execution paths, provider credentials, and page/cursor behavior.

Provider changes that affect the Flutter App also require its standard checks:

```bash
dart analyze
flutter test
```

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

## Database and Domain Validation

Keep database schema constraints focused on stable storage integrity, not product policy.

Database migrations may enforce long-lived persistence invariants such as primary keys, foreign keys, required storage columns, identity uniqueness, unique indexes, lookup indexes, and data-shape rules that are unlikely to change without a deliberate storage migration. They should not encode business-level validation that changes with product behavior.

Do not put these in SQL `CHECK` constraints, database enum types, or string enum lists:

- Role, status, signup-method, review-state, provider-type, or other business enum value lists.
- Workflow/state-machine coupling such as "pending means reviewed_at is null" or "rejected requires a reason".
- Permission assignability rules, runtime settings allowlists, feature policy, rate/limit policy, playback policy, room policy, or moderation policy.
- Numeric ranges that are product choices rather than physical storage constraints.

Store enum-like fields such as status, role, signup method, message type, and review state as numeric codes (`SMALLINT` is usually enough), and keep the meaning/mapping in Rust domain types. Avoid persisting those values as strings.

Implement volatile business rules in the domain/service layer, keep repository inputs typed where practical, and cover them with focused tests. If direct database writes could create invalid business state, prefer service-only write paths, operational consistency checks, or repair tooling over moving volatile policy into the schema.

`UNIQUE` is allowed when the rule is a stable identity or near-permanent uniqueness invariant and database atomicity is valuable: usernames, emails, provider instance names, OAuth account identities, idempotency keys, and one-active-row-per-stable-scope patterns are typical examples. Do not use `UNIQUE` for product policy that may be relaxed, re-scoped, or reinterpreted during normal feature work. A new project is not an exception: migrations should still avoid business constraints that would make ordinary product changes require database constraint churn.

User-facing labels and display names, such as room names, should be treated as product policy unless they are explicitly defined as stable identifiers. Enforce those rules in services, using transactional locks or other coordination where concurrency matters.

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
