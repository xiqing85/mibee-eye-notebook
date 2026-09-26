use crate::errors::ApiError;
use axum::extract::DefaultBodyLimit;
use axum::http::{HeaderValue, Method, Request, header};
use axum::middleware;
use axum::middleware::Next;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::{delete, get, post, put};
use axum::{Extension, Router};
use axum_server::tls_rustls::RustlsConfig;
use protocols::rtsp_server::{RtspServer, RtspServerConfig};
use rusqlite::Connection;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc, watch};
use tower_http::cors::CorsLayer;

use crate::assets;
use crate::protocol_runtime::ProtocolRuntime;
use crate::routes;
use crate::stream_manager::StreamManager;
use opentelemetry::propagation::Extractor;
use tracing::Instrument;

// ---------------------------------------------------------------------------
// Shared state types
// ---------------------------------------------------------------------------

/// Tracks which camera streams are currently active.
#[derive(Clone, Default)]
pub struct ActiveStreams(pub Arc<Mutex<HashMap<String, bool>>>);

/// Application state passed to all routes via Extension.
pub struct AppRouterState {
    /// Async pool for web CRUD operations (cameras, settings, protocols)
    pub db: sqlx::SqlitePool,
    /// Blocking connection for security crate calls (auth, sessions)
    pub auth_db: Arc<Mutex<Connection>>,
    pub active: ActiveStreams,
    pub stream_manager: Arc<StreamManager>,
    pub rtsp_server: Arc<RtspServer>,
    pub protocol_configs: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    pub protocol_runtime: Arc<Mutex<ProtocolRuntime>>,
    pub event_tx: Arc<routes::events::EventBus>,
    pub advertised_host: Arc<String>,
    /// AI detection engine (inactive when disabled/unavailable — never None
    /// so capability and detections handlers can treat it uniformly).
    pub ai: Arc<streaming::ai::AiEngine>,
}

// ---------------------------------------------------------------------------
// Static asset handler
// ---------------------------------------------------------------------------
// Static asset handler
// ---------------------------------------------------------------------------

async fn static_handler() -> impl IntoResponse {
    (
        [("content-type", "text/html; charset=utf-8")],
        assets::index_html(),
    )
}

/// Serve an embedded static asset (`/style.css`, `/js/{path}`) from the
/// shared mibee-webui build.
async fn static_file_handler(
    axum::extract::Path(path): axum::extract::Path<String>,
) -> impl IntoResponse {
    let file = if let Some(content) = assets::get_file_content(&format!("js/{path}")) {
        content
    } else {
        return (
            axum::http::StatusCode::NOT_FOUND,
            [("content-type", "text/plain")],
            "not found",
        );
    };
    let mime = if path.ends_with(".css") {
        "text/css"
    } else if path.ends_with(".js") {
        "application/javascript; charset=utf-8"
    } else if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else {
        "application/octet-stream"
    };
    (axum::http::StatusCode::OK, [("content-type", mime)], file)
}

/// Serve the stylesheet (the route carries no path parameter).
async fn static_style_handler() -> impl IntoResponse {
    match assets::get_file_content("style.css") {
        Some(css) => (
            axum::http::StatusCode::OK,
            [("content-type", "text/css")],
            css,
        ),
        None => (
            axum::http::StatusCode::NOT_FOUND,
            [("content-type", "text/plain")],
            "not found",
        ),
    }
}

// ---------------------------------------------------------------------------
// Router builders
// ---------------------------------------------------------------------------

/// Build the complete Axum `Router` with explicit application state.
///
/// Route structure:
/// - `GET /health`                    — health check (public, allowed before setup)
/// - `GET /metrics`                   — Prometheus metrics (public, allowed before setup)
/// - `POST /api/auth/setup`           — first-run setup (public, allowed before setup)
/// - `POST /api/auth/login`           — login (rate-limited, blocked before setup)
/// - `POST /api/auth/logout`          — logout (blocked before setup)
/// - `POST /api/auth/reset`           — change password (auth required)
/// - `GET /api/cameras`               — list cameras (auth required)
/// - `POST /api/cameras`              — create camera (auth required)
/// - `GET /api/cameras/{id}`          — get camera (auth required)
/// - `PUT /api/cameras/{id}`          — update camera (auth required)
/// - `DELETE /api/cameras/{id}`       — delete camera (auth required)
/// - `POST /api/cameras/{id}/start`   — start stream (auth required)
/// - `POST /api/cameras/{id}/stop`    — stop stream (auth required)
/// - `GET /api/cameras/{id}/snapshot` — snapshot (auth required)
/// - `GET /api/settings`              — list settings (auth required)
/// - `PUT /api/settings`              — update settings (auth required)
/// - `GET /`                          — static SPA (public, allowed before setup)
pub fn build_app_with_state(state: AppRouterState) -> Router {
    let db = state.db.clone();
    let auth_db = state.auth_db.clone();
    let active = state.active.clone();
    let stream_manager = state.stream_manager.clone();
    let rtsp_server = state.rtsp_server.clone();
    let protocol_configs = state.protocol_configs.clone();
    let protocol_runtime = state.protocol_runtime.clone();
    let advertised_host = state.advertised_host.clone();
    let ai = state.ai.clone();

    // -- Auth routes (public, rate-limited) --
    // -- Auth routes (public, rate-limited, 10KB body limit) --
    let auth_routes = Router::new()
        .route("/api/auth/login", post(routes::login_handler))
        .route("/api/auth/setup", post(routes::setup_handler))
        .route("/api/auth/logout", post(routes::logout_handler))
        .route_layer(DefaultBodyLimit::max(10240)) // 10KB for auth
        .route_layer(middleware::from_fn(security::middleware::rate_limit));
    // Reset password gets a rate-limited sub-router
    let reset_route = Router::new()
        .route("/api/auth/reset", post(routes::reset_password_handler))
        .route_layer(middleware::from_fn(security::middleware::rate_limit));

    let protected_routes = Router::new()
        .merge(reset_route)
        // Session introspection (auth required)
        .route("/api/auth/me", get(routes::me_handler))
        // Cameras CRUD
        .route("/api/cameras", get(routes::cameras::list_cameras))
        .route("/api/cameras", post(routes::cameras::create_camera))
        .route("/api/cameras/{id}", get(routes::cameras::get_camera))
        .route("/api/cameras/{id}", put(routes::cameras::update_camera))
        .route("/api/cameras/{id}", delete(routes::cameras::delete_camera))
        // Stream control
        .route(
            "/api/cameras/{id}/start",
            post(routes::streams::start_stream),
        )
        .route("/api/cameras/{id}/stop", post(routes::streams::stop_stream))
        .route("/api/cameras/{id}/snapshot", get(routes::streams::snapshot))
        // Live preview (MJPEG stream for <img>)
        .route("/api/cameras/{id}/live", get(routes::streams::live_preview))
        // AI detections (SPEC v1 §4.6 + per-camera multi-camera dialect)
        .route("/api/detections", get(routes::detections::get_detections))
        .route(
            "/api/cameras/{id}/detections",
            get(routes::detections::get_camera_detections),
        )
        // Model registry: list / hot-switch / upload / delete (SPEC §4.6).
        .route("/api/ai/models", get(routes::ai_models::get_models))
        .route(
            "/api/ai/models/{id}/activate",
            post(routes::ai_models::activate_model),
        )
        .route(
            "/api/ai/models/{id}",
            post(routes::ai_models::upload_model)
                .delete(routes::ai_models::delete_model)
                // Axum's 2 MB default body cap rejects model files as
                // opaque parse errors; allow the SPEC §4.6 upload size.
                .layer(axum::extract::DefaultBodyLimit::max(
                    streaming::ai::registry::UPLOAD_MAX_BYTES + (1 << 20),
                )),
        )
        // MSE / fMP4 stream for <video> (H.264, hardware-decoded by browser)
        .route("/api/cameras/{id}/stream.mse", get(routes::mse::stream_mse))
        .route(
            "/api/cameras/{id}/stream.sub.mse",
            get(routes::mse::stream_sub_mse),
        )
        // WebRTC WHIP/WHEP signalling (sub-second latency; gated by [webrtc].enabled)
        .route("/api/webrtc/whep/{id}", post(routes::webrtc::whep))
        .route("/api/webrtc/whip/{id}", post(routes::webrtc::whip))
        // Unified config + status (SPEC v1 §3, §5) — replaces /api/settings
        // and the per-protocol GET/PUT endpoints (dialect A7).
        .route("/api/config", get(routes::config_api::get_config))
        .route("/api/config", put(routes::config_api::put_config))
        .route("/api/status", get(routes::config_api::status_handler))
        // Observability (SPEC v1 §3.2)
        .route("/api/metrics/summary", get(crate::observe::metrics_summary))
        .route("/api/logs", get(crate::observe::logs_handler))
        .route("/api/requests", get(crate::observe::requests_handler))
        // Protocol runtime status stays as a device extension (dialect A7).
        .route(
            "/api/protocols/runtime-status",
            get(routes::protocols::get_protocols_runtime_status),
        )
        // SSE events
        .route("/api/events", get(routes::events::sse_events))
        // Device enumeration
        // Device enumeration
        .route(
            "/api/devices/video",
            get(routes::devices::list_video_devices),
        )
        .route(
            "/api/devices/video/{index}/formats",
            get(routes::devices::list_video_device_formats),
        )
        .route(
            "/api/devices/audio",
            get(routes::devices::list_audio_devices),
        )
        // Hardware capability introspection
        .route(
            "/api/capabilities",
            get(routes::capabilities::get_capabilities),
        )
        // Auth middleware
        .route_layer(middleware::from_fn(security::middleware::require_auth));

    // -- Build the full app --
    Router::new()
        // Public health/metrics endpoints (SPEC §1: /api/health is the
        // canonical path; /health stays as an unwrapped legacy alias).
        .route("/health", get(routes::health_handler))
        .route("/api/health", get(routes::health_handler))
        .route("/metrics", get(routes::metrics_handler))
        // Auth routes (no auth required, but blocked before setup)
        .merge(auth_routes)
        // Protected routes (auth required, blocked before setup)
        .merge(protected_routes)
        // Static SPA (shared mibee-webui build, embedded via include_dir!)
        .route("/", get(static_handler))
        .route("/style.css", get(static_style_handler))
        .route("/js/{*path}", get(static_file_handler))
        // Setup-required middleware — blocks non-allowed routes during first run
        .route_layer(middleware::from_fn(security::middleware::require_setup))
        // Extensions — must be OUTER layer so middleware can access db
        .layer(Extension(stream_manager))
        .layer(Extension(rtsp_server))
        .layer(Extension(auth_db))
        .layer(Extension(active))
        .layer(Extension(db))
        .layer(Extension(protocol_configs))
        .layer(Extension(protocol_runtime))
        .layer(Extension(state.event_tx.clone()))
        .layer(Extension(advertised_host))
        .layer(Extension(ai))
        // CSP — strict Content-Security-Policy
        .layer(middleware::from_fn(csp_middleware))
        // HSTS
        .layer(middleware::from_fn(hsts_middleware))
        // CSRF — double-submit cookie verification
        .layer(middleware::from_fn(csrf_middleware))
        .layer(cors_layer())
        // W3C trace context — extract traceparent from incoming requests
        .layer(middleware::from_fn(trace_middleware))
        // HTTP Prometheus metrics
        .layer(middleware::from_fn(metrics_middleware))
        // Observability: request traces + traffic counters (SPEC v1 §3.2)
        .layer(middleware::from_fn(crate::observe::observe_middleware))
        // Body size limits
        .layer(DefaultBodyLimit::max(1024 * 1024)) // 1MB default
        // SPEC v1 §0 response envelope — wraps successful JSON /api responses
        .layer(middleware::from_fn(crate::envelope::envelope))
}
/// Convenience builder that creates a default `ActiveStreams`, `StreamManager`,
/// and `RtspServer`.
///
/// Provided for backward compatibility with existing tests.
pub fn build_app(db: sqlx::SqlitePool, auth_db: Arc<Mutex<Connection>>) -> Router {
    build_app_with_state(AppRouterState {
        db,
        auth_db,
        active: ActiveStreams::default(),
        stream_manager: Arc::new(StreamManager::new()),
        rtsp_server: Arc::new(RtspServer::new(RtspServerConfig::default())),
        protocol_configs: Arc::new(Mutex::new(HashMap::new())),
        protocol_runtime: Arc::new(Mutex::new(ProtocolRuntime::new())),
        event_tx: Arc::new(routes::events::new_event_bus()),
        advertised_host: Arc::new("localhost".to_string()),
        ai: Arc::new(streaming::ai::AiEngine::from_parts(
            streaming::ai::AiConfig::default(),
            None,
        )),
    })
}

/// Test helper: creates in-memory pool and auth_db, seeds with a test user,
/// and returns an app ready for testing.
#[cfg(test)]
pub async fn test_app_with_user() -> Router {
    let (pool, auth_db) = crate::db::create_test_dbs().await;
    crate::db::run_migrations(&pool)
        .await
        .expect("Failed to run migrations");
    // Seed a test user in auth_db
    let conn = auth_db.lock().await;
    let password_hash =
        security::password::hash_password("test_password").expect("Failed to hash password");
    conn.execute(
        "INSERT INTO users (username, password_hash) VALUES (?1, ?2)",
        rusqlite::params!["testuser", password_hash],
    )
    .expect("Failed to seed test user");
    drop(conn);

    crate::server::build_app_with_state(AppRouterState {
        db: pool,
        auth_db,
        active: ActiveStreams::default(),
        stream_manager: Arc::new(StreamManager::new()),
        rtsp_server: Arc::new(RtspServer::new(RtspServerConfig::default())),
        protocol_configs: Arc::new(Mutex::new(HashMap::new())),
        protocol_runtime: Arc::new(Mutex::new(ProtocolRuntime::new())),
        event_tx: Arc::new(routes::events::new_event_bus()),
        advertised_host: Arc::new("localhost".to_string()),
        ai: Arc::new(streaming::ai::AiEngine::from_parts(
            streaming::ai::AiConfig::default(),
            None,
        )),
    })
}

// ---------------------------------------------------------------------------
// HSTS middleware
// ---------------------------------------------------------------------------

/// Middleware that injects the `Strict-Transport-Security` header on all responses.
async fn hsts_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );
    response
}

/// Content-Security-Policy middleware. Inline scripts/styles allowed because
/// the SPA is self-contained in index.html; a future build step will
/// externalize and tighten to 'self' only.
async fn csp_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'self'; form-action 'self'"
        ),
    );
    response
}

/// CSRF double-submit cookie middleware.
/// For GET/HEAD/OPTIONS: exempt from CSRF check.
/// For POST/PUT/DELETE/PATCH: requires csrf-token cookie AND matching X-CSRF-Token header.
/// Missing cookie or mismatched header -> 403 Forbidden.
async fn csrf_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    use axum::http::Method;
    let method = request.method().clone();
    let path = request.uri().path();
    let is_state_changing = matches!(
        method,
        Method::POST | Method::PUT | Method::DELETE | Method::PATCH
    );
    // Auth endpoints are exempt from CSRF: login/setup issue the CSRF token
    let is_auth_endpoint =
        path == "/api/auth/login" || path == "/api/auth/setup" || path == "/api/auth/logout";
    if is_state_changing && !is_auth_endpoint {
        let cookie_csrf = request
            .headers()
            .get(header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| {
                s.split(';')
                    .map(|c| c.trim())
                    .find(|c| c.starts_with("csrf-token="))
                    .map(|c| c.trim_start_matches("csrf-token=").to_string())
            });
        match cookie_csrf {
            Some(ref expected) => {
                let header_csrf = request
                    .headers()
                    .get("x-csrf-token")
                    .and_then(|v| v.to_str().ok())
                    .map(|s| s.to_string());
                match header_csrf {
                    Some(ref actual) if actual == expected => {}
                    _ => {
                        tracing::warn!(method = %method, path = %request.uri().path(), "CSRF token mismatch — rejecting");
                        return ApiError::bad_request("CSRF token missing or invalid")
                            .into_response();
                    }
                }
            }
            None => {
                tracing::warn!(method = %method, path = %request.uri().path(), "CSRF cookie missing on state-changing request — rejecting");
                return ApiError::bad_request("CSRF token missing or invalid").into_response();
            }
        }
    }
    next.run(request).await
}

// ---------------------------------------------------------------------------
// CORS layer — same-origin by default, localhost origins in debug
// ---------------------------------------------------------------------------

/// Build the CORS layer.
/// In debug builds, localhost origins are allowed for development.
/// In release builds, no origins are allowed (same-origin only).
fn cors_layer() -> CorsLayer {
    if cfg!(debug_assertions) {
        CorsLayer::new()
            .allow_origin([
                "https://localhost".parse::<HeaderValue>().unwrap(),
                "https://127.0.0.1".parse::<HeaderValue>().unwrap(),
            ])
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::COOKIE])
            .allow_credentials(true)
    } else {
        CorsLayer::new()
            .allow_methods([
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::OPTIONS,
            ])
            .allow_headers([header::CONTENT_TYPE, header::AUTHORIZATION, header::COOKIE])
            .allow_credentials(true)
    }
}

// ---------------------------------------------------------------------------
// Trace context extraction middleware (W3C TraceContext)
// ---------------------------------------------------------------------------

/// Adapter to read HTTP headers as an OpenTelemetry `Extractor`.
struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|v| v.to_str().ok())
    }
    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|k| k.as_str()).collect()
    }
}

/// Extracts `traceparent` header from incoming requests and sets the
/// extracted OpenTelemetry context as the parent of the request span,
/// enabling distributed trace continuation from upstream callers.
async fn trace_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    use tracing_opentelemetry::OpenTelemetrySpanExt;

    let extracted = opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.extract_with_context(
            &opentelemetry::Context::current(),
            &HeaderExtractor(request.headers()),
        )
    });

    let span = tracing::info_span!("http_request",
        method = %request.method(),
        uri = %request.uri(),
    );
    let _ = span.set_parent(extracted);

    next.run(request).instrument(span).await
}

/// Middleware that increments the `mibee_http_requests_total` counter for
/// every incoming HTTP request using method, path, and response status.
async fn metrics_middleware(request: Request<axum::body::Body>, next: Next) -> Response {
    let method = request.method().clone();
    let path = request.uri().path().to_string();
    let response = next.run(request).await;
    let status = response.status().as_u16();
    observability::increment_http_requests(method.as_str(), &path, status);
    response
}

/// Initialise the observability layer, build the TLS config, construct the app
/// router, and start serving.
///
/// This function blocks the current task until the server shuts down.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    host: &str,
    port: u16,
    db: sqlx::SqlitePool,
    auth_db: Connection,
    stream_manager: Arc<StreamManager>,
    rtsp_server: Arc<RtspServer>,
    protocol_configs: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    protocol_runtime: Arc<Mutex<ProtocolRuntime>>,
    advertised_host: String,
    ai: Arc<streaming::ai::AiEngine>,
) -> anyhow::Result<()> {
    observability::register_metrics()?;

    let auth_db = Arc::new(Mutex::new(auth_db));
    let state = AppRouterState {
        db,
        auth_db,
        active: ActiveStreams::default(),
        stream_manager,
        rtsp_server,
        protocol_configs,
        protocol_runtime,
        event_tx: Arc::new(routes::events::new_event_bus()),
        advertised_host: Arc::new(advertised_host),
        ai,
    };
    let app = build_app_with_state(state);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

    // Build TLS config from cert/key files; generates self-signed if missing
    let tls_conf = security::tls::build_tls_config("tls/cert.pem", "tls/key.pem")?;
    let tls_config = RustlsConfig::from_config(Arc::new(tls_conf));

    // Clone so the reload task can update the shared ArcSwap inside RustlsConfig
    let tls_config_for_reload = tls_config.clone();

    // Set up certificate file watcher for hot-reload
    let (reload_tx, mut reload_rx) = mpsc::channel::<Arc<rustls::ServerConfig>>(8);
    tokio::spawn(security::tls::start_cert_watcher(
        "tls/cert.pem".to_string(),
        "tls/key.pem".to_string(),
        reload_tx,
        std::time::Duration::from_secs(2),
    ));

    // Apply TLS reloads as they arrive from the watcher
    tokio::spawn(async move {
        while let Some(new_config) = reload_rx.recv().await {
            tls_config_for_reload.reload_from_config(new_config);
            tracing::info!("TLS certs reloaded and applied to server");
        }
    });

    tracing::info!("mibee-eye server starting on https://{}", addr);

    axum_server::bind_rustls(addr, tls_config)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

/// Like [`run`] but accepts a shutdown signal for graceful termination.
///
/// When the `shutdown_rx` watch channel receives `true`, the server stops
/// accepting new connections, finishes in-flight requests, and returns.
#[allow(clippy::too_many_arguments)]
pub async fn run_with_shutdown(
    host: &str,
    port: u16,
    http_port: u16,
    db: sqlx::SqlitePool,
    auth_db: Arc<Mutex<Connection>>,
    stream_manager: Arc<StreamManager>,
    rtsp_server: Arc<RtspServer>,
    protocol_configs: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    protocol_runtime: Arc<Mutex<ProtocolRuntime>>,
    mut shutdown_rx: watch::Receiver<bool>,
    advertised_host: String,
    event_tx: Arc<routes::events::EventBus>,
    ai: Arc<streaming::ai::AiEngine>,
) -> anyhow::Result<()> {
    // Register Prometheus metrics
    observability::register_metrics()?;
    // Real-time resource sampler for /api/metrics/summary (SPEC v1 §3.2).
    crate::observe::spawn_sampler();

    let state = AppRouterState {
        db,
        auth_db,
        active: ActiveStreams::default(),
        stream_manager,
        rtsp_server,
        protocol_configs,
        protocol_runtime,
        event_tx,
        advertised_host: Arc::new(advertised_host),
        ai,
    };
    let app = build_app_with_state(state);

    let addr: SocketAddr = format!("{}:{}", host, port).parse()?;

    // Build TLS config from cert/key files; generates self-signed if missing
    let tls_conf = security::tls::build_tls_config("tls/cert.pem", "tls/key.pem")?;
    let tls_config = RustlsConfig::from_config(Arc::new(tls_conf));

    // Clone so the reload task can update the shared ArcSwap inside RustlsConfig
    let tls_config_for_reload = tls_config.clone();

    // Set up certificate file watcher for hot-reload
    let (reload_tx, mut reload_rx) = mpsc::channel::<Arc<rustls::ServerConfig>>(8);
    tokio::spawn(security::tls::start_cert_watcher(
        "tls/cert.pem".to_string(),
        "tls/key.pem".to_string(),
        reload_tx,
        std::time::Duration::from_secs(2),
    ));

    // Apply TLS reloads as they arrive from the watcher
    tokio::spawn(async move {
        while let Some(new_config) = reload_rx.recv().await {
            tls_config_for_reload.reload_from_config(new_config);
        }
    });

    tracing::info!("mibee-eye server starting on https://{}", addr);

    // Create a handle for graceful shutdown
    let handle = axum_server::Handle::new();
    let shutdown_handle_clone = handle.clone();

    // Spawn a task that watches for the shutdown signal
    let shutdown_handle = tokio::spawn(async move {
        let _ = shutdown_rx.changed().await;
        tracing::info!("Shutdown signal received, stopping server");
        shutdown_handle_clone.graceful_shutdown(None);
    });

    // Listener scheme tagging (SPEC appendix A): session cookies issued
    // over the optional plain-HTTP listener omit the `Secure` flag.
    let https_app = app.clone().layer(axum::middleware::from_fn(
        crate::routes::mark_secure_listener,
    ));
    let mut http_shutdown: Option<tokio::task::JoinHandle<()>> = None;
    if http_port > 0 {
        let http_addr: std::net::SocketAddr = format!("{host}:{http_port}")
            .parse()
            .map_err(|e| anyhow::anyhow!("invalid http listen address {host}:{http_port}: {e}"))?;
        let http_app = app.layer(axum::middleware::from_fn(
            crate::routes::mark_insecure_listener,
        ));
        let http_handle = handle.clone();
        tracing::info!(
            "mibee-eye additional plain-HTTP listener on http://{}",
            http_addr
        );
        http_shutdown = Some(tokio::spawn(async move {
            if let Err(e) = axum_server::bind(http_addr)
                .handle(http_handle)
                .serve(http_app.into_make_service())
                .await
            {
                tracing::error!(error = %e, "plain-HTTP listener terminated");
            }
        }));
    }

    let result = axum_server::bind_rustls(addr, tls_config)
        .handle(handle)
        .serve(https_app.into_make_service())
        .await;

    shutdown_handle.abort();
    if let Some(hs) = http_shutdown {
        hs.abort();
    }
    result?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Test helpers (outside test module for cross-module access)
// ---------------------------------------------------------------------------

/// Create a DB with a pre-seeded admin user (setup already completed).
/// Accessible as `crate::server::test_db_with_user()`.
#[cfg(test)]
pub(crate) async fn test_db_with_user() -> (sqlx::SqlitePool, Arc<Mutex<Connection>>) {
    let (pool, auth_db) = crate::db::create_test_dbs().await;
    crate::db::run_migrations(&pool)
        .await
        .expect("Failed to run migrations");
    // Seed admin user in auth_db
    let conn = auth_db.lock().await;
    let hash = security::password::hash_password("test_pass").unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO users (username, password_hash) VALUES ('admin', '{hash}');"
    ))
    .expect("Failed to seed admin user");
    drop(conn);
    (pool, auth_db)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    async fn test_db() -> (sqlx::SqlitePool, Arc<Mutex<Connection>>) {
        crate::db::create_test_dbs().await
    }

    /// Helper: build a test router with bare DBs (no migrations, no seed user).
    async fn test_router() -> Router {
        let (pool, auth_db) = test_db().await;
        build_app(pool, auth_db)
    }

    /// Helper: build a test router with migrated DBs and seeded admin user.
    async fn test_router_with_user() -> Router {
        let (pool, auth_db) = test_db_with_user().await;
        build_app(pool, auth_db)
    }

    #[tokio::test]
    async fn test_health_route_exists() {
        let app = test_router().await;
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_metrics_route_exists() {
        // register_metrics must be called before the handler runs.
        let _ = observability::register_metrics();
        let app = test_router().await;
        let req = Request::builder()
            .uri("/metrics")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        // Should not be 404
        assert_ne!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_static_asset_route_public() {
        let app = test_router().await;
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1024 * 128)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert!(
            body_str.contains("MiBee Cam"),
            "static HTML should contain project name"
        );
    }

    #[tokio::test]
    async fn test_unknown_route_returns_404() {
        let app = test_router_with_user().await;
        let req = Request::builder()
            .uri("/no-such-route")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_auth_routes_public() {
        let app = test_router().await;
        // Auth routes should NOT require auth (they ARE the login/setup)
        for path in &["/api/auth/login", "/api/auth/setup", "/api/auth/logout"] {
            let req = Request::builder()
                .uri(*path)
                .method("POST")
                .body(Body::empty())
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_ne!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "auth route {path} should not require authentication"
            );
        }
    }

    #[tokio::test]
    async fn test_camera_routes_require_auth() {
        let app = test_router_with_user().await;
        let req = Request::builder()
            .uri("/api/cameras")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "camera routes should require auth"
        );
    }

    #[tokio::test]
    async fn test_config_route_requires_auth() {
        let app = test_router_with_user().await;
        let req = Request::builder()
            .uri("/api/config")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "settings routes should require auth"
        );
    }
    #[tokio::test]
    async fn test_hsts_header_present() {
        let app = test_router().await;
        let req = Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        let hsts_value = res
            .headers()
            .get(axum::http::header::STRICT_TRANSPORT_SECURITY)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_owned());
        assert!(hsts_value.is_some(), "HSTS header should be present");
        assert_eq!(
            hsts_value.as_deref(),
            Some("max-age=31536000; includeSubDomains"),
            "HSTS value should match"
        );
    }

    #[tokio::test]
    async fn test_cors_not_permissive() {
        let app = test_router().await;
        let router_str = format!("{:?}", app);
        assert!(
            !router_str.contains("permissive"),
            "CORS layer should not be permissive"
        );
    }
}
