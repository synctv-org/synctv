# Stage 1: Build
FROM rust:slim-trixie AS builder

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

# Compile SQLx query macros from checked-in .sqlx metadata instead of requiring
# a build-time database connection.
ENV SQLX_OFFLINE=true

# Container images include Kubernetes integration, mimalloc, and OpenAPI in
# addition to the crate defaults. Override SYNCTV_BUILD_FEATURES to customize
# this set.
# Set SYNCTV_BUILD_NO_DEFAULT_FEATURES=true plus SYNCTV_BUILD_FEATURES for a
# fully explicit feature set.
ARG SYNCTV_BUILD_NO_DEFAULT_FEATURES=false
ARG SYNCTV_BUILD_FEATURES="k8s,mimalloc,openapi"
ARG SYNCTV_CARGO_BUILD_ARGS=""
ARG SYNCTV_CARGO_BUILD_PROFILE=release
ARG TARGETARCH

# Copy entire source tree
COPY . .

# Build with cache mounts for cargo registry, git deps, and target directory
# Copy binary out of cache mount before RUN completes
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,id=synctv-target-${TARGETARCH},target=/app/target,sharing=locked \
    case "$SYNCTV_CARGO_BUILD_PROFILE" in \
        dev|release) ;; \
        *) echo "Unsupported SYNCTV_CARGO_BUILD_PROFILE: $SYNCTV_CARGO_BUILD_PROFILE" >&2; exit 1 ;; \
    esac; \
    build_flags="--profile $SYNCTV_CARGO_BUILD_PROFILE --bin synctv"; \
    if [ -n "$SYNCTV_CARGO_BUILD_ARGS" ]; then \
        build_flags="$build_flags $SYNCTV_CARGO_BUILD_ARGS"; \
    fi; \
    if [ "$SYNCTV_BUILD_NO_DEFAULT_FEATURES" = "true" ]; then \
        build_flags="$build_flags --no-default-features"; \
    fi; \
    if [ -n "$SYNCTV_BUILD_FEATURES" ]; then \
        build_flags="$build_flags --features $SYNCTV_BUILD_FEATURES"; \
    fi; \
    cargo +nightly build $build_flags && \
    cp "/app/target/$SYNCTV_CARGO_BUILD_PROFILE/synctv" /tmp/synctv

# Stage 2: Runtime image
FROM debian:trixie-slim

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
RUN mkdir -p /app /app/keys /app/config /data /run/synctv

RUN chown -R synctv:synctv /app /data /run/synctv

# Set working directory
WORKDIR /app

# Copy binary from builder
COPY --from=builder \
    --chown=synctv:synctv \
    /tmp/synctv /app/synctv

# Execute the binary in the final image so ABI/runtime dependency mismatches
# fail the image build for every target platform.
RUN /app/synctv --version

# Switch to non-root user
USER synctv

# Expose ports
# 8080: HTTP API + public gRPC (also serves HLS via /api/room/movie/live/hls/*)
# 1935: RTMP (livestream)
# 3478/udp: STUN (WebRTC)
EXPOSE 8080 1935 3478/udp

# Health check against the HTTP health endpoint
HEALTHCHECK --interval=30s --timeout=5s --start-period=30s --retries=3 \
    CMD ["curl", "-f", "http://localhost:8080/health/ready"]

# Run the application
ENTRYPOINT ["/app/synctv"]

CMD [ "serve" ]
