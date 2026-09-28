# SyncTV Web UI assets

This crate owns acquisition, optional Flutter builds, compression, manifests,
and compile-time embedding of the SyncTV browser client. `synctv-api-http` can
also serve a distribution directly from disk during development.

## Sources

`web-ui.toml` reads prebuilt files from `dist/`. The directory is empty in Git
apart from `.gitkeep`.

`web-ui.production.toml` is the versioned production source used by CI. Its Git
source pins both the requested revision and its expected full lowercase commit
SHA. The build fails when the revision resolves to another commit.

For local development, create the ignored `web-ui.local.toml` beside the
default file. It takes precedence unless `SYNCTV_WEB_CONFIG` names another
configuration. Relative paths resolve from the selected configuration file.

Prebuilt distribution:

```toml
schema-version = 1

[source]
kind = "dist"
path = "../synctv-app/build/web"
```

Local Flutter project:

```toml
schema-version = 1

[source]
kind = "local-project"
path = "../../flutter/synctv-app"
allow-dirty = true
```

Immutable Git checkout:

```toml
schema-version = 1

[source]
kind = "git"
repository = "https://github.com/synctv-org/synctv-app.git"
revision = "refs/tags/v1.2.3"
commit = "0123456789abcdef0123456789abcdef01234567"
```

The optional `[build]` table accepts `flutter`, `arguments`, and a
`dart-defines` mapping. Arguments and defines participate in the build
fingerprint.

## Commands

Run the server against a mutable local distribution:

```bash
make dev-serve
```

`dev-serve` enables the `web-ui-dynamic` feature and sets
`SYNCTV_SERVER_WEB_UI_DIRECTORY` to `synctv-web-ui/dist`. Override the directory
with `DEV_WEB_UI_DIR=/path/to/dist`. Files are read for every request, use
`Cache-Control: no-store`, and can be replaced without rebuilding or restarting
the Rust server. A relative runtime directory is resolved from the server's
working directory.

Build and export the Web distribution:

```bash
SYNCTV_WEB_CONFIG=synctv-web-ui/web-ui.production.toml \
  make web-ui-build WEB_UI_EXPORT_DIR=synctv-web-ui/dist
```

Build the release server with the assets embedded:

```bash
make web-release-build
```

The existing `web-ui` feature embeds the distribution for release deployment.
Both features expose the same routes. When `server.web_ui_directory` is
configured, its disk contents are authoritative and take precedence over
embedded assets.

The Web-only command exports to `target/web-ui-dist` by default. CI uploads the
exported distribution once, then passes its authenticated artifact URL and
SHA-256 digest to the existing multi-platform Docker build. Docker verifies and
embeds the archive; it never installs Flutter or builds the frontend.

## Build controls

| Variable | Behavior |
| --- | --- |
| `SYNCTV_WEB_CONFIG` | Select a configuration file explicitly. |
| `SYNCTV_WEB_CACHE_DIR` | Select the Git, Flutter output, and compression cache root. |
| `SYNCTV_WEB_EXPORT_DIR` | Copy the final uncompressed distribution to a disjoint directory. |
| `SYNCTV_WEB_OFFLINE` | Disable Git fetches and use `flutter pub get --offline`. Missing cache entries fail. |
| `SYNCTV_WEB_FORCE_REBUILD` | Fetch the pinned revision again and rebuild Flutter output. |

Relative paths in these controls resolve from the workspace root containing the
`synctv-web-ui` crate. Paths inside a selected configuration resolve from that
configuration file.

The fingerprint includes the source file hash or pinned commit, Flutter version,
build arguments, dart-defines, builder version, and final distribution hash.
Git checkout, Flutter output, and compression data use separate cache layers.
Ordinary workspace builds do not enable the `embed` feature and require no
Flutter installation, Git access, or network access.
