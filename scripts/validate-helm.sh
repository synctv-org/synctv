#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

chart_dir="${SYNCTV_HELM_CHART_DIR:-helm/synctv}"
namespace="${SYNCTV_HELM_NAMESPACE:-synctv}"
tmp_dir="$(mktemp -d)"
trap 'rm -rf "$tmp_dir"' EXIT

fail() {
  echo "Helm validation failed: $*" >&2
  exit 1
}

require_rendered() {
  local pattern="$1"
  local file="$2"
  local description="$3"

  if ! grep -q "$pattern" "$file"; then
    fail "$description was not rendered in $file"
  fi
}

chart_version="$(sed -n 's/^version:[[:space:]]*//p' "$chart_dir/Chart.yaml" | head -n1 | tr -d '"')"
app_version="$(sed -n 's/^appVersion:[[:space:]]*//p' "$chart_dir/Chart.yaml" | head -n1 | tr -d '"')"
cargo_version="$(
  awk '
    /^\[workspace.package\]/ {
      in_section = 1
      next
    }
    /^\[/ {
      in_section = 0
    }
    in_section && $1 == "version" {
      gsub(/"/, "", $3)
      print $3
      exit
    }
  ' Cargo.toml
)"
compose_image_tag="$(sed -n 's/.*SYNCTV_IMAGE_TAG:-\([^}]*\).*/\1/p' docker-compose.yml | head -n1)"
docs_default_app_version="$(sed -n "s/.*defaultAppVersion = '\([^']*\)';.*/\1/p" docs/src/lib/project.ts)"

[ -n "$chart_version" ] || fail "$chart_dir/Chart.yaml must define version"
[ -n "$app_version" ] || fail "$chart_dir/Chart.yaml must define appVersion"
[ -n "$cargo_version" ] || fail "Cargo.toml must define workspace.package.version"
[ -n "$compose_image_tag" ] || fail "docker-compose.yml must define SYNCTV_IMAGE_TAG fallback"
[ -n "$docs_default_app_version" ] || fail "docs/src/lib/project.ts must define defaultAppVersion"

[ "$chart_version" = "$cargo_version" ] ||
  fail "chart version ($chart_version) must match Cargo workspace version ($cargo_version)"
[ "$app_version" = "$cargo_version" ] ||
  fail "chart appVersion ($app_version) must match Cargo workspace version ($cargo_version)"
[ "$compose_image_tag" = "$cargo_version" ] ||
  fail "Compose image fallback tag ($compose_image_tag) must match Cargo workspace version ($cargo_version)"
[ "$docs_default_app_version" = "$cargo_version" ] ||
  fail "docs default app version ($docs_default_app_version) must match Cargo workspace version ($cargo_version)"

helm lint "$chart_dir"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  >"$tmp_dir/default.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set ingress.grpc.enabled=true \
  >"$tmp_dir/grpc.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set postgresql.mode=kubeblocks \
  --set redis.mode=kubeblocks \
  >"$tmp_dir/kubeblocks.yaml"

helm template synctv "$chart_dir" \
  --namespace "$namespace" \
  --set global.imageRegistry=registry.example.com \
  --set config.security.ssrf.allowPrivateNetworkTargets=true \
  --set config.security.ssrf.allowedHosts[0]=nas.example.internal \
  --set config.security.ssrf.allowedIpRanges[0]=10.0.8.0/24 \
  >"$tmp_dir/security.yaml"

require_rendered 'image: registry.example.com/synctvorg/synctv:' \
  "$tmp_dir/security.yaml" \
  "global SyncTV image registry"
require_rendered 'image: "registry.example.com/postgres:18.1-bookworm"' \
  "$tmp_dir/security.yaml" \
  "global PostgreSQL image registry"
require_rendered 'image: "registry.example.com/redis:8.4.0-bookworm"' \
  "$tmp_dir/security.yaml" \
  "global Redis image registry"
require_rendered 'allow_private_network_targets: true' \
  "$tmp_dir/security.yaml" \
  "SSRF private-network override"
require_rendered 'nas.example.internal' \
  "$tmp_dir/security.yaml" \
  "SSRF allowed host"
require_rendered '10.0.8.0/24' \
  "$tmp_dir/security.yaml" \
  "SSRF allowed IP range"

echo "Helm chart validation passed."
