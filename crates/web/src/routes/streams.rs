//! Stream control endpoints.
//!
//! All handlers require authentication (enforced by middleware).

use axum::Json;
use axum::extract::{Extension, Path};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::IntoResponse;
use futures_core::Stream;
use protocols::rtsp_server::RtspServer;
use sqlx::SqlitePool;
use std::collections::HashMap;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::sync::oneshot;
use tokio::time::{Duration, timeout};
use uuid::Uuid;
use rusqlite::Connection;

use crate::db;
use crate::errors::ApiError;
use crate::stream_manager::StreamManager;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// POST /api/cameras/{id}/start — begin capturing frames from a camera.
#[tracing::instrument(skip_all, fields(camera_id = %id))]
pub async fn start_stream(
    Extension(db): Extension<SqlitePool>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(rtsp_srv): Extension<Arc<RtspServer>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify the camera exists.
    let camera = match db::get_camera(&db, &id).await {
        Ok(Some(cam)) => cam,
        Ok(None) => return ApiError::not_found("camera not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera for start");
            return ApiError::internal("failed to start stream").into_response();
        }
    };
    let camera_type = camera.camera_type.clone();
    let config = camera.config.clone();


    match stream_manager
        .create_stream(id.clone(), &camera_type, &config, Some(&rtsp_srv))
        .await
    {
        Ok(info) => {
            // Log stream session start (best-effort).
            let session_id = Uuid::new_v4().to_string();
            {
                let _ = db::insert_stream_session(&db, &session_id, &id).await;
            }

            // Update camera status to "running".
            let now = crate::routes::chrono_now();
            let updated = db::CameraRow {
                status: "running".to_string(),
                updated_at: now,
                ..camera
            };
            let _ = db::update_camera(&db, &updated).await;


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
#[tracing::instrument(skip_all, fields(camera_id = %id))]
pub async fn stop_stream(
    Extension(db): Extension<SqlitePool>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify the camera exists.
    let camera = match db::get_camera(&db, &id).await {
        Ok(Some(cam)) => cam,
        Ok(None) => return ApiError::not_found("camera not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera for stop");
            return ApiError::internal("failed to stop stream").into_response();
        }
    };


    match stream_manager.stop_stream(&id).await {
        Ok(_info) => {
            // Log stream session end (best-effort).
            // TODO: wire actual stats (bytes/frames/errors) from StreamManager when available.
            {
                let _ = db::finalize_stream_session(&db, &id, 0, 0, 0).await;
            }

            // Update DB status.
            let now = crate::routes::chrono_now();
            let updated = db::CameraRow {
                status: "stopped".to_string(),
                updated_at: now,
                ..camera
            };
            let _ = db::update_camera(&db, &updated).await;


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
                // Still try to finalize any open session (may be a no-op).
                let _ = db::finalize_stream_session(&db, &id, 0, 0, 0).await;
                let now = crate::routes::chrono_now();
                let updated = db::CameraRow {
                    status: "stopped".to_string(),
                    updated_at: now,
                    ..camera
                };
                let _ = db::update_camera(&db, &updated).await;

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
#[tracing::instrument(skip_all, fields(camera_id = %id))]
pub async fn snapshot(
    Extension(db): Extension<SqlitePool>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Extension(advertised_host): Extension<Arc<String>>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    // Verify camera exists
    let camera = match db::get_camera(&db, &id).await {
        Ok(Some(cam)) => cam,
        Ok(None) => return ApiError::not_found("camera not found").into_response(),
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera for snapshot");
            return ApiError::internal("database error").into_response();
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
            return ApiError::conflict("snapshot already in progress for this camera")
                .into_response();
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
    let rtsp_host = if advertised_host.is_empty() {
        "127.0.0.1"
    } else {
        advertised_host.as_str()
    };
    let rtsp_url = format!("rtsp://{}:8554/live/{}", rtsp_host, id);

    // Use ffmpeg to capture a single JPEG frame
    let capture_result = timeout(Duration::from_secs(30), async move {
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

        Ok::<Vec<u8>, anyhow::Error>(buffer)
    })
    .await;

    drop(_camera_lock);

    match capture_result {
        Ok(Ok(jpeg_data)) => {
            let mut headers = HeaderMap::new();
            headers.insert("content-type", HeaderValue::from_static("image/jpeg"));
            headers.insert(
                "content-length",
                jpeg_data
                    .len()
                    .to_string()
                    .parse::<HeaderValue>()
                    .unwrap_or(HeaderValue::from_static("0")),
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
// Live preview (MJPEG stream)
// ---------------------------------------------------------------------------

/// GET /api/cameras/{id}/live — MJPEG live preview stream.
///
/// Returns a `multipart/x-mixed-replace` response with JPEG frames at ~10 fps.
/// Suitable for direct use in `<img src=...>` — the browser handles the
/// multipart MJPEG decoding natively, no JavaScript or MSE required.
///
/// # Requirements
/// - Camera must exist and have an active stream (returns 409 otherwise).
/// - ffmpeg must be installed and on PATH.
/// - The RTSP server must be reachable at the advertised host.
///
/// # Notes
/// - Authentication is via the session cookie (sent automatically by `<img>`
///   on same-origin requests).
/// - The stream runs until the client disconnects (connection closes).
/// - Each client gets its own ffmpeg subprocess.
#[tracing::instrument(skip_all, fields(camera_id = %id))]
pub async fn live_preview(
    Extension(db): Extension<SqlitePool>,
    Extension(stream_manager): Extension<Arc<StreamManager>>,
    Extension(_user): Extension<AuthenticatedUser>,
    Extension(advertised_host): Extension<Arc<String>>,
    Path(id): Path<String>,
) -> axum::response::Response {
    use std::process::Stdio;
    use tokio_util::io::ReaderStream;

    // Verify camera exists.
    match db::get_camera(&db, &id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return ApiError::not_found("camera not found").into_response();
        }
        Err(e) => {
            tracing::error!(error = %e, camera_id = %id, "failed to get camera");
            return ApiError::internal("database error").into_response();
        }
    }

    // Verify stream is active.
    if !stream_manager.has_stream(&id).await {
        return ApiError::conflict("stream not active - start the stream first").into_response();
    }

    // Live preview: pull from RTSP and convert to MJPEG via ffmpeg.
    // This single ffmpeg approach works for ALL camera types (USB, RTSP, ONVIF,
    // GB28181) because every camera's stream is available through the RTSP server.
    let rtsp_host = if advertised_host.is_empty() {
        "127.0.0.1"
    } else {
        advertised_host.as_str()
    };
    let rtsp_url = format!("rtsp://{}:8554/live/{}", rtsp_host, id);

    let mut cmd = Command::new("ffmpeg");
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "fatal",
        "-fflags",
        "nobuffer",
        "-flags",
        "low_delay",
        "-analyzeduration",
        "500000",
        "-rtsp_transport",
        "tcp",
        "-i",
        &rtsp_url,
        "-s",
        "640x360",
        "-f",
        "mpjpeg",
        "-r",
        "10",
        "-q:v",
        "5",
        "-an",
        "pipe:1",
    ]);

    tracing::info!(camera_id = %id, rtsp_url = %rtsp_url, "starting MJPEG live preview");

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(error = %e, "failed to spawn ffmpeg");
            return ApiError::internal("ffmpeg spawn failed").into_response();
        }
    };

    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            let _ = child.kill().await;
            return ApiError::internal("ffmpeg stdout unavailable").into_response();
        }
    };

    // Oneshot channel to signal the reaper when client disconnects.
    // The reaper task waits for either:
    //   - kill_rx triggered: client disconnected, kill ffmpeg and reap
    //   - child.wait(): ffmpeg exited naturally (EPIPE on stdout write)
    // Without this, axum does not abort the body stream when the HTTP client
    // disconnects, so the pipe read end stays open and ffmpeg never receives
    // EPIPE, becoming a zombie process.
    let (kill_tx, kill_rx) = oneshot::channel::<()>();

    tokio::spawn(async move {
        tokio::select! {
            _ = kill_rx => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                tracing::debug!(camera_id = %id, "ffmpeg killed on client disconnect");
            }
            result = child.wait() => {
                tracing::debug!(camera_id = %id, status = ?result.ok(), "ffmpeg exited naturally");
            }
        }
    });

    // Stream wrapper that sends kill signal on drop.
    // When the HTTP response body is dropped (client disconnect),
    // kill_tx fires, and the reaper kills ffmpeg.
    //
    // Safety: FfmpegStream is Unpin because:
    //   Pin<Box<...>> is Unpin, Option<oneshot::Sender<()>> is Unpin.
    // So Pin::get_unchecked_mut is safe.
    struct FfmpegStream {
        inner: Pin<Box<ReaderStream<tokio::process::ChildStdout>>>,
        kill_tx: Option<oneshot::Sender<()>>,
    }

    impl Stream for FfmpegStream {
        type Item = Result<axum::body::Bytes, std::io::Error>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let this = unsafe { self.get_unchecked_mut() };
            this.inner.as_mut().poll_next(cx)
        }
    }

    impl Drop for FfmpegStream {
        fn drop(&mut self) {
            if let Some(tx) = self.kill_tx.take() {
                let _ = tx.send(());
            }
        }
    }

    let stream = FfmpegStream {
        inner: Box::pin(ReaderStream::new(stdout)),
        kill_tx: Some(kill_tx),
    };

    axum::response::Response::builder()
        .header("content-type", "multipart/x-mixed-replace; boundary=ffmpeg")
        .header("cache-control", "no-store, no-cache, must-revalidate")
        .header("pragma", "no-cache")
        .body(axum::body::Body::from_stream(stream))
        .unwrap_or_else(|e| {
            tracing::error!(error = ?e, "failed to build MJPEG response");
            ApiError::internal("response build failed").into_response()
        })
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
            advertised_host: Arc::new("localhost".to_string()),
        protocol_runtime: Arc::new(tokio::sync::Mutex::new(crate::protocol_runtime::ProtocolRuntime::new())),
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
            advertised_host: Arc::new("localhost".to_string()),
        protocol_runtime: Arc::new(tokio::sync::Mutex::new(crate::protocol_runtime::ProtocolRuntime::new())),
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
