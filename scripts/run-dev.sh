#!/bin/bash
# Run SyncTV services in development mode

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

# Load .env if it exists
if [ -f "$PROJECT_ROOT/.env" ]; then
    export $(grep -v '^#' "$PROJECT_ROOT/.env" | xargs)
fi

cd "$PROJECT_ROOT"

echo "🚀 Starting SyncTV services in development mode..."
echo ""

# Check if Docker services are running
if ! docker-compose ps postgres 2>/dev/null | grep -q "Up"; then
    echo "⚠️  PostgreSQL is not running. Starting with docker-compose up -d..."
    docker-compose up -d postgres redis
    sleep 3
fi

# Ensure JWT secret is set
if [ -z "$SYNCTV_JWT_SECRET" ]; then
    export SYNCTV_JWT_SECRET="dev-secret-$(hostname)-$$"
    echo "⚠️  Using development JWT secret (do NOT use in production)"
fi

echo "Starting synctv server..."
echo "  HTTP: http://localhost:${SYNCTV_SERVER_HTTP_PORT:-8080}"
echo "  gRPC: localhost:${SYNCTV_SERVER_GRPC_PORT:-50051}"
echo "  RTMP: rtmp://localhost:${SYNCTV_LIVESTREAM_RTMP_PORT:-1935}"
echo ""
echo "Press Ctrl+C to stop"
echo ""

# Run the unified synctv binary
cargo run --bin synctv
