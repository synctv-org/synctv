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

cargo_version="$(awk '/^\[workspace.package\]/{in_section=1; next} /^\[/{in_section=0} in_section && $1 == "version" {gsub(/"/, "", $3); print $3; exit}' Cargo.toml)"
chart_version="$(sed -n 's/^version:[[:space:]]*//p' helm/synctv/Chart.yaml | head -n1 | tr -d '"')"
app_version="$(sed -n 's/^appVersion:[[:space:]]*//p' helm/synctv/Chart.yaml | head -n1 | tr -d '"')"

if [ "$cargo_version" != "$version" ] || [ "$chart_version" != "$version" ] || [ "$app_version" != "$version" ]; then
  echo "Failed to synchronize release version across Cargo.toml and helm/synctv/Chart.yaml." >&2
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

echo "Synchronized release version $version across Cargo workspace, Cargo.lock, and Helm chart."
