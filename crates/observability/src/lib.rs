#![cfg_attr(test, deny(warnings))]

/// Tracing subscriber initialisation (EnvFilter, fmt, OpenTelemetry OTLP).
pub mod tracing_setup;

/// Loki log shipping layer.
pub mod loki_layer;
/// Custom Prometheus metrics (streams, bytes, errors).
pub mod metrics;

// Re-export the most commonly used functions for ergonomic access.
pub use metrics::{
    increment_bytes_received, increment_capture_errors, increment_gb28181_register_status,
    increment_onvif_discovery_requests, increment_rtmp_push_bytes, increment_rtmp_push_errors,
    increment_rtsp_bytes_sent, increment_rtsp_sessions, register_metrics, render_metrics,
    set_active_streams, set_audio_level,
    // New metrics
    increment_http_requests, increment_auth_failures, set_recording_active,
    inc_recording_active, dec_recording_active, increment_frame_drops,
};
pub use tracing_setup::init_tracing;
