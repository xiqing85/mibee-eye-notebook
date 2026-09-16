#!/usr/bin/env bash
# scripts/test-perf.sh
#
# Performance benchmark for the native (openh264) encoder on a target device.
# Collects CPU%, RSS memory, encoder throughput, and frame timing over a 60s
# window. Designed to compare against the old ffmpeg-subprocess baseline.
#
# Usage:
#   ./scripts/test-perf.sh <host> [camera_id] [duration_secs]
#
# Output: a summary table printed to stdout. Also writes raw samples to
# /tmp/mibee-perf-<host>.tsv on the remote host for offline analysis.

set -euo pipefail

HOST="${1:?Usage: $0 <host> [camera_id] [duration_secs]}"
CAM_ID="${2:-}"
DURATION="${3:-60}"

echo "=== Performance benchmark: $HOST (${DURATION}s) ==="

# Discover camera_id if not given.
if [ -z "$CAM_ID" ]; then
    CAM_ID=$(ssh "$HOST" "curl -skf https://localhost:8443/api/cameras" 2>/dev/null \
        | grep -oE '"camera_id":"[^"]+"' | head -1 | cut -d'"' -f4 || echo "")
    [ -z "$CAM_ID" ] && { echo "ERROR: no cameras registered on $HOST" >&2; exit 1; }
fi
echo "  camera_id: $CAM_ID"

# Ensure stream is running.
STREAM_STATE=$(ssh "$HOST" "curl -skf https://localhost:8443/api/cameras/$CAM_ID" 2>/dev/null \
    | grep -oE '"status":"[^"]+"' | head -1 | cut -d'"' -f4 || echo "")
if [ "$STREAM_STATE" != "running" ]; then
    echo "  → Starting stream..."
    ssh "$HOST" "curl -skf -X POST https://localhost:8443/api/cameras/$CAM_ID/start" >/dev/null 2>&1 || true
    sleep 3
fi

# Warm up the encoder (let it stabilize past the first IDR).
echo "  Warming up (10s)..."
sleep 10

# Capture PID + baseline metrics.
PID=$(ssh "$HOST" "pgrep -u \$USER -f 'mibee-eye.*config' | head -1")
if [ -z "$PID" ]; then
    echo "ERROR: could not find mibee-eye process PID on $HOST" >&2
    exit 1
fi
echo "  PID: $PID"

BASE_RSS=$(ssh "$HOST" "ps -o rss= -p $PID" | tr -d ' ')
echo "  Baseline RSS: $((BASE_RSS / 1024)) MB"

# ── Sample CPU% every 5s for the duration ─────────────────────────────────────
echo "  Sampling CPU% + RSS for ${DURATION}s (every 5s)..."
TSV="/tmp/mibee-perf-$HOST.tsv"
ssh "$HOST" "echo 'elapsed_s cpu_pct rss_kb' > $TSV"

SAMPLES=$(("$DURATION" / 5))
for i in $(seq 1 "$SAMPLES"); do
    ssh "$HOST" "top -b -n 1 -d 1 -p $PID 2>/dev/null \
        | awk -v pid=$PID -v t=$(((i-1)*5)) '\$1==pid {print t, \$9, \$6}' >> $TSV" || true
    sleep 5
done

# ── Collect encoder throughput from /metrics ──────────────────────────────────
# mibee_frames_emitted_total is the frames_emitted counter (if instrumented).
METRICS=$(ssh "$HOST" "curl -skf https://localhost:8443/metrics" 2>/dev/null || echo "")
FRAMES=$(echo "$METRICS" | grep -E "frames_emitted|mjpeg_frames|video_frames" | head -1 || echo "")

# ── Summary ───────────────────────────────────────────────────────────────────
echo ""
echo "── Results: $HOST ──────────────────────────────────────────────"
ssh "$HOST" "awk -v base=$BASE_RSS '
    NR>1 {
        cpu_sum += \$2; rss_sum += \$3; n++;
        if (\$2 > cpu_max) cpu_max = \$2;
        if (\$3 > rss_max) rss_max = \$3;
    }
    END {
        if (n==0) { print \"  (no samples)\"; exit }
        printf \"  CPU%% avg:       %.1f%%\n\", cpu_sum/n
        printf \"  CPU%% peak:      %.1f%%\n\", cpu_max
        printf \"  RSS avg:        %d MB\n\", rss_sum/n/1024
        printf \"  RSS peak:       %d MB\n\", rss_max/1024
        printf \"  RSS baseline:   %d MB\n\", base/1024
        printf \"  RSS growth:     %d MB\n\", (rss_sum/n - base)/1024
    }
' $TSV"

echo ""
echo "  Encoder counter: ${FRAMES:-(not exposed in /metrics)}"
echo "  Raw samples:     $HOST:$TSV"
echo ""
echo "  Reference (old ffmpeg baseline): ~64 MB per camera subprocess."
echo "  Target: i5-6200U sustains 720p@30 with <60% single-core CPU."
echo ""
echo "✓ Benchmark complete."
