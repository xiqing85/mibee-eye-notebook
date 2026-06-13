//! Stream control endpoints.
//!
//! All handlers require authentication (enforced by middleware).

use axum::Json;
use axum::extract::{Extension, Path};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use protocols::rtsp_server::RtspServer;
use rusqlite::Connection;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::{timeout, Duration};

use crate::db;
use crate::errors::ApiError;
use crate::stream_manager::StreamManager;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /api/cameras/{id}/start — begin capturing frames from a camera.
pub async fn start_stream(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(rtsp_srv): Extension<Arc<RtspServer>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify the camera exists.
    let conn = db.lock().await;
    let camera = match db::get_camera(&conn, &id) {
        Ok(Some(cam)) => cam,
        Ok(None) => return ApiError::not_found("camera not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera for start");
            return ApiError::internal("failed to start stream").into_response();
        }
    };
    let camera_type = camera.camera_type.clone();
    let config = camera.config.clone();
    drop(conn);

    match stream_manager
        .create_stream(id.clone(), &camera_type, &config, Some(&rtsp_srv))
        .await
    {
        Ok(info) => {
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

            tracing::info!(camera_id = %id, rtsp_url = ?info.rtsp_url, "stream started");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "status": "running",
                    "rtsp_url": info.rtsp_url,
                    "camera_id": id,
                })),
            )
                .into_response()
        }
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("already exists") {
                return ApiError::conflict("stream already running").into_response();
            }
            if msg.contains("exhausted") {
                return ApiError::internal("resource limit reached").into_response();
            }
            tracing::error!(error = %e, camera_id = %id, "failed to start stream");
            ApiError::internal("failed to start stream").into_response()
        }
    }
}

/// POST /api/cameras/{id}/stop — stop capturing from a camera.
pub async fn stop_stream(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify the camera exists.
    let conn = db.lock().await;
    let camera = match db::get_camera(&conn, &id) {
        Ok(Some(cam)) => cam,
        Ok(None) => return ApiError::not_found("camera not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera for stop");
            return ApiError::internal("failed to stop stream").into_response();
        }
    };
    drop(conn);

    match stream_manager.stop_stream(&id).await {
        Ok(_info) => {
            // Update DB status.
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
        Err(e) => {
            let msg = e.to_string();
            // Idempotent stop: if no active stream, still mark DB as stopped.
            if msg.contains("no active stream") {
                let conn = db.lock().await;
                let now = crate::routes::chrono_now();
                let updated = db::CameraRow {
                    status: "stopped".to_string(),
                    updated_at: now,
                    ..camera
                };
                let _ = db::update_camera(&conn, &updated);
                drop(conn);
                return (
                    StatusCode::OK,
                    Json(serde_json::json!({"status": "stopped", "camera_id": id})),
                )
                    .into_response();
            }
            tracing::error!(error = %e, camera_id = %id, "failed to stop stream");
            ApiError::internal("failed to stop stream").into_response()
        }
    }
}

/// Per-camera serialization lock to prevent concurrent ffmpeg for the same camera.
static SNAPSHOT_LOCKS: OnceLock<Arc<Mutex<HashMap<String, ()>>>> = OnceLock::new();

/// GET /api/cameras/{id}/snapshot — capture a single JPEG frame.
///
/// Captures a JPEG snapshot from an active stream by using ffmpeg to pull
/// a single frame from the RTSP stream and encode it as JPEG.
///
/// # Returns
///
/// - `200 OK` with JPEG body on success
/// - `404 Not Found` if camera does not exist
/// - `409 Conflict` if stream is not active
/// - `504 Gateway Timeout` if capture takes longer than 30 seconds
/// - `500 Internal Server Error` on ffmpeg or IO errors
pub async fn snapshot(
    Extension(db): Extension<Arc<Mutex<Connection>>>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify camera exists
    let camera = {
        let conn = db.lock().await;
        match db::get_camera(&conn, &id) {
            Ok(Some(cam)) => cam,
        Ok(None) => return ApiError::not_found("camera not found").into_response(),
            Err(e) => {
                tracing::error!(error = %e, camera_id = %id, "failed to get camera for snapshot");
            return ApiError::internal("database error").into_response();
            }
        }
    };
    drop(camera);

    // Check if stream is active
    if !stream_manager.has_stream(&id).await {
        return ApiError::conflict("stream not active - start the stream first").into_response();
    }

    // Acquire per-camera serialization lock
    let locks = SNAPSHOT_LOCKS.get_or_init(|| Arc::new(Mutex::new(HashMap::new())));
    let _camera_lock = {
        let mut lock_map = locks.lock().await;
        if lock_map.contains_key(&id) {
            drop(lock_map);
        return ApiError::conflict("snapshot already in progress for this camera").into_response();
        }
        lock_map.insert(id.clone(), ());
        // Create a guard that removes the lock on drop
        struct LockGuard {
            locks: Arc<Mutex<HashMap<String, ()>>>,
            camera_id: String,
        }
        impl Drop for LockGuard {
            fn drop(&mut self) {
                let locks = self.locks.clone();
                let camera_id = self.camera_id.clone();
                // Use spawn to avoid blocking in drop
                tokio::spawn(async move {
                    let mut lock_map = locks.lock().await;
                    lock_map.remove(&camera_id);
                });
            }
        }
        LockGuard {
            locks: locks.clone(),
            camera_id: id.clone(),
        }
    };

    // RTSP URL for the camera's stream
    let rtsp_url = format!("rtsp://localhost:8554/live/{}", id);

    // Use ffmpeg to capture a single JPEG frame
    let capture_result = timeout(
        Duration::from_secs(30),
        async move {
            let mut child = Command::new("ffmpeg")
                .arg("-rtsp_transport")
                .arg("tcp")
                .arg("-i")
                .arg(&rtsp_url)
                .arg("-vframes")
                .arg("1")
                .arg("-f")
                .arg("image2")
                .arg("-c:v")
                .arg("mjpeg")
                .arg("-q:v")
                .arg("2")
                .arg("pipe:1")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map_err(|e| anyhow::anyhow!("failed to spawn ffmpeg for snapshot: {}", e))?;

            let mut stdout = child
                .stdout
                .take()
                .ok_or_else(|| anyhow::anyhow!("failed to capture ffmpeg stdout"))?;

            // Read all output (JPEG data)
            let mut buffer = Vec::new();
            stdout
                .read_to_end(&mut buffer)
                .await
                .map_err(|e| anyhow::anyhow!("failed to read ffmpeg output: {}", e))?;

            // Wait for ffmpeg to complete
            let status = child
                .wait()
                .await
                .map_err(|e| anyhow::anyhow!("failed to wait for ffmpeg: {}", e))?;

            if !status.success() {
                anyhow::bail!("ffmpeg exited with non-zero status: {:?}", status);
            }

            // Verify JPEG magic bytes
            if buffer.len() < 3 || buffer[..2] != [0xFF, 0xD8] {
                anyhow::bail!("output is not a valid JPEG");
            }
            if buffer.len() < 3 || buffer[..2] != [0xFF, 0xD8] {
                anyhow::bail!("output is not a valid JPEG");
            }

            Ok::<Vec<u8>, anyhow::Error>(buffer)
        },
    )
    .await;

    drop(_camera_lock);

    match capture_result {
        Ok(Ok(jpeg_data)) => {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", "image/jpeg".parse().unwrap());
            headers.insert(
                "content-length",
                jpeg_data.len().to_string().parse().unwrap(),
            );
            (StatusCode::OK, headers, jpeg_data).into_response()
        }
        Ok(Err(e)) => {
            tracing::error!(error = %e, camera_id = %id, "snapshot capture failed");
            ApiError::internal("failed to capture snapshot").into_response()
        }
        Err(_) => {
            tracing::warn!(camera_id = %id, "snapshot capture timed out after 30s");
            ApiError::gateway_timeout("snapshot capture timed out").into_response()
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
    use protocols::rtsp_server::RtspServerConfig;
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
        let stream_manager = Arc::new(StreamManager::new());
        let rtsp_server = Arc::new(RtspServer::new(RtspServerConfig::default()));
        let state = crate::server::AppRouterState {
            db,
            active,
            stream_manager,
            rtsp_server,
            protocol_configs: Arc::new(Mutex::new(std::collections::HashMap::new())),
        };
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
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(res.into_body(), 1024 * 16)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(body["status"], "running");
        assert!(body["rtsp_url"].as_str().unwrap().contains("rtsp://"));
        assert_eq!(body["camera_id"], cam_id);
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
    async fn test_snapshot_nonexistent_camera() {
        let (state, token, _) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri("/api/cameras/ghost/snapshot")
            .method("GET")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_snapshot_inactive_stream() {
        let (state, token, cam_id) = setup_with_camera().await;
        let app = crate::server::build_app_with_state(state);

        let req = Request::builder()
            .uri(format!("/api/cameras/{}/snapshot", cam_id))
            .method("GET")
            .header("cookie", format!("session={token}"))
            .body(Body::empty())
            .unwrap();
        let res = app.oneshot(req).await.unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn test_stream_routes_require_auth() {
        let conn = test_db();
        let state = crate::server::AppRouterState {
            db: conn,
            active: crate::server::ActiveStreams::default(),
            stream_manager: Arc::new(StreamManager::new()),
            rtsp_server: Arc::new(RtspServer::new(RtspServerConfig::default())),
            protocol_configs: Arc::new(Mutex::new(std::collections::HashMap::new())),
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