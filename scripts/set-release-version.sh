#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <semver-version>" >&2
  echo "Example: $0 0.2.0" >&2
}

if [ "$#" -ne 1 ]; then
  usage
  exit 2
fi

version="${1#v}"

if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z][0-9A-Za-z.-]*)?$ ]]; then
  echo "Invalid release version '$1'. Use SemVer without build metadata, for example 0.2.0 or 0.2.0-rc.1." >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

export SYNCTV_RELEASE_VERSION="$version"

perl -0pi -e '
  my $version = $ENV{"SYNCTV_RELEASE_VERSION"};
  s/(\[workspace\.package\]\n(?:(?!\n\[).)*?^version\s*=\s*")[^"]+(")/$1$version$2/ms
' Cargo.toml

perl -0pi -e '
  my $version = $ENV{"SYNCTV_RELEASE_VERSION"};
  s/^version:\s*.*/version: $version/m;
  s/^appVersion:\s*.*/appVersion: "$version"/m;
' helm/synctv/Chart.yaml

perl -0pi -e '
  my $version = $ENV{"SYNCTV_RELEASE_VERSION"};
  s/("name":\s*"synctv-docs",\n\s*"version":\s*")[^"]+(")/$1$version$2/g;
' docs/package.json docs/package-lock.json

perl -0pi -e '
  my $version = $ENV{"SYNCTV_RELEASE_VERSION"};
  s/(defaultAppVersion = '\'')[^'\'']+('\'';)/$1$version$2/;
' docs/src/lib/project.ts

perl -0pi -e '
  my $version = $ENV{"SYNCTV_RELEASE_VERSION"};
  s/(\$\{SYNCTV_IMAGE_TAG:-)[^}]+(\})/$1$version$2/g;
' docker-compose.yml

perl -0pi -e '
  my $version = $ENV{"SYNCTV_RELEASE_VERSION"};
  s/(--version\s+)[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?/$1$version/g;
' helm/synctv/README.md

cargo_version="$(cargo metadata --format-version 1 --no-deps | node -e 'const fs = require("fs"); const meta = JSON.parse(fs.readFileSync(0, "utf8")); process.stdout.write(meta.workspace_default_members.length ? meta.packages.find((pkg) => pkg.id === meta.workspace_default_members[0]).version : meta.packages[0].version);')"
chart_version="$(ruby -ryaml -e 'puts YAML.load_file(ARGV.fetch(0)).fetch("version")' helm/synctv/Chart.yaml)"
app_version="$(ruby -ryaml -e 'puts YAML.load_file(ARGV.fetch(0)).fetch("appVersion")' helm/synctv/Chart.yaml)"
docs_package_version="$(node -p 'require("./docs/package.json").version')"
docs_lock_version="$(node -p 'require("./docs/package-lock.json").version')"
docs_lock_root_version="$(node -p 'require("./docs/package-lock.json").packages[""].version')"
docs_default_app_version="$(node --input-type=module -e 'const project = await import("./docs/src/lib/project.ts"); process.stdout.write(project.dockerImageTag);')"
compose_image_tag="$(ruby -ryaml -e '
  compose = YAML.load_file(ARGV.fetch(0))
  image = compose.fetch("services").fetch("synctv").fetch("image")
  match = image.match(/\$\{SYNCTV_IMAGE_TAG:-([^}]+)\}/)
  abort("docker-compose.yml synctv image must use SYNCTV_IMAGE_TAG fallback") unless match
  puts match[1]
' docker-compose.yml)"

if [ "$cargo_version" != "$version" ] ||
  [ "$chart_version" != "$version" ] ||
  [ "$app_version" != "$version" ] ||
  [ "$docs_package_version" != "$version" ] ||
  [ "$docs_lock_version" != "$version" ] ||
  [ "$docs_lock_root_version" != "$version" ] ||
  [ "$docs_default_app_version" != "$version" ] ||
  [ "$compose_image_tag" != "$version" ]; then
  echo "Failed to synchronize release version across Cargo.toml, Helm chart, Compose, and docs metadata." >&2
  exit 1
fi

cargo update --workspace
cargo metadata --format-version 1 --no-deps >/dev/null

stale_lock_versions="$(
  awk '
    function flush() {
      if (name ~ /^synctv/ && version != expected) {
        print name " " version
      }
    }
    BEGIN {
      expected = ENVIRON["SYNCTV_RELEASE_VERSION"]
    }
    /^\[\[package\]\]/ {
      flush()
      name = ""
      version = ""
      next
    }
    /^name = / {
      name = $3
      gsub(/"/, "", name)
      next
    }
    /^version = / {
      version = $3
      gsub(/"/, "", version)
      next
    }
    END {
      flush()
    }
  ' Cargo.lock
)"

if [ -n "$stale_lock_versions" ]; then
  echo "Cargo.lock still contains SyncTV workspace packages with stale versions:" >&2
  echo "$stale_lock_versions" >&2
  exit 1
fi

echo "Synchronized release version $version across Cargo workspace, Cargo.lock, Helm chart, Compose, and docs metadata."
