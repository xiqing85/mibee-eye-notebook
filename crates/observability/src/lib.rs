#![cfg_attr(test, deny(warnings))]

/// Tracing subscriber initialisation (EnvFilter, fmt, OpenTelemetry OTLP).
pub mod tracing_setup;

/// Custom Prometheus metrics (streams, bytes, errors).
pub mod metrics;

// Re-export the most commonly used functions for ergonomic access.
pub use metrics::{
    increment_bytes_received, increment_capture_errors, register_metrics, render_metrics,
    set_active_streams,
};
pub use tracing_setup::init_tracing;
