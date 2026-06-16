use anyhow::{Context, Result};
use prometheus::{
    Encoder, GaugeVec, IntCounter, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};
use std::sync::OnceLock;

/// Container for all custom Prometheus metrics.
///
/// Constructed once via [`register_metrics`] and stored in a global `OnceLock`.
/// Tests can construct independent `Metrics` instances to avoid global-state
/// conflicts.
pub struct Metrics {
    registry: Registry,
    active_streams: IntGauge,
    bytes_received: IntCounterVec,
    capture_errors: IntCounterVec,
    // ─── Protocol-specific counters ───────────────────────────
    rtsp_sessions: IntCounterVec,
    rtsp_bytes_sent: IntCounter,
    rtmp_push_bytes: IntCounter,
    rtmp_push_errors: IntCounter,
    onvif_discovery_requests: IntCounter,
    gb28181_register_status: IntCounterVec,
    audio_level_db: GaugeVec,
    // ─── New metrics ────────────────────────────────
    http_requests_total: IntCounterVec,
    auth_failures_total: IntCounterVec,
    recording_active: IntGauge,
    frame_drops_total: IntCounter,
}

// ---------------------------------------------------------------------------
// Global singleton
// ---------------------------------------------------------------------------

static GLOBAL_METRICS: OnceLock<Metrics> = OnceLock::new();

/// Register all custom metrics with the global Prometheus registry.
///
/// Must be called exactly once during application startup. Calling it a second
/// time will return an error.
pub fn register_metrics() -> Result<()> {
    let metrics = Metrics::new().context("failed to create metrics")?;
    GLOBAL_METRICS
        .set(metrics)
        .map_err(|_| anyhow::anyhow!("metrics already registered"))
}

fn global_metrics() -> &'static Metrics {
    GLOBAL_METRICS
        .get()
        .expect("metrics not registered — call register_metrics() during startup")
}

// ---------------------------------------------------------------------------
// Public API functions
// ---------------------------------------------------------------------------

/// Increment the `mibee_rec_bytes_received` counter for a camera.
pub fn increment_bytes_received(camera_id: &str, bytes: u64) {
    global_metrics()
        .bytes_received
        .with_label_values(&[camera_id])
        .inc_by(bytes);
}

/// Increment the `mibee_rec_capture_errors` counter for a camera.
pub fn increment_capture_errors(camera_id: &str) {
    global_metrics()
        .capture_errors
        .with_label_values(&[camera_id])
        .inc();
}

// ─── Protocol-specific counter helpers ─────────────────────────────────

/// Increment the `mibee_rec_rtsp_sessions_total` counter by status.
pub fn increment_rtsp_sessions(status: &str) {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.rtsp_sessions.with_label_values(&[status]).inc();
    }
}

/// Increment the `mibee_rec_rtsp_bytes_sent_total` counter.
pub fn increment_rtsp_bytes_sent(bytes: u64) {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.rtsp_bytes_sent.inc_by(bytes);
    }
}

/// Increment the `mibee_rec_rtmp_push_bytes_total` counter.
pub fn increment_rtmp_push_bytes(bytes: u64) {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.rtmp_push_bytes.inc_by(bytes);
    }
}

/// Increment the `mibee_rec_rtmp_push_errors_total` counter.
pub fn increment_rtmp_push_errors() {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.rtmp_push_errors.inc();
    }
}

/// Increment the `mibee_rec_onvif_discovery_requests_total` counter.
pub fn increment_onvif_discovery_requests() {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.onvif_discovery_requests.inc();
    }
}

/// Increment the `mibee_rec_gb28181_register_status` counter by status.
pub fn increment_gb28181_register_status(status: &str) {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.gb28181_register_status.with_label_values(&[status]).inc();
    }
}

/// Set the `mibee_rec_streams_active` gauge to an absolute value.
pub fn set_active_streams(count: i64) {
    global_metrics().active_streams.set(count);
}
/// Set the `mibee_rec_audio_level_db` gauge for a stream.
///
/// Silently returns if metrics have not yet been registered — this allows
/// audio capture callbacks to fire before `register_metrics()` is called
/// during startup without panicking.
pub fn set_audio_level(stream_id: &str, db_level: f64) {
    // SAFETY: Audio callbacks run on real-time threads; we must not panic.
    // Using GLOBAL_METRICS.get() avoids the `expect()` in global_metrics().
    if let Some(metrics) = GLOBAL_METRICS.get() {
        metrics
            .audio_level_db
            .with_label_values(&[stream_id])
            .set(db_level);
    }
}

// ─── New metric helpers ─────────────────────────────────────────

/// Increment the `mibee_http_requests_total` counter.
pub fn increment_http_requests(method: &str, path: &str, status: u16) {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.http_requests_total
            .with_label_values(&[method, path, &status.to_string()])
            .inc();
    }
}

/// Increment the `mibee_auth_failures_total` counter by failure type.
///
/// `failure_type` must be one of `"bad_password"`, `"locked_out"`, or `"rate_limited"`.
pub fn increment_auth_failures(failure_type: &str) {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.auth_failures_total.with_label_values(&[failure_type]).inc();
    }
}

/// Set the `mibee_recording_active` gauge to an absolute value.
pub fn set_recording_active(count: i64) {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.recording_active.set(count);
    }
}

/// Increment the `mibee_recording_active` gauge by 1.
pub fn inc_recording_active() {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.recording_active.inc();
    }
}

/// Decrement the `mibee_recording_active` gauge by 1.
pub fn dec_recording_active() {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.recording_active.dec();
    }
}

/// Increment the `mibee_frame_drops_total` counter.
pub fn increment_frame_drops() {
    if let Some(m) = GLOBAL_METRICS.get() {
        m.frame_drops_total.inc();
    }
}

/// Render all registered metrics in Prometheus text-0.0.4 exposition format.
pub fn render_metrics() -> String {
    let metric_families = global_metrics().registry.gather();
    let encoder = TextEncoder::new();
    let mut buffer = Vec::new();
    // encode() only fails if the writer errors — Vec<u8> never does.
    encoder
        .encode(&metric_families, &mut buffer)
        .expect("prometheus text encoding should never fail for Vec<u8>");
    String::from_utf8(buffer).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Internal implementation
// ---------------------------------------------------------------------------

impl Metrics {
    /// Construct a new `Metrics` instance with a fresh registry and all
    /// custom metrics registered.
    fn new() -> Result<Self> {
        let registry = Registry::new();

        let active_streams = IntGauge::new(
            "mibee_rec_streams_active",
            "Number of currently active streams",
        )?;
        registry.register(Box::new(active_streams.clone()))?;

        let bytes_received = IntCounterVec::new(
            Opts::new(
                "mibee_rec_bytes_received",
                "Total bytes received from cameras",
            ),
            &["camera_id"],
        )?;
        registry.register(Box::new(bytes_received.clone()))?;

        let capture_errors = IntCounterVec::new(
            Opts::new("mibee_rec_capture_errors", "Total capture errors by camera"),
            &["camera_id"],
        )?;
        registry.register(Box::new(capture_errors.clone()))?;

        // ─── Protocol-specific counters ───────────────────────────
        let rtsp_sessions = IntCounterVec::new(
            Opts::new(
                "mibee_rec_rtsp_sessions_total",
                "Total number of RTSP sessions by status",
            ),
            &["status"],
        )?;
        registry.register(Box::new(rtsp_sessions.clone()))?;

        let rtsp_bytes_sent = IntCounter::new(
            "mibee_rec_rtsp_bytes_sent_total",
            "Total RTSP/RTP bytes transmitted",
        )?;
        registry.register(Box::new(rtsp_bytes_sent.clone()))?;

        let rtmp_push_bytes = IntCounter::new(
            "mibee_rec_rtmp_push_bytes_total",
            "Total bytes pushed via RTMP",
        )?;
        registry.register(Box::new(rtmp_push_bytes.clone()))?;

        let rtmp_push_errors =
            IntCounter::new("mibee_rec_rtmp_push_errors_total", "Total RTMP push errors")?;
        registry.register(Box::new(rtmp_push_errors.clone()))?;

        let onvif_discovery_requests = IntCounter::new(
            "mibee_rec_onvif_discovery_requests_total",
            "Total ONVIF WS-Discovery probe requests received",
        )?;
        registry.register(Box::new(onvif_discovery_requests.clone()))?;

        let gb28181_register_status = IntCounterVec::new(
            Opts::new(
                "mibee_rec_gb28181_register_status",
                "Total GB28181 registration attempts by status",
            ),
            &["status"],
        )?;
        registry.register(Box::new(gb28181_register_status.clone()))?;

        // ─── Audio level gauge ───────────────────────────────────
        let audio_level_db = GaugeVec::new(
            Opts::new(
                "mibee_rec_audio_level_db",
                "Current audio input level in dBFS",
            ),
            &["stream_id"],
        )?;
        registry.register(Box::new(audio_level_db.clone()))?;

        // ─── New metrics ───────────────────────────────────
        let http_requests_total = IntCounterVec::new(
            Opts::new(
                "mibee_http_requests_total",
                "Total HTTP requests by method, path, and status",
            ),
            &["method", "path", "status"],
        )?;
        registry.register(Box::new(http_requests_total.clone()))?;

        let auth_failures_total = IntCounterVec::new(
            Opts::new(
                "mibee_auth_failures_total",
                "Total authentication failures by type",
            ),
            &["type"],
        )?;
        registry.register(Box::new(auth_failures_total.clone()))?;

        let recording_active = IntGauge::new(
            "mibee_recording_active",
            "Number of active recording outputs",
        )?;
        registry.register(Box::new(recording_active.clone()))?;

        let frame_drops_total = IntCounter::new(
            "mibee_frame_drops_total",
            "Total number of dropped frames in broadcast",
        )?;
        registry.register(Box::new(frame_drops_total.clone()))?;

        Ok(Metrics {
            registry,
            active_streams,
            bytes_received,
            capture_errors,
            rtsp_sessions,
            rtsp_bytes_sent,
            rtmp_push_bytes,
            rtmp_push_errors,
            onvif_discovery_requests,
            gb28181_register_status,
            audio_level_db,
            http_requests_total,
            auth_failures_total,
            recording_active,
            frame_drops_total,
        })
    }

    #[cfg(test)]
    fn render(&self) -> String {
        let metric_families = self.registry.gather();
        let encoder = TextEncoder::new();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .expect("prometheus text encoding should never fail for Vec<u8>");
        String::from_utf8(buffer).unwrap_or_default()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_create_and_render() {
        let m = Metrics::new().expect("metrics creation should succeed");

        // Increment counters/gauges so they appear in output (Prometheus
        // omits zero-value counter vecs from rendered output)
        m.bytes_received.with_label_values(&["test"]).inc_by(1);
        m.capture_errors.with_label_values(&["test"]).inc();
        m.active_streams.set(0);
        // Activate protocol counters so they appear in output
        m.rtsp_sessions.with_label_values(&["active"]).inc();
        m.rtsp_bytes_sent.inc_by(100);
        m.rtmp_push_bytes.inc_by(200);
        m.rtmp_push_errors.inc();
        m.onvif_discovery_requests.inc();
        m.gb28181_register_status
            .with_label_values(&["registered"])
            .inc();
        // Activate new metrics so they appear in output
        m.http_requests_total
            .with_label_values(&["GET", "/health", "200"])
            .inc();
        m.auth_failures_total
            .with_label_values(&["bad_password"])
            .inc();
        m.recording_active.set(1);
        m.frame_drops_total.inc_by(3);

        let output = m.render();
        // All metrics should appear in the rendered text
        assert!(
            output.contains("mibee_rec_streams_active"),
            "output should contain active_streams gauge"
        );
        assert!(
            output.contains("mibee_rec_bytes_received"),
            "output should contain bytes_received counter"
        );
        assert!(
            output.contains("mibee_rec_capture_errors"),
            "output should contain capture_errors counter"
        );
        assert!(
            output.contains("mibee_rec_rtsp_sessions_total"),
            "output should contain rtsp_sessions counter"
        );
        assert!(
            output.contains("mibee_rec_rtsp_bytes_sent_total"),
            "output should contain rtsp_bytes_sent counter"
        );
        assert!(
            output.contains("mibee_rec_rtmp_push_bytes_total"),
            "output should contain rtmp_push_bytes counter"
        );
        assert!(
            output.contains("mibee_rec_rtmp_push_errors_total"),
            "output should contain rtmp_push_errors counter"
        );
        assert!(
            output.contains("mibee_rec_onvif_discovery_requests_total"),
            "output should contain onvif_discovery_requests counter"
        );
        assert!(
            output.contains("mibee_rec_gb28181_register_status"),
            "output should contain gb28181_register_status counter"
        );
        assert!(
            output.contains("mibee_http_requests_total"),
            "output should contain http_requests_total counter"
        );
        assert!(
            output.contains("mibee_auth_failures_total"),
            "output should contain auth_failures_total counter"
        );
        assert!(
            output.contains("mibee_recording_active"),
            "output should contain recording_active gauge"
        );
        assert!(
            output.contains("mibee_frame_drops_total"),
            "output should contain frame_drops_total counter"
        );
    }

    #[test]
    fn test_protocol_counters_increment() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.rtsp_sessions.with_label_values(&["active"]).inc();
        m.rtsp_sessions.with_label_values(&["active"]).inc();
        m.rtsp_sessions.with_label_values(&["closed"]).inc();
        m.rtsp_bytes_sent.inc_by(1500);
        m.rtmp_push_bytes.inc_by(4096);
        m.rtmp_push_errors.inc();
        m.rtmp_push_errors.inc();
        m.onvif_discovery_requests.inc();
        m.gb28181_register_status
            .with_label_values(&["registered"])
            .inc();
        m.gb28181_register_status
            .with_label_values(&["failed"])
            .inc();
        m.gb28181_register_status
            .with_label_values(&["failed"])
            .inc();

        let output = m.render();

        assert!(
            output.contains("mibee_rec_rtsp_sessions_total{status=\"active\"} 2"),
            "active sessions should be 2\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains("mibee_rec_rtsp_sessions_total{status=\"closed\"} 1"),
            "closed sessions should be 1\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains("mibee_rec_rtsp_bytes_sent_total 1500"),
            "rtsp bytes should be 1500\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains("mibee_rec_rtmp_push_bytes_total 4096"),
            "rtmp bytes should be 4096\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains("mibee_rec_rtmp_push_errors_total 2"),
            "rtmp errors should be 2\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains("mibee_rec_onvif_discovery_requests_total 1"),
            "discovery requests should be 1\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains("mibee_rec_gb28181_register_status{status=\"registered\"} 1"),
            "registered should be 1\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains("mibee_rec_gb28181_register_status{status=\"failed\"} 2"),
            "failed should be 2\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_increment_bytes_received() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.bytes_received.with_label_values(&["cam_1"]).inc_by(100);
        m.bytes_received.with_label_values(&["cam_1"]).inc_by(50);
        m.bytes_received.with_label_values(&["cam_2"]).inc_by(200);

        let output = m.render();

        // Total for cam_1 should be 150
        assert!(
            output.contains(r#"mibee_rec_bytes_received{camera_id="cam_1"} 150"#),
            "cam_1 should show 150 bytes\n=== output ===\n{}",
            output
        );
        // Total for cam_2 should be 200
        assert!(
            output.contains(r#"mibee_rec_bytes_received{camera_id="cam_2"} 200"#),
            "cam_2 should show 200 bytes\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_increment_capture_errors() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.capture_errors.with_label_values(&["cam_1"]).inc();
        m.capture_errors.with_label_values(&["cam_1"]).inc();
        m.capture_errors.with_label_values(&["cam_2"]).inc();

        let output = m.render();

        assert!(
            output.contains(r#"mibee_rec_capture_errors{camera_id="cam_1"} 2"#),
            "cam_1 should have 2 errors\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains(r#"mibee_rec_capture_errors{camera_id="cam_2"} 1"#),
            "cam_2 should have 1 error\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_set_active_streams() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.active_streams.set(3);
        let output = m.render();
        assert!(
            output.contains("mibee_rec_streams_active 3"),
            "active_streams should be 3\n=== output ===\n{}",
            output
        );

        m.active_streams.set(0);
        let output = m.render();
        assert!(
            output.contains("mibee_rec_streams_active 0"),
            "active_streams should be 0\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_registry_freshness() {
        // Each Metrics instance gets its own registry — they should not
        // interfere.
        let m1 = Metrics::new().expect("m1 creation");
        let m2 = Metrics::new().expect("m2 creation");

        m1.active_streams.set(42);
        m2.active_streams.set(99);

        let out1 = m1.render();
        let out2 = m2.render();

        assert!(
            out1.contains("mibee_rec_streams_active 42"),
            "m1 should have 42\n=== out1 ===\n{}",
            out1
        );
        assert!(
            out2.contains("mibee_rec_streams_active 99"),
            "m2 should have 99\n=== out2 ===\n{}",
            out2
        );
    }

    #[test]
    fn test_audio_level_gauge() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.audio_level_db.with_label_values(&["default"]).set(-12.5);
        let output = m.render();
        assert!(
            output.contains(r#"mibee_rec_audio_level_db{stream_id="default"} -12.5"#),
            "audio_level_db should show -12.5\n=== output ===\n{}",
            output
        );

        m.audio_level_db.with_label_values(&["default"]).set(0.0);
        let output = m.render();
        assert!(
            output.contains(r#"mibee_rec_audio_level_db{stream_id="default"} 0"#),
            "audio_level_db should show 0\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_increment_http_requests() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.http_requests_total
            .with_label_values(&["POST", "/api/auth/login", "200"])
            .inc();
        m.http_requests_total
            .with_label_values(&["GET", "/health", "200"])
            .inc();
        m.http_requests_total
            .with_label_values(&["GET", "/health", "200"])
            .inc();

        let output = m.render();
        assert!(
            output.contains(r#"mibee_http_requests_total{method="POST",path="/api/auth/login",status="200"} 1"#),
            "POST login should appear once\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains(r#"mibee_http_requests_total{method="GET",path="/health",status="200"} 2"#),
            "GET health should appear twice\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_increment_auth_failures() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.auth_failures_total
            .with_label_values(&["bad_password"])
            .inc();
        m.auth_failures_total
            .with_label_values(&["bad_password"])
            .inc();
        m.auth_failures_total
            .with_label_values(&["locked_out"])
            .inc();
        m.auth_failures_total
            .with_label_values(&["rate_limited"])
            .inc();

        let output = m.render();
        assert!(
            output.contains(r#"mibee_auth_failures_total{type="bad_password"} 2"#),
            "bad_password should be 2\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains(r#"mibee_auth_failures_total{type="locked_out"} 1"#),
            "locked_out should be 1\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains(r#"mibee_auth_failures_total{type="rate_limited"} 1"#),
            "rate_limited should be 1\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_set_recording_active() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.recording_active.set(2);
        let output = m.render();
        assert!(
            output.contains("mibee_recording_active 2"),
            "recording_active should be 2\n=== output ===\n{}",
            output
        );

        m.recording_active.set(0);
        let output = m.render();
        assert!(
            output.contains("mibee_recording_active 0"),
            "recording_active should be 0\n=== output ===\n{}",
            output
        );
    }

    #[test]
    fn test_increment_frame_drops() {
        let m = Metrics::new().expect("metrics creation should succeed");

        m.frame_drops_total.inc();
        m.frame_drops_total.inc();
        m.frame_drops_total.inc();

        let output = m.render();
        assert!(
            output.contains("mibee_frame_drops_total 3"),
            "frame_drops should be 3\n=== output ===\n{}",
            output
        );
    }
}
