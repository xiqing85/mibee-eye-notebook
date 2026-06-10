use anyhow::{Context, Result};
use prometheus::{Encoder, IntCounterVec, IntGauge, Opts, Registry, TextEncoder};
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

/// Increment the `notebook_cam_bytes_received` counter for a camera.
pub fn increment_bytes_received(camera_id: &str, bytes: u64) {
    global_metrics()
        .bytes_received
        .with_label_values(&[camera_id])
        .inc_by(bytes);
}

/// Increment the `notebook_cam_capture_errors` counter for a camera.
pub fn increment_capture_errors(camera_id: &str) {
    global_metrics()
        .capture_errors
        .with_label_values(&[camera_id])
        .inc();
}

/// Set the `notebook_cam_streams_active` gauge to an absolute value.
pub fn set_active_streams(count: i64) {
    global_metrics().active_streams.set(count);
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
            "notebook_cam_streams_active",
            "Number of currently active streams",
        )?;
        registry.register(Box::new(active_streams.clone()))?;

        let bytes_received = IntCounterVec::new(
            Opts::new(
                "notebook_cam_bytes_received",
                "Total bytes received from cameras",
            ),
            &["camera_id"],
        )?;
        registry.register(Box::new(bytes_received.clone()))?;

        let capture_errors = IntCounterVec::new(
            Opts::new(
                "notebook_cam_capture_errors",
                "Total capture errors by camera",
            ),
            &["camera_id"],
        )?;
        registry.register(Box::new(capture_errors.clone()))?;

        Ok(Metrics {
            registry,
            active_streams,
            bytes_received,
            capture_errors,
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

        let output = m.render();
        // All three metrics should appear in the rendered text
        assert!(
            output.contains("notebook_cam_streams_active"),
            "output should contain active_streams gauge"
        );
        assert!(
            output.contains("notebook_cam_bytes_received"),
            "output should contain bytes_received counter"
        );
        assert!(
            output.contains("notebook_cam_capture_errors"),
            "output should contain capture_errors counter"
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
            output.contains(r#"notebook_cam_bytes_received{camera_id="cam_1"} 150"#),
            "cam_1 should show 150 bytes\n=== output ===\n{}",
            output
        );
        // Total for cam_2 should be 200
        assert!(
            output.contains(r#"notebook_cam_bytes_received{camera_id="cam_2"} 200"#),
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
            output.contains(r#"notebook_cam_capture_errors{camera_id="cam_1"} 2"#),
            "cam_1 should have 2 errors\n=== output ===\n{}",
            output
        );
        assert!(
            output.contains(r#"notebook_cam_capture_errors{camera_id="cam_2"} 1"#),
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
            output.contains("notebook_cam_streams_active 3"),
            "active_streams should be 3\n=== output ===\n{}",
            output
        );

        m.active_streams.set(0);
        let output = m.render();
        assert!(
            output.contains("notebook_cam_streams_active 0"),
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
            out1.contains("notebook_cam_streams_active 42"),
            "m1 should have 42\n=== out1 ===\n{}",
            out1
        );
        assert!(
            out2.contains("notebook_cam_streams_active 99"),
            "m2 should have 99\n=== out2 ===\n{}",
            out2
        );
    }
}
