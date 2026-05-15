#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

cp "$root/docker-compose.yml" \
  "$root/.env.postgres.example" \
  "$root/.env.redis.example" \
  "$root/.env.synctv.example" \
  "$tmpdir"/
mkdir -p "$tmpdir/scripts"
cp "$root/scripts/init-compose-env.sh" "$tmpdir/scripts/"

(
  cd "$tmpdir"
  ./scripts/init-compose-env.sh >/dev/null
  docker compose config >/dev/null

  redis_password="$(sed -n 's/^REDIS_PASSWORD=//p' .env.redis)"
  synctv_redis_url="$(sed -n 's/^SYNCTV_REDIS_URL=//p' .env.synctv)"

  if [ -z "$redis_password" ]; then
    echo ".env.redis must contain REDIS_PASSWORD" >&2
    exit 1
  fi

  if grep -q '^SYNCTV_' .env.redis; then
    echo ".env.redis must not contain SYNCTV_* variables" >&2
    exit 1
  fi

  if grep -q '^REDIS_PASSWORD=' .env.synctv; then
    echo ".env.synctv must not contain Redis container variables" >&2
    exit 1
  fi

  if ! printf '%s\n' "$synctv_redis_url" | grep -Eq '^redis://:.+@redis:6379$'; then
    echo ".env.synctv must contain an authenticated SYNCTV_REDIS_URL" >&2
    exit 1
  fi

  if ! printf '%s\n' "$synctv_redis_url" | grep -Fq "$redis_password"; then
    echo "SYNCTV_REDIS_URL must use the same Redis password as .env.redis" >&2
    exit 1
  fi
)
