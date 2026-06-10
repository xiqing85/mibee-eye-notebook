//! Settings endpoints.
//!
//! All handlers require authentication (enforced by middleware).

use axum::Json;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use rusqlite::Connection;
use serde::Deserialize;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::db;
use crate::routes::error_response;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

/// Request body for PUT /api/settings.
#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    /// Map of setting key → new value.
    pub settings: std::collections::HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/settings — return all settings as a key-value object.
pub async fn get_settings(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    let conn = db.lock().await;
    match db::list_settings(&conn) {
        Ok(rows) => {
            let map: std::collections::BTreeMap<String, String> = rows.into_iter().collect();
            (StatusCode::OK, Json(serde_json::to_value(map).unwrap())).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list settings");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to list settings")
        }
    }
}

/// PUT /api/settings — update one or more settings.
pub async fn update_settings(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(body): Json<UpdateSettingsRequest>,
) -> impl IntoResponse {
    let conn = db.lock().await;
    for (key, value) in &body.settings {
        if let Err(e) = db::set_setting(&conn, key, value) {
            tracing::error!(error = %e, setting_key = %key, "failed to set setting");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed to update settings",
            );
        }
    }
    (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response()
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
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../../migrations/001_initial.sql"))
            .unwrap();
        seed_user(&conn);
        Arc::new(Mutex::new(conn))
    }

    fn seed_user(conn: &rusqlite::Connection) {
        security::auth::init_users_table(conn).unwrap();
        let hash = security::password::hash_password("test_pass").unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO users (username, password_hash) VALUES (?1, ?2)",
            rusqlite::params!["admin", hash],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn test_get_settings_empty() {
        let conn = test_db();
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
        };
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/settings")
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
        assert!(body.is_object());
        assert_eq!(body.as_object().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_update_settings() {
        let conn = test_db();
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
        };
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        // Set two settings
        let req = Request::builder()
            .uri("/api/settings")
            .method("PUT")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "settings": {
                        "theme": "dark",
                        "language": "zh-CN"
                    }
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Read them back
        let req = Request::builder()
            .uri("/api/settings")
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
        assert_eq!(body["theme"], "dark");
        assert_eq!(body["language"], "zh-CN");

        // Override one, keep the other
        let req = Request::builder()
            .uri("/api/settings")
            .method("PUT")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "settings": { "theme": "light" }
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        let req = Request::builder()
            .uri("/api/settings")
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
        assert_eq!(body["theme"], "light");
        assert_eq!(body["language"], "zh-CN");
    }

    #[tokio::test]
    async fn test_settings_requires_auth() {
        let conn = test_db();
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
