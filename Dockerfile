# Stage 1: Build
FROM rust:slim AS builder

# Install build dependencies
# build-essential, perl, cmake needed for vendored builds (xiu/opus dependencies)
# curl needed for utoipa-swagger-ui to download assets
RUN apt-get update && apt-get install -y \
    protobuf-compiler \
    pkg-config \
    build-essential \
    cmake \
    curl \
    perl \
    perl-modules-5.40 && rm -rf /var/lib/apt/lists/*

# Set working directory
WORKDIR /app

# Copy entire source tree
COPY . .

# Build with cache mounts for cargo registry, git deps, and target directory
# Copy binary out of cache mount before RUN completes
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/app/target \
    cargo build --release --bin synctv && \
    cp /app/target/release/synctv /tmp/synctv

# Stage 2: Runtime image
FROM debian:bookworm-slim

# OCI image labels
LABEL org.opencontainers.image.title="SyncTV" \
      org.opencontainers.image.description="Distributed video synchronization platform with real-time streaming" \
      org.opencontainers.image.url="https://github.com/synctv-org/synctv" \
      org.opencontainers.image.source="https://github.com/synctv-org/synctv" \
      org.opencontainers.image.licenses="MIT"

# Install runtime dependencies (curl needed for healthcheck)
RUN apt-get update && apt-get install -y \
    ca-certificates \
    curl && rm -rf /var/lib/apt/lists/*

# Create synctv for running the application
RUN useradd -m -u 1000 synctv

# Create necessary directories
RUN mkdir -p /app /app/keys /app/config

RUN chown -R synctv:synctv /app

# Set working directory
WORKDIR /app

# Copy binary from builder
COPY --from=builder \
    --chown=synctv:synctv \
    /tmp/synctv /app/synctv

# Switch to non-root user
USER synctv

# Expose ports
# 8080: HTTP API (also serves HLS via /api/room/movie/live/hls/*)
# 50051: gRPC API
# 1935: RTMP (livestream)
# 3478/udp: STUN (WebRTC)
EXPOSE 8080 50051 1935 3478/udp

# Set environment variables
ENV RUST_LOG=info
ENV RUST_BACKTRACE=1

# Health check against the HTTP health endpoint
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["curl", "-f", "http://localhost:8080/health"]

# Run the application
CMD ["/app/synctv"]
