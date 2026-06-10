//! Stream control endpoints.
//!
//! All handlers require authentication (enforced by middleware).

use axum::Json;
use axum::extract::{Extension, Path};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use rusqlite::Connection;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::db;
use crate::routes::error_response;
use crate::server::ActiveStreams;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /api/cameras/{id}/start — begin capturing frames from a camera.
pub async fn start_stream(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(active): Extension<ActiveStreams>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify the camera exists.
    let conn = db.lock().await;
    let camera = match db::get_camera(&conn, &id) {
        Ok(Some(cam)) => cam,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "camera not found"),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera for start");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to start stream");
        }
    };
    drop(conn);

    // Check if already running.
    {
        let mut streams = active.0.lock().await;
        if streams.get(&id).copied().unwrap_or(false) {
            return error_response(StatusCode::CONFLICT, "stream already running");
        }
        streams.insert(id.clone(), true);
    }

    // Update camera status to "running".
    let conn = db.lock().await;
    let now = crate::routes::chrono_now();
    let updated = db::CameraRow {
        status: "running".to_string(),
        updated_at: now,
        ..camera
    };
    let _ = db::update_camera(&conn, &updated);
    drop(conn);

    tracing::info!(camera_id = %id, "stream started");
    (
        StatusCode::OK,
        Json(serde_json::json!({"status": "ok", "camera_id": id})),
    )
        .into_response()
}

/// POST /api/cameras/{id}/stop — stop capturing from a camera.
pub async fn stop_stream(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(active): Extension<ActiveStreams>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify the camera exists.
    let conn = db.lock().await;
    let camera = match db::get_camera(&conn, &id) {
        Ok(Some(cam)) => cam,
        Ok(None) => return error_response(StatusCode::NOT_FOUND, "camera not found"),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera for stop");
            return error_response(StatusCode::INTERNAL_SERVER_ERROR, "failed to stop stream");
        }
    };
    drop(conn);

    // Mark as stopped.
    {
        let mut streams = active.0.lock().await;
        streams.insert(id.clone(), false);
    }

    // Update camera status.
    let conn = db.lock().await;
    let now = crate::routes::chrono_now();
    let updated = db::CameraRow {
        status: "stopped".to_string(),
        updated_at: now,
        ..camera
    };
    let _ = db::update_camera(&conn, &updated);
    drop(conn);

    tracing::info!(camera_id = %id, "stream stopped");
    (
        StatusCode::OK,
        Json(serde_json::json!({"status": "ok", "camera_id": id})),
    )
        .into_response()
}

/// GET /api/cameras/{id}/snapshot — capture a single JPEG frame.
///
/// **Note**: Snapshot capture is not yet implemented at the pipeline level.
/// This endpoint returns 501 Not Implemented for now.
pub async fn snapshot(
    Extension(_db): Extension<Arc<Mutex<Connection>>>,
    Extension(_active): Extension<ActiveStreams>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(_id): Path<String>,
) -> impl IntoResponse {
    // TODO(T??): Implement actual frame capture from the stream pipeline.
    // For now, return a placeholder error indicating the feature is coming.
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(serde_json::json!({
            "error": "snapshot capture not yet implemented",
            "code": 501
        })),
    )
        .into_response()
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

    /// Helper: create a test app with a seeded camera and session token.
    async fn setup_with_camera() -> (crate::server::AppRouterState, String, String) {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute_batch(include_str!("../../../../migrations/001_initial.sql"))
            .unwrap();
        seed_user(&conn);
        let token = security::auth::create_session(&conn, "admin").unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let now = format!(
            "{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs()
        );

        conn.execute(
            "INSERT INTO cameras (id, name, camera_type, config, status, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                "Test Cam",
                "rtsp",
                "{\"url\":\"rtsp://localhost/stream\"}",
                "stopped",
                now,
                now,
            ],
        )
        .unwrap();

        let db = Arc::new(Mutex::new(conn));
        let active = crate::server::ActiveStreams::default();
        let state = crate::server::AppRouterState { db, active };
        (state, token, id)
    }

    #[tokio::test]
    async fn test_start_stream_success() {
        let (state, token, cam_id) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri(format!("/api/cameras/{}/start", cam_id))
            .method("POST")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_start_stream_nonexistent_camera() {
        let (state, token, _) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras/does-not-exist/start")
            .method("POST")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_start_stream_twice_returns_conflict() {
        let (state, token, cam_id) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        // First start succeeds.
        let req = Request::builder()
            .uri(format!("/api/cameras/{}/start", cam_id))
            .method("POST")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.clone().oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // Second start should conflict.
        let req = Request::builder()
            .uri(format!("/api/cameras/{}/start", cam_id))
            .method("POST")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_stop_stream_success() {
        let (state, token, cam_id) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        // Start first.
        let req = Request::builder()
            .uri(format!("/api/cameras/{}/start", cam_id))
            .method("POST")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let _ = app.clone().oneshot(req).await.unwrap();

        // Stop.
        let req = Request::builder()
            .uri(format!("/api/cameras/{}/stop", cam_id))
            .method("POST")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn test_stop_stream_nonexistent_camera() {
        let (state, token, _) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras/ghost/stop")
            .method("POST")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_snapshot_returns_not_implemented() {
        let (state, token, cam_id) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri(format!("/api/cameras/{}/snapshot", cam_id))
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_IMPLEMENTED);
    }

    #[tokio::test]
    async fn test_stream_routes_require_auth() {
        let conn = test_db();
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
        };
        let app = crate::server::build_app_with_state(state);

        let cam_id = "some-id";
        for path in &[
            format!("/api/cameras/{cam_id}/start"),
            format!("/api/cameras/{cam_id}/stop"),
            format!("/api/cameras/{cam_id}/snapshot"),
        ] {
            let req = Request::builder()
                .uri(path.as_str())
                .method("POST")
                .body(Body::empty())
                .unwrap();
            let res = app.clone().oneshot(req).await.unwrap();
            assert_eq!(
                res.status(),
                StatusCode::UNAUTHORIZED,
                "stream route {path} should require auth"
            );
        }
    }
}
