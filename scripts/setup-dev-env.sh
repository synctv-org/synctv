#!/bin/bash
# Setup development environment for SyncTV

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"

echo "🚀 Setting up SyncTV development environment..."
echo ""

# Check required tools
echo "Checking required tools..."
command -v cargo >/dev/null 2>&1 || { echo "❌ cargo not found. Install Rust from https://rustup.rs/"; exit 1; }
command -v docker >/dev/null 2>&1 || { echo "❌ docker not found. Install Docker from https://docker.com/"; exit 1; }
command -v docker-compose >/dev/null 2>&1 || { echo "❌ docker-compose not found"; exit 1; }
command -v openssl >/dev/null 2>&1 || { echo "❌ openssl not found"; exit 1; }
command -v sqlx >/dev/null 2>&1 || { echo "⚠️  sqlx-cli not found. Installing..."; cargo install sqlx-cli --no-default-features --features postgres; }

echo "✓ All required tools found"
echo ""

# Generate JWT keys if they don't exist
KEYS_DIR="$PROJECT_ROOT/keys"
if [ ! -f "$KEYS_DIR/jwt_private.pem" ]; then
    echo "Generating JWT RSA keys..."
    "$SCRIPT_DIR/generate-jwt-keys.sh" "$KEYS_DIR"
    echo ""
else
    echo "✓ JWT keys already exist at $KEYS_DIR"
    echo ""
fi

# Start Docker services (PostgreSQL, Redis)
echo "Starting Docker services (PostgreSQL, Redis)..."
cd "$PROJECT_ROOT"
DEV_COMPOSE_FILE="docker-compose.dev.yml"

if [ -f "$DEV_COMPOSE_FILE" ]; then
    docker-compose -f "$DEV_COMPOSE_FILE" up -d postgres redis
    echo "✓ Docker services started"
    echo ""

    # Wait for PostgreSQL to be ready
    echo "Waiting for PostgreSQL to be ready..."
    for i in {1..30}; do
        if docker-compose -f "$DEV_COMPOSE_FILE" exec -T postgres pg_isready -U synctv >/dev/null 2>&1; then
            echo "✓ PostgreSQL is ready"
            break
        fi
        if [ $i -eq 30 ]; then
            echo "❌ PostgreSQL failed to start within 30 seconds"
            exit 1
        fi
        sleep 1
    done
    echo ""

    # Run database migrations
    echo "Running database migrations..."
    export DATABASE_URL="postgresql://synctv:synctv@localhost:5432/synctv"
    cd "$PROJECT_ROOT"
    sqlx migrate run
    echo "✓ Database migrations completed"
    echo ""
else
    echo "❌ $DEV_COMPOSE_FILE not found."
    echo "   Restore it from the repository and rerun this script."
    exit 1
fi

# Create .env file if it doesn't exist
if [ ! -f "$PROJECT_ROOT/.env" ]; then
    echo "Creating .env file..."
    cat > "$PROJECT_ROOT/.env" <<EOF
# Database
SYNCTV_DATABASE_URL=postgresql://synctv:synctv@localhost:5432/synctv

# Redis
SYNCTV_REDIS_URL=redis://localhost:6379

# JWT (fixed working development default)
SYNCTV_JWT_SECRET=dev-jwt-secret-please-change-in-production-1234567890
SYNCTV_SERVER_CLUSTER_SECRET=dev-cluster-secret-please-change-in-production-1234567890
SYNCTV_JWT_ACCESS_TOKEN_DURATION_HOURS=1
SYNCTV_JWT_REFRESH_TOKEN_DURATION_DAYS=30

# Server
SYNCTV_SERVER_HOST=0.0.0.0
SYNCTV_SERVER_PORT=8080

# Stream Server
RTMP_ADDR=0.0.0.0:1935
GRPC_ADDR=0.0.0.0:50052
ENABLE_GOP_CACHE=true
MAX_GOPS=2
MAX_GOP_CACHE_SIZE_MB=100

# Logging
SYNCTV_LOGGING_FILTER=info,synctv=debug
SYNCTV_LOGGING_BACKTRACE=true
EOF
    echo "✓ Created .env file"
    echo ""
else
    echo "✓ .env file already exists"
    echo ""
fi

echo "✅ Development environment setup complete!"
echo ""
echo "Next steps:"
echo "  1. Build the project:    cargo build"
echo "  2. Run tests:            cargo test"
echo "  3. Start server:         cargo run --bin synctv"
echo ""
echo "Services running:"
echo "  PostgreSQL: localhost:5432 (user: synctv, password: synctv, db: synctv)"
echo "  Redis:      localhost:6379"
echo ""
echo "To stop services:  docker-compose -f $DEV_COMPOSE_FILE down"
echo "To view logs:      docker-compose -f $DEV_COMPOSE_FILE logs -f"
