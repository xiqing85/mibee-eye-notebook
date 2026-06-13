# =============================================================================
# Dockerfile — notebook-cam (MiBee Rec)
# Multi-stage build: builder (rust:1.85-slim) → runtime (debian:bookworm-slim)
# =============================================================================

# ---------- Builder Stage ----------
FROM rust:1.85-slim AS builder

WORKDIR /usr/src/notebook-cam

# Install build-time system dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    libv4l-dev \
    libasound2-dev \
    libclang-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first to leverage Docker layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY src/ ./src/
COPY migrations/ ./migrations/
COPY config.toml ./
COPY tls/ ./tls/

# Build release binary
RUN cargo build --release

# ---------- Runtime Stage ----------
FROM debian:bookworm-slim

# Install runtime system dependencies (ALSA + V4L libraries)
RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2 \
    libv4l-1 \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy binary (renamed from mibee-rec to notebook-cam for consistency)
COPY --from=builder /usr/src/notebook-cam/target/release/mibee-rec /usr/local/bin/notebook-cam

# Copy configuration and migrations
COPY --from=builder /usr/src/notebook-cam/config.toml /etc/notebook-cam/config.toml
COPY --from=builder /usr/src/notebook-cam/migrations/ /usr/local/share/notebook-cam/migrations/

# Create working directory
WORKDIR /var/lib/notebook-cam

# Expose ports: web UI (8443), RTSP (8554), RTMP (1935)
EXPOSE 8443
EXPOSE 8554
EXPOSE 1935

# Health check
HEALTHCHECK --interval=30s --timeout=3s CMD curl -f http://localhost:8443/health || exit 1

# Run as non-root
USER nobody

ENTRYPOINT ["/usr/local/bin/notebook-cam", "--config", "/etc/notebook-cam/config.toml"]
