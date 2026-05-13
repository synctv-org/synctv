#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

SYNCTV_REGEN_PROTO=1 cargo check \
  -p synctv-proto \
  -p synctv-media-providers \
  -p synctv-cluster \
  --locked
