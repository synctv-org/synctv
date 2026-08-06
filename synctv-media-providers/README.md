# SyncTV Media Providers

`synctv-media-providers` contains the upstream protocol implementations used by
SyncTV. Each provider owns its HTTP/WebSocket protocol, request signing, DTOs,
pagination, authentication details, playback extraction, and protocol tests.

The complete integration guide is available in Chinese and English:

- [Provider 开发指南](../docs/src/content/docs/develop/provider-development.mdx)
- [Provider Development Guide](../docs/src/content/docs/en/develop/provider-development.mdx)
- [Provider 使用手册](../docs/src/content/docs/use/provider-guide.mdx)
- [Provider User Guide](../docs/src/content/docs/en/use/provider-guide.mdx)

## Modules

| Category | Modules | Upstream capabilities represented here |
| --- | --- | --- |
| Video and live platforms | `bilibili`, `youtube`, `twitch`, `douyin`, `tiktok`, `huya`, `douyu`, `acfun`, `cctv` | URL/ID parsing, metadata, native playback variants, platform feeds, subtitles, danmaku/chat, chapters, storyboard, and live protocols where available |
| Media servers and file services | `emby`, `alist`, `cloudreve` | Authentication, browsing, search, metadata, thumbnails, subtitles, direct streams, transcode/remux, and playback reporting where available |
| NAS and private cloud | `fnos`, `qnap`, `synology`, `nextcloud`, `seafile`, `truenas` | File browsing, search, preview/thumbnail, media libraries, native transcode, favorites, history, and playback reporting according to each server API |

Provider modules remain independent. A provider-specific source enum, cursor,
signature, media-library model, or playback response stays inside that provider.
Shared code is reserved for stable transport concerns such as HTTP safety,
bounded response reads, retries, credentials, circuit breakers, and errors.

## Architecture

Provider support crosses three API boundaries:

| Boundary | Location | Purpose |
| --- | --- | --- |
| Upstream client | `synctv-media-providers/src/<provider>/` | Calls the provider's native HTTP, WebSocket, XML, or binary protocol |
| Internal remote-provider transport | `synctv-media-providers/proto/`, `src/grpc/`, `src/remote_transport/` | Runs selected provider clients in a separate SyncTV provider process |
| Public SyncTV API | `synctv-proto/proto/providers/`, `synctv-proto/proto/playback_provider/`, `synctv-api/` | Exposes typed parse, preview, list, binding, and playback operations to App and CLI clients |

The Core adapters live in `synctv-core/src/provider/`. They connect upstream
clients to credentials, persistent source configs and targets, dynamic playlists,
autoplay, `PlaybackResult`, and proxy resource resolution.

Current standalone remote gRPC services cover Alist, Bilibili, and Emby. Other
providers run through their local clients and public SyncTV APIs. Extending the
remote transport requires a provider-specific internal proto, server, client,
registration, configuration, and local/remote parity tests.

## Library Usage

Clients create or accept SyncTV's guarded HTTP configuration so redirects and
resolved addresses remain inside the configured SSRF policy:

```rust
use synctv_media_providers::{
    build_provider_http_client, BilibiliClient, CloudreveClient,
};
use synctv_common::ssrf::SsrfGuard;

let http = build_provider_http_client(SsrfGuard::strict_policy())?;
let bilibili = BilibiliClient::new()?;
let cloudreve = CloudreveClient::with_http_client(
    "https://cloud.example.com",
    http,
)?;
```

Consult each module's `client.rs` and tests for its constructors and typed
requests. Core and API code should consume typed provider methods rather than
constructing upstream URLs or decoding provider JSON themselves.

## Standalone Provider Service

The provider server currently publishes Alist, Bilibili, and Emby internal gRPC
services. Every request carries `x-provider-secret`; each service also applies
message limits, compression policy, and an independent circuit breaker.

```bash
cargo +nightly build --release \
  -p synctv-media-providers \
  --bin media-provider-server

PROVIDER_AUTH_SECRET="$(openssl rand -hex 32)" \
PROVIDER_LISTEN_ADDR="0.0.0.0:50051" \
./target/release/media-provider-server
```

TLS features are `tls-aws-lc`, `tls-ring`, `tls-webpki-roots`, and
`tls-native-roots`. Select one crypto provider and the root source required by
the deployment.

## Adding or Extending a Provider

1. Define the provider's real product flows: login, parse, preview, browse,
   dynamic sources, playback variants, auxiliary media, and status reporting.
2. Implement a dedicated module under `src/<provider>/` with its own DTOs,
   pagination, signing, and protocol tests.
3. Add typed source configs and targets, then implement the Core provider and
   dynamic playlist behavior.
4. Add public protobuf messages, HTTP/gRPC handlers, OpenAPI schemas, CLI paths,
   and every generated playback-resource resolver.
5. Add internal remote transport when that provider needs separate deployment.
6. Sync public protobuf into the Flutter App and implement codec, binding,
   parse/preview, selection, and creation UI.
7. Test upstream contracts, Core behavior, public transports, CLI, database
   integration, and Flutter flows.

Typed parse, resolve, list, search, and preview results carry a media or playlist
source config. Page, cursor, offset, and continuation models follow each
provider's upstream contract and map to the explicit public pagination type.

## Development

Use the repository nightly toolchain for builds and checks:

```bash
cargo +nightly fmt --all
cargo +nightly check -p synctv-media-providers --all-targets
cargo +nightly nextest run -p synctv-media-providers --run-ignored all --nff
```

Run the complete workspace suite from the repository root after changing Core,
protobuf, API, management, CLI, or persistence behavior:

```bash
cargo +nightly check --workspace --all-targets
make nextest
```

Tests that call upstream services use deterministic protocol fixtures or
WireMock. Database and service integration scenarios use the workspace
Testcontainers setup and run through `make nextest`.
