//! Protocol configuration endpoints.
//!
//! All handlers require authentication (enforced by middleware).
//! Configs are stored in an in-memory [`HashMap`] keyed by protocol name
//! (`onvif`, `gb28181`, `rtmp_push`), initialized from `config.toml` at startup.

use axum::Json;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

type ConfigStore = Arc<Mutex<HashMap<String, serde_json::Value>>>;

// ---------------------------------------------------------------------------
// ONVIF config
// ---------------------------------------------------------------------------

/// GET /api/protocols/onvif — return ONVIF device config as JSON.
pub async fn get_protocols_onvif(
    Extension(configs): Extension<ConfigStore>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    match get_config(&configs, "onvif").await {
        Some(val) => (StatusCode::OK, Json(val)).into_response(),
        None => ApiError::not_found("onvif config not found").into_response(),
    }
}

/// PUT /api/protocols/onvif — update ONVIF device config.
pub async fn update_protocols_onvif(
    Extension(configs): Extension<ConfigStore>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut store = configs.lock().await;
    match store.get_mut("onvif") {
        Some(existing) => {
            merge_json(existing, &payload);
            (StatusCode::OK, Json(existing.clone())).into_response()
        }
        None => ApiError::not_found("onvif config not found").into_response(),
    }
}

// ---------------------------------------------------------------------------
// GB28181 config
// ---------------------------------------------------------------------------

/// GET /api/protocols/gb28181 — return GB28181 device config as JSON.
pub async fn get_protocols_gb28181(
    Extension(configs): Extension<ConfigStore>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    match get_config(&configs, "gb28181").await {
        Some(val) => (StatusCode::OK, Json(val)).into_response(),
        None => ApiError::not_found("gb28181 config not found").into_response(),
    }
}

/// PUT /api/protocols/gb28181 — update GB28181 device config.
pub async fn update_protocols_gb28181(
    Extension(configs): Extension<ConfigStore>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut store = configs.lock().await;
    match store.get_mut("gb28181") {
        Some(existing) => {
            merge_json(existing, &payload);
            (StatusCode::OK, Json(existing.clone())).into_response()
        }
        None => ApiError::not_found("gb28181 config not found").into_response(),
    }
}

// ---------------------------------------------------------------------------
// RTMP push config
// ---------------------------------------------------------------------------

/// GET /api/protocols/rtmp — return RTMP push config as JSON.
pub async fn get_protocols_rtmp(
    Extension(configs): Extension<ConfigStore>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    match get_config(&configs, "rtmp_push").await {
        Some(val) => (StatusCode::OK, Json(val)).into_response(),
        None => ApiError::not_found("rtmp_push config not found").into_response(),
    }
}

/// PUT /api/protocols/rtmp — update RTMP push config.
pub async fn update_protocols_rtmp(
    Extension(configs): Extension<ConfigStore>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(payload): Json<serde_json::Value>,
) -> impl IntoResponse {
    let mut store = configs.lock().await;
    match store.get_mut("rtmp_push") {
        Some(existing) => {
            merge_json(existing, &payload);
            (StatusCode::OK, Json(existing.clone())).into_response()
        }
        None => ApiError::not_found("rtmp_push config not found").into_response(),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Retrieve a config value by key from the store.
async fn get_config(store: &ConfigStore, key: &str) -> Option<serde_json::Value> {
    let guard = store.lock().await;
    guard.get(key).cloned()
}

/// Merge `update` into `target` at the top level (shallow merge).
///
/// Fields present in `update` override those in `target`; fields in `target`
/// that are absent from `update` are preserved. Both values must be JSON
/// objects.
fn merge_json(target: &mut serde_json::Value, update: &serde_json::Value) {
    match (target, update) {
        (serde_json::Value::Object(t), serde_json::Value::Object(u)) => {
            for (k, v) in u {
                t.insert(k.clone(), v.clone());
            }
        }
        // If update is not an object, replace entirely
        (t, u) => *t = u.clone(),
    }
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

    fn test_config_store() -> ConfigStore {
        let mut map = HashMap::new();
        map.insert(
            "onvif".into(),
            serde_json::json!({
                "enabled": false,
                "device_name": "notebook-cam",
                "manufacturer": "MiBee",
                "model": "Rec-01",
                "serial": "NC00000001",
                "firmware_version": "1.0.0"
            }),
        );
        map.insert(
            "gb28181".into(),
            serde_json::json!({
                "enabled": false,
                "platform_sip_address": "192.168.1.100",
                "platform_sip_port": 5060,
                "device_id": "34020000002000000001",
                "username": "",
                "password": "",
                "sip_domain": "3402000000",
                "register_interval_secs": 60
            }),
        );
        map.insert(
            "rtmp_push".into(),
            serde_json::json!({
                "enabled": false,
                "push_url": "rtmp://192.168.1.100:1935/live",
                "app_name": "live",
                "stream_name": "stream1",
                "reconnect_interval_secs": 5,
                "max_reconnect_attempts": 10
            }),
        );
        Arc::new(Mutex::new(map))
    }

    /// Build an AppRouterState seeded with a user + protocol configs.
    fn test_state() -> crate::server::AppRouterState {
        crate::server::AppRouterState {
            db: crate::server::test_db_with_user(),
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
            protocol_configs: test_config_store(),
        }
    }

    // -----------------------------------------------------------------------
    // ONVIF
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_onvif_config() {
        let state = test_state();
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/protocols/onvif")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["enabled"], false);
        assert_eq!(body["device_name"], "notebook-cam");
    }

    #[tokio::test]
    async fn test_update_onvif_config() {
        let state = test_state();
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/protocols/onvif")
            .method("PUT")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "device_name": "custom-cam",
                    "enabled": true
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Read back
        let req = Request::builder()
            .uri("/api/protocols/onvif")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["device_name"], "custom-cam");
        assert_eq!(body["enabled"], true);
        assert_eq!(body["manufacturer"], "MiBee"); // preserved unchanged
    }

    // -----------------------------------------------------------------------
    // GB28181
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_gb28181_config() {
        let state = test_state();
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/protocols/gb28181")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["device_id"], "34020000002000000001");
        assert_eq!(body["platform_sip_port"], 5060);
    }

    #[tokio::test]
    async fn test_update_gb28181_config() {
        let state = test_state();
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/protocols/gb28181")
            .method("PUT")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "platform_sip_address": "10.0.0.1",
                    "register_interval_secs": 120
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/protocols/gb28181")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["platform_sip_address"], "10.0.0.1");
        assert_eq!(body["register_interval_secs"], 120);
        assert_eq!(body["device_id"], "34020000002000000001"); // preserved
    }

    // -----------------------------------------------------------------------
    // RTMP push
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_get_rtmp_config() {
        let state = test_state();
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/protocols/rtmp")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["push_url"], "rtmp://192.168.1.100:1935/live");
        assert!(!body["enabled"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_update_rtmp_config() {
        let state = test_state();
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/protocols/rtmp")
            .method("PUT")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "enabled": true,
                    "push_url": "rtmp://example.com:1935/live"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/protocols/rtmp")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["enabled"], true);
        assert_eq!(body["push_url"], "rtmp://example.com:1935/live");
        assert_eq!(body["app_name"], "live"); // preserved
    }

    // -----------------------------------------------------------------------
    // Auth
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn test_protocols_routes_require_auth() {
        let state = test_state();
        let app = crate::server::build_app_with_state(state);

        for path in &[
            "/api/protocols/onvif",
            "/api/protocols/gb28181",
            "/api/protocols/rtmp",
        ] {
            let req = Request::builder().uri(*path).body(Body::empty()).unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "{path} should require auth"
            );
        }
    }
}
