use std::collections::HashMap;
use tracing_loki::BackgroundTask;

/// Runtime configuration for the optional Loki log shipping layer.
///
/// Constructed from the `[observability.logs]` TOML section (see `RemoteLogConfig`
/// in `src/config.rs`), then passed into [`build_loki_layer`].
pub struct LokiConfig {
    pub endpoint: String,
    pub batch_size: usize,
    pub flush_interval_secs: u64,
    pub labels: HashMap<String, String>,
}

/// Build a [`tracing_loki::Layer`] and its [`BackgroundTask`] from the given
/// [`LokiConfig`].
///
/// This function is **fail-open**: if the endpoint URL is malformed, label
/// validation fails, or any other construction error occurs, a warning is
/// emitted via `eprintln` (tracing is not yet initialised) and `None` is
/// returned. The caller should continue with fallback logging (stdout-only).
pub fn build_loki_layer(config: LokiConfig) -> Option<(tracing_loki::Layer, BackgroundTask)> {
    // If the endpoint is empty, treat it as "not configured" and bail silently.
    if config.endpoint.is_empty() {
        return None;
    }

    let url = match tracing_loki::url::Url::parse(&config.endpoint) {
        Ok(u) => u,
        Err(e) => {
            eprintln!(
                "WARN: Failed to parse Loki endpoint URL '{}': {}. \
                 Remote log shipping disabled.",
                config.endpoint, e
            );
            return None;
        }
    };

    let mut builder = tracing_loki::builder();

    // Add configured labels.
    for (key, value) in &config.labels {
        match builder.clone().label(key.as_str(), value.as_str()) {
            Ok(b) => builder = b,
            Err(e) => {
                eprintln!(
                    "WARN: Invalid Loki label '{}={}': {}. Skipping label.",
                    key, value, e
                );
                // Continue with remaining labels — fail sub-element, not the
                // whole layer.
            }
        }
    }

    match builder.build_url(url) {
        Ok((layer, task)) => Some((layer, task)),
        Err(e) => {
            eprintln!(
                "WARN: Failed to build Loki layer for endpoint '{}': {}. \
                 Remote log shipping disabled.",
                config.endpoint, e
            );
            None
        }
    }
}
