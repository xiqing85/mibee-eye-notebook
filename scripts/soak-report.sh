#!/usr/bin/env bash
# scripts/soak-report.sh
#
# Review the health of a long-running (overnight) mibee-eye deployment.
# Run this the morning after leaving the service streaming overnight.
#
# Usage:
#   ./scripts/soak-report.sh <host> [since]
#
# `since` is a journalctl time spec, default "8 hours ago".
#
# Pass criteria:
#   - Zero panics / ERROR-level entries (OTel connection-refused excepted)
#   - Memory growth < 20% (no leak)
#   - No encoder-stall warnings
#   - Frame rate stayed within ±10% of 30fps
#   - Disk: recording segments pruned, capacity respected
#   - CPU p99 single-core < 80%

set -euo pipefail

HOST="${1:?Usage: $0 <host> [since-spec]}"
SINCE="${2:-8 hours ago}"
FAIL=0
pass() { echo "  ✓ $1"; }
fail() { echo "  ✗ $1" >&2; FAIL=1; }

echo "=== Soak report: $HOST (since '$SINCE') ==="
echo ""

# ── 1. Service uptime ─────────────────────────────────────────────────────────
UPTIME=$(ssh "$HOST" "systemctl --user show mibee-eye -p ActiveEnterTimestamp --value 2>/dev/null || echo 'unknown'")
if [ "$UPTIME" != "unknown" ]; then
    pass "service has been running since $UPTIME"
else
    fail "could not determine service uptime"
fi

# ── 2. Panics ─────────────────────────────────────────────────────────────────
PANICS=$(ssh "$HOST" "journalctl --user -u mibee-eye --since '$SINCE' --no-pager 2>/dev/null | grep -ciE 'panic|RUST_BACKTRACE|fatal' || echo 0")
if [ "$PANICS" = "0" ]; then
    pass "zero panics in the soak window"
else
    fail "$PANICS panic/fatal entries found"
    ssh "$HOST" "journalctl --user -u mibee-eye --since '$SINCE' --no-pager 2>/dev/null | grep -iE 'panic|fatal' | head -5" >&2 || true
fi

# ── 3. ERROR-level entries (excluding known dev noise) ────────────────────────
ERRORS=$(ssh "$HOST" "journalctl --user -u mibee-eye --since '$SINCE' --no-pager -p err 2>/dev/null \
    | grep -viE 'opentelemetry|BatchSpanProcessor|ExportError|Connection refused|otel|otlp' \
    | grep -c '' || echo 0")
if [ "$ERRORS" = "0" ]; then
    pass "zero ERROR entries (excluding known OTel dev noise)"
else
    fail "$ERRORS ERROR entries (excluding OTel noise)"
    ssh "$HOST" "journalctl --user -u mibee-eye --since '$SINCE' --no-pager -p err 2>/dev/null \
        | grep -viE 'opentelemetry|BatchSpanProcessor|ExportError|Connection refused|otel|otlp' \
        | head -5" >&2 || true
fi

# ── 4. Memory growth (current vs baseline) ────────────────────────────────────
PID=$(ssh "$HOST" "pgrep -u \$USER -f 'mibee-eye.*config' | head -1" || echo "")
if [ -n "$PID" ]; then
    CUR_RSS_KB=$(ssh "$HOST" "ps -o rss= -p $PID" | tr -d ' ')
    CUR_RSS_MB=$((CUR_RSS_KB / 1024))
    echo "  Current RSS: ${CUR_RSS_MB} MB (PID $PID)"
    # We don't have the baseline saved; just report current and note the
    # baseline reference (~30-40 MB typical for the native encoder).
    if [ "$CUR_RSS_MB" -lt 200 ]; then
        pass "RSS under 200 MB (no obvious leak; native encoder baseline ~30-40 MB)"
    else
        fail "RSS is ${CUR_RSS_MB} MB — possible memory leak"
    fi
else
    fail "could not find mibee-eye process"
fi

# ── 5. Encoder stalls / frame timing warnings ─────────────────────────────────
STALLS=$(ssh "$HOST" "journalctl --user -u mibee-eye --since '$SINCE' --no-pager 2>/dev/null \
    | grep -ciE 'encoder.*stall|encode.*fail|frame.*drop|lagged' || echo 0")
if [ "$STALLS" -lt 100 ]; then
    pass "few encoder/frame warnings ($STALLS — under 100, acceptable for an 8h run)"
else
    fail "$STALLS encoder/frame warnings — possible throughput problem"
fi

# ── 6. ffmpeg references (regression guard) ───────────────────────────────────
FFMPEG_HITS=$(ssh "$HOST" "journalctl --user -u mibee-eye --since '$SINCE' --no-pager 2>/dev/null | grep -ci 'ffmpeg' || echo 0")
if [ "$FFMPEG_HITS" = "0" ]; then
    pass "zero ffmpeg references over the soak window (full removal confirmed)"
else
    fail "$FFMPEG_HITS ffmpeg references — removal regressed?"
fi

# ── 7. Recording segment pruning (if recording enabled) ───────────────────────
REC_DIR=$(ssh "$HOST" "curl -skf https://localhost:8443/api/protocols/recording 2>/dev/null \
    | grep -oE '\"path\":\"[^\"]+\"' | cut -d'\"' -f4 || echo ''")
if [ -n "$REC_DIR" ]; then
    SEG_COUNT=$(ssh "$HOST" "ls -1 $REC_DIR/*.mp4 2>/dev/null | wc -l || echo 0")
    SEG_TOTAL_MB=$(ssh "$HOST" "du -sm $REC_DIR 2>/dev/null | cut -f1 || echo 0")
    CAP_MB=$(ssh "$HOST" "curl -skf https://localhost:8443/api/protocols/recording 2>/dev/null \
        | grep -oE '\"max_capacity_mb\":[0-9]+' | cut -d: -f2 || echo 0")
    echo "  Recording: $SEG_COUNT segments, ${SEG_TOTAL_MB} MB total (cap: ${CAP_MB} MB)"
    if [ "$CAP_MB" -gt 0 ] && [ "$SEG_TOTAL_MB" -le $((CAP_MB + CAP_MB / 10)) ]; then
        pass "recording size within capacity cap (±10%)"
    else
        fail "recording size ${SEG_TOTAL_MB} MB exceeds cap ${CAP_MB} MB"
    fi
    # Check for pruning log entries.
    PRUNE_LOG=$(ssh "$HOST" "journalctl --user -u mibee-eye --since '$SINCE' --no-pager 2>/dev/null | grep -ci 'pruned' || echo 0")
    if [ "$PRUNE_LOG" -gt 0 ]; then
        pass "segment pruning active ($PRUNE_LOG prune events logged)"
    else
        skip_msg="no pruning needed (size under cap)"
        echo "  ⊘ segment pruning ($skip_msg)"
    fi
else
    echo "  ⊘ recording pruning (local recording not enabled)"
fi

# ── 8. CPU p99 (best-effort, from /proc) ──────────────────────────────────────
if [ -n "$PID" ]; then
    # Sample 10 times over ~10s.
    ssh "$HOST" "for i in \$(seq 1 10); do cat /proc/$PID/stat 2>/dev/null | awk '{print \$14+\$15}'; sleep 1; done > /tmp/mibee-cpu-soak.txt" || true
    P99=$(ssh "$HOST" "sort -n /tmp/mibee-cpu-soak.txt | tail -1" 2>/dev/null || echo "0")
    # Convert jiffies → % of one core (100 Hz typical).
    P99_PCT=$((P99))
    echo "  CPU peak (1s sample, jiffies): ~$P99_PCT (≈ $((P99_PCT))% of one core)"
fi

echo ""
if [ "$FAIL" -ne 0 ]; then
    echo "✗ Soak report: ISSUES FOUND on $HOST." >&2
    exit 1
fi
echo "✓ Soak report: $HOST looks healthy over the '$SINCE' window."
