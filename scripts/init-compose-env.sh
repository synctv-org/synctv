#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
postgres_env="$root/.env.postgres"
redis_env="$root/.env.redis"
synctv_env="$root/.env.synctv"
postgres_example="$root/.env.postgres.example"
redis_example="$root/.env.redis.example"
synctv_example="$root/.env.synctv.example"

for file in "$postgres_env" "$redis_env" "$synctv_env"; do
  if [ -e "$file" ]; then
    echo "$(basename "$file") already exists; refusing to overwrite it." >&2
    exit 1
  fi
done

if ! command -v openssl >/dev/null 2>&1; then
  cat >&2 <<'EOF'
openssl is required to generate production secrets.

Install OpenSSL and rerun this script, for example:
  Debian/Ubuntu: sudo apt-get install openssl
  Fedora/RHEL:   sudo dnf install openssl
  macOS:         brew install openssl
EOF
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  cat >&2 <<'EOF'
python3 is required to URL-encode the generated PostgreSQL password.

Install Python 3 and rerun this script, for example:
  Debian/Ubuntu: sudo apt-get install python3
  Fedora/RHEL:   sudo dnf install python3
  macOS:         brew install python
EOF
  exit 1
fi

cp "$postgres_example" "$postgres_env"
cp "$redis_example" "$redis_env"
cp "$synctv_example" "$synctv_env"

set_env() {
  local file="$1"
  local key="$2"
  local value="$3"
  local escaped

  escaped="$(printf '%s' "$value" | sed 's/[&/\]/\\&/g')"
  sed -i.bak "s/^${key}=.*/${key}=${escaped}/" "$file"
}

url_encode() {
  local value="$1"
  VALUE="$value" python3 - <<'PY'
import os
import urllib.parse

print(urllib.parse.quote(os.environ["VALUE"], safe=""))
PY
}

postgres_password="$(openssl rand -hex 32)"
postgres_password_encoded="$(url_encode "$postgres_password")"
redis_password="$(openssl rand -hex 32)"
redis_password_encoded="$(url_encode "$redis_password")"
set_env "$postgres_env" POSTGRES_PASSWORD "$postgres_password"
set_env "$redis_env" REDIS_PASSWORD "$redis_password"
set_env "$synctv_env" SYNCTV_DATABASE_URL "postgresql://synctv:${postgres_password_encoded}@postgres:5432/synctv"
set_env "$synctv_env" SYNCTV_REDIS_URL "redis://:${redis_password_encoded}@redis:6379"
set_env "$synctv_env" SYNCTV_JWT_SECRET "$(openssl rand -base64 32)"
set_env "$synctv_env" SYNCTV_CLUSTER_SECRET "$(openssl rand -hex 32)"
set_env "$synctv_env" SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY "$(openssl rand -hex 32)"
set_env "$synctv_env" SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET "$(openssl rand -base64 48)"
rm -f "$postgres_env.bak" "$redis_env.bak" "$synctv_env.bak"

cat <<'EOF'
Created .env.postgres, .env.redis, and .env.synctv with generated production secrets.

Edit SYNCTV_BOOTSTRAP_ROOT_PASSWORD in .env.synctv before starting production Compose:

  docker compose config
  docker compose up -d

For local development, use docker-compose.dev.yml directly; it has built-in
local-only settings and does not read these production env files.

Keep all three files backed up and stable across restarts, host reboots, and upgrades.
EOF
