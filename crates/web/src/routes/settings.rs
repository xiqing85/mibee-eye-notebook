//! Settings endpoints.
//!
//! All handlers require authentication (enforced by middleware).

use axum::Json;
use axum::extract::Extension;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use serde::Deserialize;
use sqlx::SqlitePool;

use crate::db;
use crate::errors::ApiError;
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
#[tracing::instrument(skip_all)]
pub async fn get_settings(
    Extension(db): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    match db::list_settings(&db).await {
        Ok(rows) => {
            let map: std::collections::BTreeMap<String, String> = rows.into_iter().collect();
            match serde_json::to_value(map) {
                Ok(v) => (StatusCode::OK, Json(v)).into_response(),
                Err(e) => {
                    tracing::error!(error = %e, "failed to serialize settings");
                    ApiError::internal("failed to serialize settings").into_response()
                }
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list settings");
            ApiError::internal("failed to list settings").into_response()
        }
    }
}

/// PUT /api/settings — update one or more settings.
#[tracing::instrument(skip_all)]
pub async fn update_settings(
    Extension(db): Extension<SqlitePool>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(body): Json<UpdateSettingsRequest>,
) -> impl IntoResponse {
    for (key, value) in &body.settings {
        if let Err(e) = db::set_setting(&db, key, value).await {
            tracing::error!(error = %e, setting_key = %key, "failed to set setting");
            return ApiError::internal("failed to update settings").into_response();
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
    use rusqlite::Connection;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    /// Helper: create in-memory test DBs (pool + auth_db) with migrations and seeded user.
    async fn test_db() -> (SqlitePool, Arc<Mutex<Connection>>) {
        let (pool, auth_db) = crate::db::create_test_dbs().await;
        crate::db::run_migrations(&pool).await.unwrap();
        {
            let conn = auth_db.lock().await;
            conn.execute_batch(include_str!("../../../../migrations/001_initial.sql"))
                .unwrap();
            seed_user(&conn);
        }
        (pool, auth_db)
    }

    /// Helper: construct AppRouterState with default stream infrastructure.
    fn build_state(
        db: SqlitePool,
        auth_db: Arc<Mutex<Connection>>,
    ) -> crate::server::AppRouterState {
        crate::server::AppRouterState {
            db,
            auth_db,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
            protocol_configs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            advertised_host: Arc::new("localhost".to_string()),
            protocol_runtime: Arc::new(tokio::sync::Mutex::new(
                crate::protocol_runtime::ProtocolRuntime::new(),
            )),
        }
    }

    /// Seed the users table with an admin user for test setup.
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
        let (pool, auth_db) = test_db().await;
        let token = {
            let c = auth_db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let state = build_state(pool, auth_db);
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/settings")
            .header("cookie", format!("session={token}; csrf-token=test-csrf"))
            .header("x-csrf-token", "test-csrf")
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
        let (pool, auth_db) = test_db().await;
        let token = {
            let c = auth_db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let state = build_state(pool, auth_db);
        let app = crate::server::build_app_with_state(state);

        // Set two settings
        let req = Request::builder()
            .uri("/api/settings")
            .method("PUT")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}; csrf-token=test-csrf"))
            .header("x-csrf-token", "test-csrf")
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
            .header("cookie", format!("session={token}; csrf-token=test-csrf"))
            .header("x-csrf-token", "test-csrf")
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
            .header("cookie", format!("session={token}; csrf-token=test-csrf"))
            .header("x-csrf-token", "test-csrf")
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
            .header("cookie", format!("session={token}; csrf-token=test-csrf"))
            .header("x-csrf-token", "test-csrf")
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
        let (pool, auth_db) = test_db().await;
        let state = build_state(pool, auth_db);
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/settings")
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
