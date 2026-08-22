# SyncTV Web UI assets

This crate owns the acquisition, optional Flutter build, compression, manifest,
and compile-time embedding of the SyncTV browser client. `synctv-api-http`
only serves the generated asset table.

## Sources

`web-ui.toml` is the versioned production configuration. A Git source must pin
both the requested revision and its expected full lowercase commit SHA. The
build fails when the revision resolves to another commit.

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

Build and export the Web distribution:

```bash
make web-ui-build
```

Build the release server with the assets embedded:

```bash
make web-release-build
```

The Web-only command exports to `target/web-ui-dist` by default. Set
`WEB_UI_EXPORT_DIR` to select another disjoint directory.

## Build controls

| Variable | Behavior |
| --- | --- |
| `SYNCTV_WEB_CONFIG` | Select a configuration file explicitly. |
| `SYNCTV_WEB_DIST` | Use a prebuilt directory. This compatibility override takes precedence over configured sources. |
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
