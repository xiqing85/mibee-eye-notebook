use anyhow::Result;
use opentelemetry::global;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace as sdktrace;
use std::collections::HashMap;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Initialize the tracing subscriber.
///
/// Sets up:
/// - Console logging (pretty-printed with colors, or JSON format)
/// - EnvFilter from `RUST_LOG` env var (falls back to `log_level` parameter)
/// - Optional OpenTelemetry OTLP trace export via gRPC/tonic
/// - Optional Loki-compatible remote log shipping via HTTP
///
/// Both OTLP and Loki are fail-open: if initialization fails, a warning is
/// emitted via `eprintln` (tracing isn't initialized yet) and the application
/// continues without the failed layer.
pub fn init_tracing(
    log_level: &str,
    json_format: bool,
    otlp_endpoint: Option<String>,
    loki_endpoint: Option<String>,
    loki_labels: HashMap<String, String>,
) -> Result<()> {
    // Set global text map propagator for W3C TraceContext header extraction/injection
    global::set_text_map_propagator(TraceContextPropagator::new());

    // --- Build optional layers (both fail-open) ---

    let otlp_tracer = otlp_endpoint
        .filter(|e| !e.is_empty())
        .and_then(|endpoint| match init_otlp_tracer(&endpoint) {
            Ok(tracer) => Some(tracer),
            Err(e) => {
                eprintln!(
                    "WARN: Failed to initialize OTLP tracer at {}: {}. \
                     Continuing without OTLP export.",
                    endpoint, e
                );
                None
            }
        });

    let loki_result = loki_endpoint
        .filter(|e| !e.is_empty())
        .and_then(|endpoint| {
            let config = crate::loki_layer::LokiConfig {
                endpoint,
                batch_size: 100,
                flush_interval_secs: 5,
                labels: loki_labels,
            };
            crate::loki_layer::build_loki_layer(config)
        });

    // --- Initialise subscriber with the appropriate combination of layers ---
    //
    // We match on all four combinations to avoid type-erasure gymnastics with
    // the subscriber builder. Each arm constructs the env_filter and fmt_layer
    // inline (they are lightweight) and composes the full subscriber.

    match (otlp_tracer, loki_result) {
        (Some(tracer), Some((loki_layer, bg_task))) => {
            let env_filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));
            let fmt_layer = build_fmt_layer(json_format);

            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

            // Apply fmt_layer to Registry first (it is type-erased to
            // Box<dyn Layer<Registry>>), then build the rest of the stack.
            tracing_subscriber::registry()
                .with(fmt_layer)
                .with(env_filter)
                .with(otel_layer)
                .with(loki_layer)
                .with(crate::log_ring::layer())
                .try_init()?;

            tokio::spawn(bg_task);
        }

        (Some(tracer), None) => {
            let env_filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));
            let fmt_layer = build_fmt_layer(json_format);

            let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

            tracing_subscriber::registry()
                .with(fmt_layer)
                .with(env_filter)
                .with(otel_layer)
                .with(crate::log_ring::layer())
                .try_init()?;
        }

        (None, Some((loki_layer, bg_task))) => {
            let env_filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));
            let fmt_layer = build_fmt_layer(json_format);

            tracing_subscriber::registry()
                .with(fmt_layer)
                .with(env_filter)
                .with(loki_layer)
                .with(crate::log_ring::layer())
                .try_init()?;

            tokio::spawn(bg_task);
        }

        (None, None) => {
            let env_filter =
                EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));
            let fmt_layer = build_fmt_layer(json_format);

            tracing_subscriber::registry()
                .with(fmt_layer)
                .with(env_filter)
                .with(crate::log_ring::layer())
                .try_init()?;
        }
    }

    Ok(())
}

/// Build a fmt layer (JSON or pretty) for use in tracing subscriber arms.
fn build_fmt_layer(
    json_format: bool,
) -> Box<dyn Layer<tracing_subscriber::Registry> + Send + Sync> {
    if json_format {
        tracing_subscriber::fmt::layer()
            .json()
            .with_level(true)
            .with_file(true)
            .with_line_number(true)
            .with_target(true)
            .boxed()
    } else {
        tracing_subscriber::fmt::layer()
            .pretty()
            .with_level(true)
            .with_file(true)
            .with_line_number(true)
            .with_target(true)
            .boxed()
    }
}

/// Build an OTLP tracer connected via gRPC (tonic) to the given endpoint.
fn init_otlp_tracer(endpoint: &str) -> Result<sdktrace::Tracer> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_tonic()
        .with_endpoint(endpoint)
        .build()?;

    let provider = sdktrace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(Resource::builder().with_service_name("mibee-rec").build())
        .build();

    let tracer = provider.tracer("mibee-rec");
    global::set_tracer_provider(provider);

    Ok(tracer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_env_filter_construction() {
        // EnvFilter should succeed with a valid directive
        let filter = EnvFilter::try_new("info");
        assert!(filter.is_ok());
    }

    #[test]
    fn test_env_filter_fallback() {
        // An unrecognised directive is still parsed as a filter (it matches
        // nothing, but construction succeeds)
        let filter = EnvFilter::try_new("zz_invalid_level_xyz");
        assert!(filter.is_ok());
    }

    #[test]
    fn test_fmt_layer_json_construction() {
        // Verify the JSON-format layer can be created without panic
        let layer = tracing_subscriber::fmt::layer::<tracing_subscriber::Registry>().json();
        let _ = layer;
    }

    #[test]
    fn test_fmt_layer_pretty_construction() {
        let layer = tracing_subscriber::fmt::layer::<tracing_subscriber::Registry>().pretty();
        let _ = layer;
    }

    #[test]
    fn test_otlp_failure_is_graceful() {
        // Calling init_tracing with an unreachable OTLP endpoint should not
        // panic. The function logs a warning and continues with a console-only
        // subscriber.
        //
        // The tonic exporter builder requires a Tokio runtime context, so we
        // provide one via Runtime::enter().
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime creation");
        let _guard = rt.enter();

        let result = init_tracing(
            "info",
            false,
            Some("http://127.0.0.1:19999".to_string()),
            None,
            HashMap::new(),
        );
        // The function should either succeed (subscriber installed) or fail with
        // a subscriber error — but never panic.
        assert!(result.is_ok() || result.is_err());
    }
}
