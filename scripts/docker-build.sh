#!/usr/bin/env bash
# scripts/docker-build.sh
#
# Cross-compile mibee-eye for Linux x86_64 from Windows using the project's
# multi-stage Dockerfile, then extract the release binary + config + migrations
# into ./target/linux-x86_64/ for deployment.
#
# Usage:
#   ./scripts/docker-build.sh [--no-cache]
#
# Output:
#   target/linux-x86_64/mibee-eye          # release binary (glibc 2.36, x86_64)
#   target/linux-x86_64/config.toml        # default config
#   target/linux-x86_64/migrations/        # SQL migrations
#
# The binary is forward-compatible with Pop!_OS 24.04 (glibc 2.39) and
# EndeavourOS / Arch rolling (glibc >= 2.36).

set -euo pipefail

cd "$(dirname "$0")/.."

EXTRACT_CONTAINER="mibee-eye-extract-$$"
IMAGE_TAG="mibee-eye:build"
OUT_DIR="target/linux-x86_64"

# ── 1. Ensure Docker Desktop is running (Windows host) ────────────────────────
if ! docker info >/dev/null 2>&1; then
    echo "→ Docker daemon not running. Attempting to start Docker Desktop..."
    if command -v powershell.exe >/dev/null 2>&1; then
        powershell.exe -Command "Start-Process 'C:\Program Files\Docker\Docker\Docker Desktop.exe'" \
            >/dev/null 2>&1 || true
    elif command -v docker-desktop >/dev/null 2>&1; then
        (docker-desktop &) >/dev/null 2>&1 || true
    else
        echo "ERROR: Docker is not running and could not be auto-started." >&2
        echo "       Please launch Docker Desktop manually and re-run this script." >&2
        exit 1
    fi

    echo "  Waiting for Docker daemon..."
    for i in $(seq 1 60); do
        if docker info >/dev/null 2>&1; then
            echo "  Docker is up (after ${i}0s)."
            break
        fi
        sleep 10
        if [ "$i" -eq 60 ]; then
            echo "ERROR: Docker daemon did not come up within 10 minutes." >&2
            exit 1
        fi
    done
fi

# ── 2. Build the image ────────────────────────────────────────────────────────
BUILD_ARGS=()
if [[ "${1:-}" == "--no-cache" ]]; then
    BUILD_ARGS+=("--no-cache")
    echo "→ Building with --no-cache (full rebuild)..."
else
    echo "→ Building (incremental, use --no-cache for a full rebuild)..."
fi

docker build -t "$IMAGE_TAG" "${BUILD_ARGS[@]}" . || {
    echo "ERROR: docker build failed. See output above." >&2
    exit 1
}

# ── 3. Extract artifacts into ./target/linux-x86_64/ ─────────────────────────
echo "→ Extracting release binary + config + migrations to $OUT_DIR/..."
mkdir -p "$OUT_DIR"

# Clean up any stale container first (in case a prior run was interrupted).
docker rm -f "$EXTRACT_CONTAINER" >/dev/null 2>&1 || true

docker create --name "$EXTRACT_CONTAINER" "$IMAGE_TAG" >/dev/null

docker cp "$EXTRACT_CONTAINER:/usr/local/bin/mibee-eye" "$OUT_DIR/mibee-eye"
docker cp "$EXTRACT_CONTAINER:/usr/local/share/mibee-eye/migrations" "$OUT_DIR/migrations"
docker cp "$EXTRACT_CONTAINER:/etc/mibee-eye/config.toml" "$OUT_DIR/config.toml"

docker rm "$EXTRACT_CONTAINER" >/dev/null

# ── 4. Verify ────────────────────────────────────────────────────────────────
if [ ! -s "$OUT_DIR/mibee-eye" ]; then
    echo "ERROR: extracted binary is empty or missing." >&2
    exit 1
fi

SIZE=$(du -h "$OUT_DIR/mibee-eye" | cut -f1)
echo ""
echo "✓ Build complete."
echo "  Binary:  $OUT_DIR/mibee-eye ($SIZE)"
echo "  Config:  $OUT_DIR/config.toml"
echo "  Migrations: $OUT_DIR/migrations/"
echo ""
echo "Next: ./scripts/deploy.sh device-1   # or device-2"
