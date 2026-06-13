use axum::middleware;
use axum::response::IntoResponse;
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
use crate::routes;
use crate::stream_manager::StreamManager;

// ---------------------------------------------------------------------------
// Shared state types
// ---------------------------------------------------------------------------

/// Tracks which camera streams are currently active.
#[derive(Clone, Default)]
pub struct ActiveStreams(pub Arc<Mutex<HashMap<String, bool>>>);

/// Application state passed to all routes via Extension.
pub struct AppRouterState {
    pub db: Arc<Mutex<Connection>>,
    pub active: ActiveStreams,
    pub stream_manager: Arc<StreamManager>,
    pub rtsp_server: Arc<RtspServer>,
    pub protocol_configs: Arc<Mutex<HashMap<String, serde_json::Value>>>,
}

// ---------------------------------------------------------------------------
// Static asset handler
// ---------------------------------------------------------------------------

async fn static_handler() -> impl IntoResponse {
    (
        [("content-type", "text/html; charset=utf-8")],
        assets::index_html(),
    )
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
    let active = state.active.clone();
    let stream_manager = state.stream_manager.clone();
    let rtsp_server = state.rtsp_server.clone();
    let protocol_configs = state.protocol_configs.clone();

    // -- Auth routes (public — these ARE the login/setup endpoints) --
    let login_route = Router::new()
        .route("/api/auth/login", post(routes::login_handler))
        .route_layer(middleware::from_fn(security::middleware::rate_limit));

    let auth_routes = Router::new()
        .merge(login_route)
        .route("/api/auth/setup", post(routes::setup_handler))
        .route("/api/auth/logout", post(routes::logout_handler));

    // -- Protected routes (require auth) --
    let protected_routes = Router::new()
        // Password reset
        .route("/api/auth/reset", post(routes::reset_password_handler))
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
        // Settings
        .route("/api/settings", get(routes::settings::get_settings))
        .route("/api/settings", put(routes::settings::update_settings))
        // Protocol configs
        .route(
            "/api/protocols/onvif",
            get(routes::protocols::get_protocols_onvif),
        )
        .route(
            "/api/protocols/onvif",
            put(routes::protocols::update_protocols_onvif),
        )
        .route(
            "/api/protocols/gb28181",
            get(routes::protocols::get_protocols_gb28181),
        )
        .route(
            "/api/protocols/gb28181",
            put(routes::protocols::update_protocols_gb28181),
        )
        .route(
            "/api/protocols/rtmp",
            get(routes::protocols::get_protocols_rtmp),
        )
        .route(
            "/api/protocols/rtmp",
            put(routes::protocols::update_protocols_rtmp),
        )
        // ONVIF
        // Device enumeration
        .route(
            "/api/devices/video",
            get(routes::devices::list_video_devices),
        )
        .route(
            "/api/devices/audio",
            get(routes::devices::list_audio_devices),
        )
        // Auth middleware
        .route_layer(middleware::from_fn(security::middleware::require_auth));

    // -- Build the full app --
    Router::new()
        // Public health/metrics endpoints
        .route("/health", get(routes::health_handler))
        .route("/metrics", get(routes::metrics_handler))
        // Auth routes (no auth required, but blocked before setup)
        .merge(auth_routes)
        // Protected routes (auth required, blocked before setup)
        .merge(protected_routes)
        // Static SPA fallback
        .route("/", get(static_handler))
        // Setup-required middleware — blocks non-allowed routes during first run
        .route_layer(middleware::from_fn(security::middleware::require_setup))
        // Extensions — must be OUTER layer so middleware can access db
        .layer(Extension(stream_manager))
        .layer(Extension(rtsp_server))
        .layer(Extension(active))
        .layer(Extension(db))
        .layer(Extension(protocol_configs))
        // CORS — permissive for localhost dev
        .layer(CorsLayer::permissive())
}

/// Convenience builder that creates a default `ActiveStreams`, `StreamManager`,
/// and `RtspServer`.
///
/// Provided for backward compatibility with existing tests.
pub fn build_app(db: Arc<Mutex<Connection>>) -> Router {
    build_app_with_state(AppRouterState {
        db,
        active: ActiveStreams::default(),
        stream_manager: Arc::new(StreamManager::new()),
        rtsp_server: Arc::new(RtspServer::new(RtspServerConfig::default())),
        protocol_configs: Arc::new(Mutex::new(HashMap::new())),
    })
}

/// Initialise the observability layer, build the TLS config, construct the app
/// router, and start serving.
///
/// This function blocks the current task until the server shuts down.
pub async fn run(
    host: &str,
    port: u16,
    db: Connection,
    stream_manager: Arc<StreamManager>,
    rtsp_server: Arc<RtspServer>,
    protocol_configs: Arc<Mutex<HashMap<String, serde_json::Value>>>,
) -> anyhow::Result<()> {
    // Register Prometheus metrics
    observability::register_metrics()?;

    let db = Arc::new(Mutex::new(db));
    let state = AppRouterState {
        db,
        active: ActiveStreams::default(),
        stream_manager,
        rtsp_server,
        protocol_configs,
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

    tracing::info!("notebook-cam server starting on https://{}", addr);

    axum_server::bind_rustls(addr, tls_config)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

/// Like [`run`] but accepts a shutdown signal for graceful termination.
///
/// When the `shutdown_rx` watch channel receives `true`, the server stops
/// accepting new connections, finishes in-flight requests, and returns.
pub async fn run_with_shutdown(
    host: &str,
    port: u16,
    db: Connection,
    stream_manager: Arc<StreamManager>,
    rtsp_server: Arc<RtspServer>,
    protocol_configs: Arc<Mutex<HashMap<String, serde_json::Value>>>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> anyhow::Result<()> {
    // Register Prometheus metrics
    observability::register_metrics()?;

    let db = Arc::new(Mutex::new(db));
    let state = AppRouterState {
        db,
        active: ActiveStreams::default(),
        stream_manager,
        rtsp_server,
        protocol_configs,
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

    tracing::info!("notebook-cam server starting on https://{}", addr);

    // Create a handle for graceful shutdown
    let handle = axum_server::Handle::new();
    let shutdown_handle_clone = handle.clone();

    // Spawn a task that watches for the shutdown signal
    let shutdown_handle = tokio::spawn(async move {
        let _ = shutdown_rx.changed().await;
        tracing::info!("Shutdown signal received, stopping server");
        shutdown_handle_clone.graceful_shutdown(None);
    });

    let result = axum_server::bind_rustls(addr, tls_config)
        .handle(handle)
        .serve(app.into_make_service())
        .await;

    shutdown_handle.abort();
    result?;

    Ok(())
}

// ---------------------------------------------------------------------------
// Test helpers (outside test module for cross-module access)
// ---------------------------------------------------------------------------

/// Create a DB with a pre-seeded admin user (setup already completed).
/// Accessible as `crate::server::test_db_with_user()`.
#[cfg(test)]
pub(crate) fn test_db_with_user() -> Arc<Mutex<Connection>> {
    let conn = Connection::open_in_memory().unwrap();
    let hash = security::password::hash_password("test_pass").unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE IF NOT EXISTS users (
            username TEXT PRIMARY KEY,
            password_hash TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now')),
            updated_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        INSERT INTO users (username, password_hash) VALUES ('admin', '{hash}');"
    ))
    .unwrap();
    Arc::new(Mutex::new(conn))
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

    fn test_db() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(Connection::open_in_memory().unwrap()))
    }

    #[tokio::test]
    async fn test_health_route_exists() {
        let app = build_app(test_db());
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
        let app = build_app(test_db());
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
        let app = build_app(test_db());
        let req = Request::builder().uri("/").body(Body::empty()).unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body = axum::body::to_bytes(res.into_body(), 1024 * 128)
            .await
            .unwrap();
        let body_str = String::from_utf8_lossy(&body);
        assert!(
            body_str.contains("notebook-cam"),
            "static HTML should contain project name"
        );
    }

    #[tokio::test]
    async fn test_unknown_route_returns_404() {
        let app = build_app(test_db_with_user());
        let req = Request::builder()
            .uri("/no-such-route")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_auth_routes_public() {
        let app = build_app(test_db());
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
        let app = build_app(test_db_with_user());
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
    async fn test_settings_routes_require_auth() {
        let app = build_app(test_db_with_user());
        let req = Request::builder()
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "settings routes should require auth"
        );
    }
}
