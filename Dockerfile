# =============================================================================
# Dockerfile — mibee-rec (MiBee Rec)
# Multi-stage build: builder (rust:1.85-slim) → runtime (debian:bookworm-slim)
# =============================================================================

# ---------- Builder Stage ----------
FROM rust:1.85-slim AS builder

WORKDIR /usr/src/mibee-rec

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
    ffmpeg \
    && rm -rf /var/lib/apt/lists/*

# Copy binary (renamed from mibee-rec to mibee-rec for consistency)
COPY --from=builder /usr/src/mibee-rec/target/release/mibee-rec /usr/local/bin/mibee-rec

# Copy configuration and migrations
COPY --from=builder /usr/src/mibee-rec/config.toml /etc/mibee-rec/config.toml
COPY --from=builder /usr/src/mibee-rec/migrations/ /usr/local/share/mibee-rec/migrations/

# Create working directory
WORKDIR /var/lib/mibee-rec

# Expose ports: web UI (8443), RTSP (8554), RTMP (1935)
EXPOSE 8443
EXPOSE 8554
EXPOSE 1935

# Health check
HEALTHCHECK --interval=30s --timeout=3s CMD curl -kf https://localhost:8443/health || exit 1

# Create mibee-rec user with video/audio groups for device access
RUN useradd -r -m -G video,audio mibee-rec

# Switch to mibee-rec user (non-root for security)
USER mibee-rec

ENTRYPOINT ["/usr/local/bin/mibee-rec", "--config", "/etc/mibee-rec/config.toml"]
