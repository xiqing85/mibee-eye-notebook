# =============================================================================
# Dockerfile — mibee-eye (MiBee Eye)
# Multi-stage build: builder (rust:1-slim, latest stable) → runtime (debian:bookworm-slim)
# =============================================================================

# ---------- Builder Stage ----------
# Pin to rust:1.88-bookworm to get a rustc new enough for modern deps (≥1.88
# required by rcgen/time/tonic/image/jpeg-encoder) while staying on Debian
# Bookworm (glibc 2.36) for runtime stability — Trixie (the default for
# rust:1-slim / latest) has shown a userspace CPU-spin issue on the target
# kernels.
FROM rust:1.88-bookworm AS builder

WORKDIR /usr/src/mibee-eye

# Install build-time system dependencies.
# g++ (C++ compiler) is required by openh264-sys2's build.rs — it compiles the
#   vendored Cisco OpenH264 C++ source via the `cc` crate. libclang-dev alone
#   provides headers/libclang but not the `c++` binary.
# libv4l-dev + libasound2-dev are needed by nokhwa (V4L2) and cpal (ALSA).
# libssl-dev is needed by openssl-sys (pulled in transitively by reqwest/tokio).
RUN apt-get update && apt-get install -y --no-install-recommends \
    g++ \
    libv4l-dev \
    libasound2-dev \
    libclang-dev \
    libssl-dev \
    pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Copy manifests first to leverage Docker layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY src/ ./src/
COPY migrations/ ./migrations/
COPY config.toml ./
# Build release binary. The web UI is the shared mibee-webui vanilla build
# (plain static files embedded via include_dir!) — no node/npm/esbuild step.
RUN cargo build --release

# ---------- Runtime Stage ----------
FROM debian:bookworm-slim

# Install runtime system dependencies.
# No more ffmpeg — encoding is now done in-process via openh264 + muxide.
# libasound2 + libv4l-0 are needed for ALSA audio capture and V4L2 device access.
# libssl3 is needed by the OpenSSL-linked TLS stack (reqwest/tokio).
RUN apt-get update && apt-get install -y --no-install-recommends \
    libasound2 \
    libv4l-0 \
    libssl3 \
    curl \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Copy binary
COPY --from=builder /usr/src/mibee-eye/target/release/mibee-eye /usr/local/bin/mibee-eye

# Copy configuration and migrations
COPY --from=builder /usr/src/mibee-eye/config.toml /etc/mibee-eye/config.toml
COPY --from=builder /usr/src/mibee-eye/migrations/ /usr/local/share/mibee-eye/migrations/

# Create working directory
WORKDIR /var/lib/mibee-eye

# Expose ports: web UI (8443), RTSP (8554), RTMP (1935)
EXPOSE 8443
EXPOSE 8554
EXPOSE 1935

# Health check
HEALTHCHECK --interval=30s --timeout=3s CMD curl -kf https://localhost:8443/health || exit 1

# Create mibee-eye user with video/audio groups for device access
RUN useradd -r -m -G video,audio mibee-eye \
    && mkdir -p /var/lib/mibee-eye \
    && chown mibee-eye:mibee-eye /var/lib/mibee-eye

# Switch to mibee-eye user (non-root for security)
USER mibee-eye

ENTRYPOINT ["/usr/local/bin/mibee-eye", "--config", "/etc/mibee-eye/config.toml"]
