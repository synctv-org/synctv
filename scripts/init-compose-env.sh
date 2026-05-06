#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
postgres_env="$root/.env.postgres"
synctv_env="$root/.env.synctv"
postgres_example="$root/.env.postgres.example"
synctv_example="$root/.env.synctv.example"

for file in "$postgres_env" "$synctv_env"; do
  if [ -e "$file" ]; then
    echo "$(basename "$file") already exists; refusing to overwrite it." >&2
    exit 1
  fi
done

cp "$postgres_example" "$postgres_env"
cp "$synctv_example" "$synctv_env"

set_env() {
  local file="$1"
  local key="$2"
  local value="$3"
  local escaped

  escaped="$(printf '%s' "$value" | sed 's/[&/\]/\\&/g')"
  sed -i.bak "s/^${key}=.*/${key}=${escaped}/" "$file"
}

postgres_password="$(openssl rand -hex 32)"
set_env "$postgres_env" POSTGRES_PASSWORD "$postgres_password"
set_env "$synctv_env" SYNCTV_DATABASE_URL "postgresql://synctv:${postgres_password}@postgres:5432/synctv"
set_env "$synctv_env" SYNCTV_JWT_SECRET "$(openssl rand -base64 32)"
set_env "$synctv_env" SYNCTV_SERVER_CLUSTER_SECRET "$(openssl rand -hex 32)"
set_env "$synctv_env" SYNCTV_SECURITY_CREDENTIAL_ENCRYPTION_KEY "$(openssl rand -hex 32)"
set_env "$synctv_env" SYNCTV_SECURITY_OPAQUE_SERVER_SETUP_SECRET "$(openssl rand -base64 48)"
rm -f "$postgres_env.bak" "$synctv_env.bak"

cat <<'EOF'
Created .env.postgres and .env.synctv with generated production secrets.

Edit SYNCTV_BOOTSTRAP_ROOT_PASSWORD in .env.synctv before starting:

  docker compose config
  docker compose up -d

Keep both files backed up and stable across restarts, host reboots, and upgrades.
EOF
