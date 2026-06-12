//! Camera CRUD endpoints.
//!
//! All handlers require authentication (enforced by middleware).

use axum::Json;
use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::db::{self, CameraRow};
use crate::routes::error_response;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

/// Request body for POST /api/cameras.
#[derive(Debug, Deserialize)]
pub struct CreateCameraRequest {
    pub name: String,
    pub camera_type: String,
    #[serde(default = "default_config")]
    pub config: serde_json::Value,
}

fn default_config() -> serde_json::Value {
    serde_json::Value::Object(Default::default())
}

/// Request body for PUT /api/cameras/{id}.
#[derive(Debug, Deserialize)]
pub struct UpdateCameraRequest {
    pub name: Option<String>,
    pub camera_type: Option<String>,
    pub config: Option<serde_json::Value>,
    pub status: Option<String>,
}

/// Response body for a single camera.
#[derive(Debug, Serialize)]
pub struct CameraResponse {
    pub id: String,
    pub name: String,
    pub camera_type: String,
    pub config: serde_json::Value,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

impl From<CameraRow> for CameraResponse {
    fn from(row: CameraRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            camera_type: row.camera_type,
            config: row.config,
            status: row.status,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /api/cameras — list all cameras.
pub async fn list_cameras(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> impl IntoResponse {
    let conn = db.lock().await;
    match db::list_cameras(&conn) {
        Ok(rows) => {
            let cameras: Vec<CameraResponse> = rows.into_iter().map(CameraResponse::from).collect();
            (StatusCode::OK, Json(serde_json::to_value(cameras).unwrap())).into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "failed to list cameras");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to list cameras")
        }
    }
}

/// GET /api/cameras/{id} — get a single camera.
pub async fn get_camera(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let conn = db.lock().await;
    match db::get_camera(&conn, &id) {
        Ok(Some(row)) => (
            StatusCode::OK,
            Json(serde_json::to_value(CameraResponse::from(row)).unwrap()),
        )
            .into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "camera not found"),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to get camera")
        }
    }
}

/// POST /api/cameras — create a new camera.
pub async fn create_camera(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Json(body): Json<CreateCameraRequest>,
) -> impl IntoResponse {
    let id = uuid::Uuid::new_v4().to_string();
    let now = crate::routes::chrono_now();

    let row = CameraRow {
        id: id.clone(),
        name: body.name,
        camera_type: body.camera_type,
        config: body.config,
        status: "stopped".to_string(),
        created_at: now.clone(),
        updated_at: now,
    };

    let conn = db.lock().await;
    match db::create_camera(&conn, &row) {
        Ok(()) => (
            StatusCode::CREATED,
            Json(serde_json::to_value(CameraResponse::from(row)).unwrap()),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "failed to create camera");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to create camera")
        }
    }
}

/// PUT /api/cameras/{id} — update an existing camera.
pub async fn update_camera(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
    Json(body): Json<UpdateCameraRequest>,
) -> impl IntoResponse {
    let conn = db.lock().await;

    // Fetch existing camera, or return 404.
    let existing = match db::get_camera(&conn, &id) {
        Ok(Some(row)) => row,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "camera not found"),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to fetch camera for update");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to update camera");
        }
    };

    let now = crate::routes::chrono_now();
    let updated = CameraRow {
        id: existing.id,
        name: body.name.unwrap_or(existing.name),
        camera_type: body.camera_type.unwrap_or(existing.camera_type),
        config: body.config.unwrap_or(existing.config),
        status: body.status.unwrap_or(existing.status),
        created_at: existing.created_at,
        updated_at: now,
    };

    match db::update_camera(&conn, &updated) {
        Ok(()) => (
            StatusCode::OK,
            Json(serde_json::to_value(CameraResponse::from(updated)).unwrap()),
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to update camera");
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to update camera")
        }
    }
}

/// DELETE /api/cameras/{id} — delete a camera.
pub async fn delete_camera(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let conn = db.lock().await;
    match db::delete_camera(&conn, &id) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"status": "ok"}))).into_response(),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") {
                error_response(StatusCode::NOT_FOUND, "camera not found")
            } else {
                tracing::error!(error = %e, camera_id = %id, "failed to delete camera");
                error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to delete camera")
            }
        }
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

    /// Helper: create an in-memory test DB with migration and a seeded user.
    fn test_db() -> Arc<Mutex<Connection>> {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../../migrations/001_initial.sql"))
            .unwrap();
        seed_user(&conn);
        Arc::new(Mutex::new(conn))
    }

    /// Helper: build a test app with a seeded user and session token.
    fn test_app_with_token() -> (crate::server::AppRouterState, String) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../../migrations/001_initial.sql"))
            .unwrap();
        seed_user(&conn);
        let token = security::auth::create_session(&conn, "admin").unwrap();
        let db = Arc::new(Mutex::new(conn));
        let active = crate::server::ActiveStreams::default();
        let stream_manager = Arc::new(crate::stream_manager::StreamManager::new());
        let rtsp_server = Arc::new(protocols::rtsp_server::RtspServer::new(
            protocols::rtsp_server::RtspServerConfig::default(),
        ));
        let state = crate::server::AppRouterState {
            db,
            active,
            stream_manager,
            rtsp_server,
            protocol_configs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };
        (state, token)
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
    async fn test_list_cameras_empty() {
        let conn = test_db();
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
            protocol_configs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };
        let token = {
            let c = state.db.lock().await;
            security::auth::create_session(&c, "admin").unwrap()
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras")
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
        assert_eq!(body.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_create_camera() {
        let (state, token) = test_app_with_token();
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras")
            .method("POST")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "name": "Front Door",
                    "camera_type": "rtsp",
                    "config": {"url": "rtsp://192.168.1.100:554/stream1"}
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["name"], "Front Door");
        assert_eq!(body["camera_type"], "rtsp");
        assert_eq!(body["status"], "stopped");
        assert!(!body["id"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_camera_crud_full_cycle() {
        let (state, token) = test_app_with_token();
        let app = crate::server::build_app_with_state(state);

        // Create
        let req = Request::builder()
            .uri("/api/cameras")
            .method("POST")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "name": "Driveway",
                    "camera_type": "onvif",
                    "config": {"host": "192.168.1.200"}
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let created: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        let cam_id = created["id"].as_str().unwrap().to_string();

        // Get by ID
        let req = Request::builder()
            .uri(format!("/api/cameras/{}", cam_id))
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Update
        let req = Request::builder()
            .uri(format!("/api/cameras/{}", cam_id))
            .method("PUT")
            .header("content-type", "application/json")
            .header("cookie", format!("session={token}"))
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "name": "Back Driveway",
                    "status": "running"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let updated: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(updated["name"], "Back Driveway");
        assert_eq!(updated["status"], "running");

        // List should now have one camera
        let req = Request::builder()
            .uri("/api/cameras")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let list: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 1);

        // Delete
        let req = Request::builder()
            .uri(format!("/api/cameras/{}", cam_id))
            .method("DELETE")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // List empty
        let req = Request::builder()
            .uri("/api/cameras")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let list: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(list.as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_get_nonexistent_camera_returns_404() {
        let (state, token) = test_app_with_token();
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras/nonexistent-id")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_nonexistent_camera_returns_404() {
        let (state, token) = test_app_with_token();
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras/ghost")
            .method("DELETE")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_camera_requires_auth() {
        let conn = test_db();
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(crate::stream_manager::StreamManager::new()),
            rtsp_server: Arc::new(protocols::rtsp_server::RtspServer::new(
                protocols::rtsp_server::RtspServerConfig::default(),
            )),
            protocol_configs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras")
            .method("POST")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::to_vec(&serde_json::json!({
                    "name": "Unauth",
                    "camera_type": "rtsp"
                }))
                .unwrap(),
            ))
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
