#!/bin/bash
# Run SyncTV services in development mode

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Load .env if it exists
if [ -f "$PROJECT_ROOT/.env" ]; then
    set -a
    # shellcheck disable=SC1090
    source "$PROJECT_ROOT/.env"
    set +a
fi

cd "$PROJECT_ROOT"

echo "🚀 Starting SyncTV services in development mode..."
echo ""

DEV_COMPOSE_FILE="docker-compose.dev.yml"

# Check if Docker services are running
if ! docker compose -f "$DEV_COMPOSE_FILE" ps postgres 2>/dev/null | grep -q "Up"; then
    echo "⚠️  PostgreSQL is not running. Starting with docker compose -f $DEV_COMPOSE_FILE up -d..."
    docker compose -f "$DEV_COMPOSE_FILE" up -d postgres redis
    sleep 3
fi

# Ensure JWT secret is set
if [ -z "$SYNCTV_JWT_SECRET" ]; then
    export SYNCTV_JWT_SECRET="dev-jwt-secret-please-change-in-production-1234567890"
    echo "⚠️  Using development JWT secret (do NOT use in production)"
fi

if [ -z "$SYNCTV_SERVER_CLUSTER_SECRET" ]; then
    export SYNCTV_SERVER_CLUSTER_SECRET="dev-cluster-secret-please-change-in-production-1234567890"
fi

if [ -z "$SYNCTV_BOOTSTRAP_CREATE_ROOT_USER" ]; then
    export SYNCTV_BOOTSTRAP_CREATE_ROOT_USER="true"
fi

if [ -z "$SYNCTV_BOOTSTRAP_ROOT_PASSWORD" ]; then
    export SYNCTV_BOOTSTRAP_ROOT_PASSWORD="DevRootPass12345"
    echo "⚠️  Using development root bootstrap password for the default 'root' management user"
fi

echo "Starting synctv server..."
echo "  API:  http://localhost:${SYNCTV_SERVER_PORT:-8080} (HTTP/1 REST, HTTP/2 gRPC)"
echo "  RTMP: rtmp://localhost:${SYNCTV_LIVESTREAM_RTMP_PORT:-1935}"
echo ""
echo "Press Ctrl+C to stop"
echo ""

# Run the unified synctv binary
cargo run --bin synctv -- serve
