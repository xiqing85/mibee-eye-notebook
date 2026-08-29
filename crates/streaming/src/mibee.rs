//! MiBee NVR REST API client.
//!
//! Provides a typed HTTP client for the MiBee NVR API, supporting camera CRUD,
//! stream URL retrieval, camera list synchronization, and event subscription
//! via Server-Sent Events.
//!
//! # Design
//!
//! - [`MiBeeClient`] is the main entry point — holds a [`reqwest::Client`] and
//!   the target NVR base URL.
//! - All public methods are `async` and return [`anyhow::Result`].
//! - Authentication is HTTP Basic Auth (base64-encoded username:password), as
//!   used by MiBee NVR's bcrypt-based auth model.
//! - Stream URLs are constructed from the NVR host address (extracted from
//!   `base_url`) rather than queried via API.
//!
//! # Example
//!
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use streaming::mibee::MiBeeClient;
//!
//! let client = MiBeeClient::new("http://192.168.1.100:8080", "admin", "password");
//! let cameras = client.list_cameras().await?;
//! println!("Found {} MiBee cameras", cameras.len());
//! # Ok(())
//! # }
//! ```

use anyhow::{Context, Result};
use opentelemetry::propagation::Injector;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
// ── Re-exports ──────────────────────────────────────────────────────────────────

pub use reqwest::header::HeaderValue;

// ── Type aliases ────────────────────────────────────────────────────────────────

/// Identifier for a camera in the local configuration.
pub type CameraId = String;

// ── Data types ──────────────────────────────────────────────────────────────────

/// A camera registered on the MiBee NVR.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MiBeeCamera {
    /// Unique camera identifier.
    pub id: String,
    /// Human-readable camera name.
    pub name: String,
    /// Source URL (e.g. RTSP URL of the camera).
    pub source: String,
    /// Whether the camera is currently enabled.
    pub enabled: bool,
    /// Stream type (e.g. "rtsp", "onvif").
    #[serde(rename = "streamType")]
    pub stream_type: String,
    /// Creation timestamp.
    #[serde(rename = "createdAt")]
    pub created_at: Option<String>,
    /// Last update timestamp.
    #[serde(rename = "updatedAt")]
    pub updated_at: Option<String>,
}

/// Request payload for creating a new camera on the MiBee NVR.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateCameraRequest {
    /// Camera name.
    pub name: String,
    /// Source URL (e.g. `rtsp://user:pass@192.168.1.100:554/stream1`).
    pub source: String,
    /// Stream protocol type (defaults to "rtsp" if not set).
    #[serde(rename = "streamType")]
    pub stream_type: Option<String>,
    /// Whether the camera is enabled on creation (defaults to `true`).
    pub enabled: Option<bool>,
}

/// Request payload for updating an existing camera.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCameraRequest {
    /// New camera name.
    pub name: Option<String>,
    /// New source URL.
    pub source: Option<String>,
    /// Whether the camera is enabled.
    pub enabled: Option<bool>,
}

/// Result of a camera sync operation between local config and MiBee NVR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncResult {
    /// Camera IDs that exist on MiBee but not locally (candidates for addition).
    pub added: Vec<String>,
    /// Camera IDs that exist locally but not on MiBee (candidates for removal).
    pub removed: Vec<String>,
    /// Camera IDs that exist in both places (candidates for update).
    pub updated: Vec<String>,
    /// Error messages encountered during sync.
    pub errors: Vec<String>,
}

/// An event received from MiBee NVR's SSE endpoint (`/api/events`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CameraEvent {
    /// Event type (e.g. "camera_added", "camera_removed", "motion_detected").
    ///
    /// Absent from SSE JSON data (it comes from the "event:" line) so we
    /// default to "unknown" for deserialization; `handle_sse_line` overwrites
    /// it with the actual event type after parsing.
    #[serde(rename = "type", default = "default_event_type")]
    pub event_type: String,
    /// The camera this event pertains to.
    #[serde(rename = "cameraId")]
    pub camera_id: String,
    /// Unix timestamp (milliseconds) when the event occurred.
    pub timestamp: u64,
    /// Optional event-specific payload.
    pub data: Option<serde_json::Value>,
}

fn default_event_type() -> String {
    "unknown".to_string()
}

// ── MiBeeClient ─────────────────────────────────────────────────────────────────

/// A typed HTTP client for the MiBee NVR REST API.
///
/// Manages authentication, request/response serialization, and URL construction.
/// All methods are `async` and produce structured error messages via `anyhow`.
#[derive(Debug, Clone)]
pub struct MiBeeClient {
    /// Inner `reqwest::Client` (cheap to clone — uses `Arc` internally).
    client: Client,
    /// Base URL of the MiBee NVR (e.g. `http://192.168.1.100:8080`).
    base_url: String,
    /// Username for HTTP Basic Auth.
    username: String,
    /// Password for HTTP Basic Auth.
    password: String,
}

impl MiBeeClient {
    /// Create a new `MiBeeClient` targeting the given MiBee NVR instance.
    ///
    /// `base_url` is the HTTP endpoint of the NVR web UI (e.g.
    /// `http://192.168.1.100:8080`). Trailing slashes are stripped.
    ///
    /// `username` and `password` are the credentials used for HTTP Basic Auth.
    pub fn new(base_url: &str, username: &str, password: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest::Client::builder() should never fail with default settings");

        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    // ── Private helpers ──────────────────────────────────────────────────────

    /// Build the full URL for a given API path.
    fn build_url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Extract the hostname from the base URL for constructing protocol URLs.
    fn extract_host(&self) -> &str {
        self.base_url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .split(':')
            .next()
            .unwrap_or("localhost")
    }

    /// Perform an authenticated GET and deserialize the JSON response.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = self.build_url(path);
        let mut request = self
            .client
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .build()
            .context("failed to build GET request")?;
        inject_trace_context(request.headers_mut());
        let response = self
            .client
            .execute(request)
            .await
            .with_context(|| format!("GET {url} failed — is the MiBee NVR reachable?"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("GET {url} returned {status}: {body}");
        }

        response
            .json()
            .await
            .with_context(|| format!("GET {url} returned invalid JSON"))
    }

    /// Perform an authenticated POST with a JSON body and deserialize the response.
    async fn post_json<T: serde::de::DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = self.build_url(path);
        let mut request = self
            .client
            .post(&url)
            .basic_auth(&self.username, Some(&self.password))
            .json(body)
            .build()
            .context("failed to build POST request")?;
        inject_trace_context(request.headers_mut());
        let response = self
            .client
            .execute(request)
            .await
            .with_context(|| format!("POST {url} failed"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("POST {url} returned {status}: {body}");
        }

        response
            .json()
            .await
            .with_context(|| format!("POST {url} returned invalid JSON"))
    }

    /// Perform an authenticated PUT with a JSON body and deserialize the response.
    async fn put_json<T: serde::de::DeserializeOwned, B: Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T> {
        let url = self.build_url(path);
        let mut request = self
            .client
            .put(&url)
            .basic_auth(&self.username, Some(&self.password))
            .json(body)
            .build()
            .context("failed to build PUT request")?;
        inject_trace_context(request.headers_mut());
        let response = self
            .client
            .execute(request)
            .await
            .with_context(|| format!("PUT {url} failed"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("PUT {url} returned {status}: {body}");
        }

        response
            .json()
            .await
            .with_context(|| format!("PUT {url} returned invalid JSON"))
    }

    /// Perform an authenticated DELETE.
    async fn delete_request(&self, path: &str) -> Result<()> {
        let url = self.build_url(path);
        let mut request = self
            .client
            .delete(&url)
            .basic_auth(&self.username, Some(&self.password))
            .build()
            .context("failed to build DELETE request")?;
        inject_trace_context(request.headers_mut());
        let response = self
            .client
            .execute(request)
            .await
            .with_context(|| format!("DELETE {url} failed"))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("DELETE {url} returned {status}: {body}");
        }

        Ok(())
    }

    // ── Camera CRUD ──────────────────────────────────────────────────────────

    /// List all cameras registered on the MiBee NVR.
    ///
    /// `GET /api/cameras`
    pub async fn list_cameras(&self) -> Result<Vec<MiBeeCamera>> {
        self.get_json("/api/cameras").await
    }

    /// Get a single camera by its ID.
    ///
    /// `GET /api/cameras/{id}`
    pub async fn get_camera(&self, id: &str) -> Result<MiBeeCamera> {
        self.get_json(&format!("/api/cameras/{}", id)).await
    }

    /// Register a new camera on the MiBee NVR.
    ///
    /// `POST /api/cameras`
    pub async fn create_camera(&self, camera: &CreateCameraRequest) -> Result<MiBeeCamera> {
        self.post_json("/api/cameras", camera).await
    }

    /// Update an existing camera's configuration.
    ///
    /// `PUT /api/cameras/{id}`
    pub async fn update_camera(
        &self,
        id: &str,
        camera: &UpdateCameraRequest,
    ) -> Result<MiBeeCamera> {
        self.put_json(&format!("/api/cameras/{}", id), camera).await
    }

    /// Delete a camera from the MiBee NVR.
    ///
    /// `DELETE /api/cameras/{id}`
    pub async fn delete_camera(&self, id: &str) -> Result<()> {
        self.delete_request(&format!("/api/cameras/{}", id)).await
    }

    // ── Stream operations ────────────────────────────────────────────────────

    /// Get the stream URL for a camera using the specified protocol.
    ///
    /// Supported protocols:
    /// - `"rtsp"` — returns an `rtsp://` URL for RTSP consumption
    /// - `"hls"` — returns an `http://` URL for HLS playback
    /// - `"ws"` or `"websocket"` — returns a `ws://` URL for WebSocket streaming
    ///
    /// The URL is constructed from the MiBee NVR host address, not retrieved via API.
    pub fn get_stream_url(&self, camera_id: &str, protocol: &str) -> Result<String> {
        let host = self.extract_host();
        match protocol {
            "rtsp" => Ok(format!("rtsp://{host}:8554/live/{camera_id}")),
            "hls" => Ok(format!("http://{host}:8080/hls/{camera_id}/index.m3u8")),
            "ws" | "websocket" => Ok(format!("ws://{host}:8080/ws/{camera_id}")),
            other => {
                anyhow::bail!("unsupported stream protocol: {other} (expected rtsp, hls, or ws)")
            }
        }
    }

    /// Get the RTMP push URL for streaming *to* the MiBee NVR.
    ///
    /// Use this URL as the RTMP output target in encoders like OBS or FFmpeg:
    /// `rtmp://{nvr-host}:1935/live/{camera_id}`
    pub fn push_stream_rtmp(&self, camera_id: &str) -> String {
        let host = self.extract_host();
        format!("rtmp://{host}:1935/live/{camera_id}")
    }

    // ── Camera sync ──────────────────────────────────────────────────────────

    /// Compute the diff between cameras registered on MiBee and the local set.
    ///
    /// This is a **read-only** merge: it compares MiBee's camera IDs with the
    /// provided `local_cameras` and returns three lists:
    ///
    /// - **`added`**: cameras present on MiBee but absent from the local set
    ///   (candidates for local configuration creation).
    /// - **`removed`**: cameras present locally but absent from MiBee
    ///   (candidates for local configuration removal).
    /// - **`updated`**: cameras present in both sets
    ///   (candidates for local config refresh).
    ///
    /// Actual add/remove/update operations are performed separately via the
    /// CRUD methods.
    pub async fn sync_cameras(&self, local_cameras: &[CameraId]) -> Result<SyncResult> {
        let remote = match self.list_cameras().await {
            Ok(list) => list,
            Err(e) => {
                return Ok(SyncResult {
                    added: vec![],
                    removed: vec![],
                    updated: vec![],
                    errors: vec![format!("failed to list MiBee cameras: {e}")],
                });
            }
        };

        let remote_ids: HashSet<&str> = remote.iter().map(|c| c.id.as_str()).collect();
        let local_set: HashSet<&str> = local_cameras.iter().map(|c| c.as_str()).collect();

        let mut added: Vec<String> = Vec::new();
        let mut updated: Vec<String> = Vec::new();

        for id in &remote_ids {
            if local_set.contains(id) {
                updated.push((*id).to_string());
            } else {
                added.push((*id).to_string());
            }
        }

        let mut removed: Vec<String> = Vec::new();
        for id in &local_set {
            if !remote_ids.contains(id) {
                removed.push((*id).to_string());
            }
        }

        Ok(SyncResult {
            added,
            removed,
            updated,
            errors: vec![],
        })
    }

    // ── Event subscription ───────────────────────────────────────────────────

    /// Subscribe to real-time events from the MiBee NVR via Server-Sent Events.
    ///
    /// Opens a long-lived HTTP connection to `GET /api/events` and parses the
    /// SSE stream, forwarding [`CameraEvent`] values through the returned
    /// [`tokio::sync::mpsc::Receiver`].
    ///
    /// The channel has capacity 256; if the receiver is too slow, older events
    /// are dropped. The background reader task logs errors and exits if the
    /// connection drops.
    pub async fn subscribe_events(&self) -> Result<tokio::sync::mpsc::Receiver<CameraEvent>> {
        let url = self.build_url("/api/events");
        let mut request = self
            .client
            .get(&url)
            .basic_auth(&self.username, Some(&self.password))
            .build()
            .context("failed to build SSE request")?;
        inject_trace_context(request.headers_mut());
        let response = self
            .client
            .execute(request)
            .await
            .with_context(|| format!("SSE connection to {url} failed"))?;

        let status = response.status();
        if !status.is_success() {
            anyhow::bail!("SSE endpoint {url} returned {status}");
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<CameraEvent>(256);

        tokio::spawn(async move {
            if let Err(e) = read_sse_events(response, tx).await {
                tracing::warn!("MiBee SSE reader exited: {e:?}");
            }
        });

        Ok(rx)
    }
}

// ── Trace context injection ─────────────────────────────────────────────────────

/// Injects the current OpenTelemetry trace context (`traceparent` header) into
/// an outbound HTTP request's headers using the globally configured propagator.
///
/// This enables distributed tracing: the receiving service can extract the
/// trace context and continue the same trace across service boundaries.
fn inject_trace_context(headers: &mut reqwest::header::HeaderMap) {
    struct HeaderInjector<'a>(&'a mut reqwest::header::HeaderMap);

    impl Injector for HeaderInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            if let Ok(name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                && let Ok(val) = reqwest::header::HeaderValue::from_str(&value)
            {
                self.0.insert(name, val);
            }
        }
    }

    opentelemetry::global::get_text_map_propagator(|propagator| {
        propagator.inject_context(
            &opentelemetry::Context::current(),
            &mut HeaderInjector(headers),
        )
    });
}

// ── SSE parsing ─────────────────────────────────────────────────────────────────

/// Read an SSE byte stream and forward parsed [`CameraEvent`] values into `tx`.
///
/// SSE format per line:
/// - `event: <type>` — sets the event type
/// - `data: <json>` — appends to the data buffer
/// - (empty line) — dispatches a `CameraEvent` built from the accumulated fields
async fn read_sse_events(
    response: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<CameraEvent>,
) -> Result<()> {
    use futures::StreamExt;

    let mut stream = response.bytes_stream();
    let mut buf = Vec::<u8>::new();
    let mut current_event: Option<String> = None;
    let mut current_data: String = String::new();

    while let Some(chunk_result) = stream.next().await {
        let chunk = chunk_result.context("SSE read error")?;
        buf.extend_from_slice(&chunk);

        // Process complete lines from the buffer.
        #[allow(clippy::while_let_loop)]
        loop {
            // Find the next newline position.
            let nl_pos = match buf.iter().position(|&b| b == b'\n') {
                Some(pos) => pos,
                None => break, // wait for more data
            };

            // Extract the line (excluding the newline character).
            let line: Vec<u8> = buf.drain(..=nl_pos).collect();
            let line = &line[..line.len() - 1]; // strip '\n'
            let line_str = String::from_utf8_lossy(line);

            handle_sse_line(&line_str, &mut current_event, &mut current_data, &tx);

            // If the channel is closed, stop reading.
            if tx.is_closed() {
                return Ok(());
            }
        }
    }

    Ok(())
}

fn handle_sse_line(
    line: &str,
    current_event: &mut Option<String>,
    current_data: &mut String,
    tx: &tokio::sync::mpsc::Sender<CameraEvent>,
) {
    if line.is_empty() {
        // Empty line — dispatch the event if we have data.
        if current_data.is_empty() {
            return;
        }

        let event_type = current_event
            .take()
            .unwrap_or_else(|| "unknown".to_string());
        let data = std::mem::take(current_data);

        match serde_json::from_str::<CameraEvent>(&data) {
            Ok(mut ev) => {
                ev.event_type = event_type;
                let _ = tx.try_send(ev);
            }
            Err(e) => {
                tracing::warn!("Failed to parse SSE data as CameraEvent: {e} — data: {data}");
            }
        }
        return;
    }

    if let Some(event_val) = line.strip_prefix("event: ") {
        *current_event = Some(event_val.trim().to_string());
    } else if let Some(data_val) = line.strip_prefix("data: ") {
        if !current_data.is_empty() {
            current_data.push('\n');
        }
        current_data.push_str(data_val.trim());
    }
    // Lines not matching "event:" or "data:" are ignored per SSE spec.
}

// ── Tests ───────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Path, State},
        http::StatusCode,
        response::IntoResponse,
        routing::get,
    };
    use std::sync::Arc;
    use tokio::sync::Mutex;

    // ── Mock MiBee server ────────────────────────────────────────────────────

    /// Shared state for the mock MiBee NVR server.
    #[derive(Default)]
    struct MockMiBeeState {
        cameras: Vec<MiBeeCamera>,
    }

    type SharedMockState = Arc<Mutex<MockMiBeeState>>;

    /// Start a mock MiBee NVR HTTP server on a random port.
    /// Returns the base URL and a handle to modify mock state.
    async fn start_mock_server() -> (String, SharedMockState) {
        let state: SharedMockState = Arc::new(Mutex::new(MockMiBeeState::default()));

        let app = Router::new()
            .route(
                "/api/cameras",
                get(mock_list_cameras).post(mock_create_camera),
            )
            .route(
                "/api/cameras/{id}",
                get(mock_get_camera)
                    .put(mock_update_camera)
                    .delete(mock_delete_camera),
            )
            .route("/api/events", get(mock_sse_events))
            .with_state(state.clone());

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind mock server");
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{}", addr);

        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        // Give the server a moment to start serving.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        (base_url, state)
    }

    async fn mock_list_cameras(State(state): State<SharedMockState>) -> Json<Vec<MiBeeCamera>> {
        let guard = state.lock().await;
        Json(guard.cameras.clone())
    }

    async fn mock_get_camera(
        State(state): State<SharedMockState>,
        Path(id): Path<String>,
    ) -> Result<Json<MiBeeCamera>, StatusCode> {
        let guard = state.lock().await;
        guard
            .cameras
            .iter()
            .find(|c| c.id == id)
            .map(|c| Json(c.clone()))
            .ok_or(StatusCode::NOT_FOUND)
    }

    async fn mock_create_camera(
        State(state): State<SharedMockState>,
        Json(req): Json<CreateCameraRequest>,
    ) -> (StatusCode, Json<MiBeeCamera>) {
        let mut guard = state.lock().await;
        let id = format!("cam_{}", guard.cameras.len() + 1);
        let camera = MiBeeCamera {
            id: id.clone(),
            name: req.name,
            source: req.source,
            enabled: req.enabled.unwrap_or(true),
            stream_type: req.stream_type.unwrap_or_else(|| "rtsp".to_string()),
            created_at: Some("2026-06-09T00:00:00Z".to_string()),
            updated_at: Some("2026-06-09T00:00:00Z".to_string()),
        };
        guard.cameras.push(camera.clone());
        (StatusCode::CREATED, Json(camera))
    }

    async fn mock_update_camera(
        State(state): State<SharedMockState>,
        Path(id): Path<String>,
        Json(req): Json<UpdateCameraRequest>,
    ) -> Result<Json<MiBeeCamera>, StatusCode> {
        let mut guard = state.lock().await;
        let camera = guard
            .cameras
            .iter_mut()
            .find(|c| c.id == id)
            .ok_or(StatusCode::NOT_FOUND)?;

        if let Some(name) = req.name {
            camera.name = name;
        }
        if let Some(source) = req.source {
            camera.source = source;
        }
        if let Some(enabled) = req.enabled {
            camera.enabled = enabled;
        }
        camera.updated_at = Some("2026-06-09T01:00:00Z".to_string());

        Ok(Json(camera.clone()))
    }

    async fn mock_delete_camera(
        State(state): State<SharedMockState>,
        Path(id): Path<String>,
    ) -> StatusCode {
        let mut guard = state.lock().await;
        let pos = guard.cameras.iter().position(|c| c.id == id);
        match pos {
            Some(idx) => {
                guard.cameras.remove(idx);
                StatusCode::NO_CONTENT
            }
            None => StatusCode::NOT_FOUND,
        }
    }

    async fn mock_sse_events() -> impl IntoResponse {
        let body = "event: camera_added\ndata: {\"type\":\"x\",\"cameraId\":\"cam_1\",\"timestamp\":1,\"data\":null}\n\nevent: motion_detected\ndata: {\"type\":\"x\",\"cameraId\":\"cam_2\",\"timestamp\":2,\"data\":{\"zone\":\"entrance\"}}\n\n";
        (
            StatusCode::OK,
            [("Content-Type", "text/event-stream")],
            body,
        )
    }

    // ── Client creation tests ────────────────────────────────────────────────

    #[test]
    fn test_new_client() {
        let client = MiBeeClient::new("http://localhost:8080", "admin", "secret");
        assert_eq!(client.base_url, "http://localhost:8080");
        assert_eq!(client.username, "admin");
        assert_eq!(client.password, "secret");
    }

    #[test]
    fn test_new_client_strips_trailing_slash() {
        let client = MiBeeClient::new("http://localhost:8080/", "u", "p");
        assert_eq!(client.base_url, "http://localhost:8080");
    }

    #[test]
    fn test_new_client_https() {
        let client = MiBeeClient::new("https://nvr.example.com", "u", "p");
        assert_eq!(client.base_url, "https://nvr.example.com");
    }

    // ── URL construction tests ───────────────────────────────────────────────

    #[test]
    fn test_build_url() {
        let client = MiBeeClient::new("http://localhost:8080", "u", "p");
        assert_eq!(
            client.build_url("/api/cameras"),
            "http://localhost:8080/api/cameras"
        );
        assert_eq!(
            client.build_url("/api/cameras/123"),
            "http://localhost:8080/api/cameras/123"
        );
    }

    #[test]
    fn test_extract_host() {
        let client = MiBeeClient::new("http://192.168.1.100:8080", "u", "p");
        assert_eq!(client.extract_host(), "192.168.1.100");
    }

    #[test]
    fn test_extract_host_localhost() {
        let client = MiBeeClient::new("http://localhost:8080", "u", "p");
        assert_eq!(client.extract_host(), "localhost");
    }

    #[test]
    fn test_extract_host_https() {
        let client = MiBeeClient::new("https://nvr.example.com:8443", "u", "p");
        assert_eq!(client.extract_host(), "nvr.example.com");
    }

    // ── Stream URL construction tests ────────────────────────────────────────

    #[test]
    fn test_get_stream_url_rtsp() {
        let client = MiBeeClient::new("http://192.168.1.100:8080", "u", "p");
        let url = client.get_stream_url("cam_1", "rtsp").unwrap();
        assert_eq!(url, "rtsp://192.168.1.100:8554/live/cam_1");
    }

    #[test]
    fn test_get_stream_url_hls() {
        let client = MiBeeClient::new("http://192.168.1.100:8080", "u", "p");
        let url = client.get_stream_url("cam_1", "hls").unwrap();
        assert_eq!(url, "http://192.168.1.100:8080/hls/cam_1/index.m3u8");
    }

    #[test]
    fn test_get_stream_url_websocket() {
        let client = MiBeeClient::new("http://192.168.1.100:8080", "u", "p");
        let url = client.get_stream_url("cam_1", "ws").unwrap();
        assert_eq!(url, "ws://192.168.1.100:8080/ws/cam_1");
    }

    #[test]
    fn test_get_stream_url_websocket_full_name() {
        let client = MiBeeClient::new("http://192.168.1.100:8080", "u", "p");
        let url = client.get_stream_url("cam_1", "websocket").unwrap();
        assert_eq!(url, "ws://192.168.1.100:8080/ws/cam_1");
    }

    #[test]
    fn test_get_stream_url_unsupported_protocol() {
        let client = MiBeeClient::new("http://localhost:8080", "u", "p");
        let result = client.get_stream_url("cam_1", "webrtc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("unsupported"));
    }

    #[test]
    fn test_push_stream_rtmp() {
        let client = MiBeeClient::new("http://192.168.1.100:8080", "u", "p");
        let url = client.push_stream_rtmp("cam_1");
        assert_eq!(url, "rtmp://192.168.1.100:1935/live/cam_1");
    }

    // ── Sync logic tests ─────────────────────────────────────────────────────

    #[test]
    fn test_sync_result_construction() {
        let result = SyncResult {
            added: vec!["cam_1".to_string()],
            removed: vec!["cam_2".to_string()],
            updated: vec![],
            errors: vec![],
        };
        assert_eq!(result.added, vec!["cam_1"]);
        assert_eq!(result.removed, vec!["cam_2"]);
        assert!(result.updated.is_empty());
        assert!(result.errors.is_empty());
    }

    // ── Camera CRUD tests (against mock HTTP server) ─────────────────────────

    async fn seed_camera(state: &SharedMockState, id: &str, name: &str, source: &str) {
        let mut guard = state.lock().await;
        guard.cameras.push(MiBeeCamera {
            id: id.to_string(),
            name: name.to_string(),
            source: source.to_string(),
            enabled: true,
            stream_type: "rtsp".to_string(),
            created_at: Some("2026-06-09T00:00:00Z".to_string()),
            updated_at: Some("2026-06-09T00:00:00Z".to_string()),
        });
    }

    #[tokio::test]
    async fn test_list_cameras_empty() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");
        let cameras = client.list_cameras().await.unwrap();
        assert!(cameras.is_empty());
    }

    #[tokio::test]
    async fn test_list_cameras_with_data() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;
        seed_camera(
            &state,
            "cam_2",
            "Back Yard",
            "rtsp://192.168.1.11:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");
        let cameras = client.list_cameras().await.unwrap();
        assert_eq!(cameras.len(), 2);
        assert_eq!(cameras[0].id, "cam_1");
        assert_eq!(cameras[0].name, "Front Door");
        assert_eq!(cameras[1].id, "cam_2");
    }

    #[tokio::test]
    async fn test_get_camera() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");
        let camera = client.get_camera("cam_1").await.unwrap();
        assert_eq!(camera.id, "cam_1");
        assert_eq!(camera.name, "Front Door");
    }

    #[tokio::test]
    async fn test_get_camera_not_found() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");
        let result = client.get_camera("nonexistent").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_create_camera() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");

        let req = CreateCameraRequest {
            name: "New Camera".to_string(),
            source: "rtsp://192.168.1.20:554/stream1".to_string(),
            stream_type: Some("rtsp".to_string()),
            enabled: Some(true),
        };

        let camera = client.create_camera(&req).await.unwrap();
        assert_eq!(camera.name, "New Camera");
        assert_eq!(camera.source, "rtsp://192.168.1.20:554/stream1");

        // Verify it was actually added.
        let cameras = client.list_cameras().await.unwrap();
        assert_eq!(cameras.len(), 1);
    }

    #[tokio::test]
    async fn test_create_camera_defaults() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");

        let req = CreateCameraRequest {
            name: "Minimal Camera".to_string(),
            source: "rtsp://192.168.1.30:554/stream1".to_string(),
            stream_type: None,
            enabled: None,
        };

        let camera = client.create_camera(&req).await.unwrap();
        assert_eq!(camera.name, "Minimal Camera");
        assert!(camera.enabled);
        assert_eq!(camera.stream_type, "rtsp");
    }

    #[tokio::test]
    async fn test_update_camera() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");

        let req = UpdateCameraRequest {
            name: Some("Front Door Updated".to_string()),
            source: None,
            enabled: Some(false),
        };

        let camera = client.update_camera("cam_1", &req).await.unwrap();
        assert_eq!(camera.name, "Front Door Updated");
        assert!(!camera.enabled);
        assert_eq!(camera.source, "rtsp://192.168.1.10:554/stream1"); // unchanged
    }

    #[tokio::test]
    async fn test_update_camera_not_found() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");

        let req = UpdateCameraRequest {
            name: Some("Ghost".to_string()),
            source: None,
            enabled: None,
        };

        let result = client.update_camera("nonexistent", &req).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_delete_camera() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");
        client.delete_camera("cam_1").await.unwrap();

        let cameras = client.list_cameras().await.unwrap();
        assert!(cameras.is_empty());
    }

    #[tokio::test]
    async fn test_delete_camera_not_found() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");
        let result = client.delete_camera("nonexistent").await;
        assert!(result.is_err());
    }

    // ── Sync tests ───────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_sync_cameras_empty_both() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");

        let result = client.sync_cameras(&[]).await.unwrap();
        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert!(result.updated.is_empty());
        assert!(result.errors.is_empty());
    }

    #[tokio::test]
    async fn test_sync_cameras_all_added() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;
        seed_camera(
            &state,
            "cam_2",
            "Back Yard",
            "rtsp://192.168.1.11:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");
        let result = client.sync_cameras(&[]).await.unwrap();

        assert_eq!(result.added.len(), 2);
        assert!(result.added.contains(&"cam_1".to_string()));
        assert!(result.added.contains(&"cam_2".to_string()));
        assert!(result.removed.is_empty());
        assert!(result.updated.is_empty());
    }

    #[tokio::test]
    async fn test_sync_cameras_all_removed() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");
        let result = client
            .sync_cameras(&["cam_1".to_string(), "cam_2".to_string()])
            .await
            .unwrap();

        // cam_1: in both sets → updated
        // cam_2: local only → removed
        assert!(result.added.is_empty());
        assert!(result.updated.contains(&"cam_1".to_string()));
        assert_eq!(result.removed, vec!["cam_2"]);
    }

    #[tokio::test]
    async fn test_sync_cameras_mixed() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;
        seed_camera(
            &state,
            "cam_2",
            "Back Yard",
            "rtsp://192.168.1.11:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");
        let result = client
            .sync_cameras(&["cam_2".to_string(), "cam_3".to_string()])
            .await
            .unwrap();

        // cam_1: MiBee only → added
        // cam_2: both → updated
        // cam_3: local only → removed
        assert_eq!(result.added, vec!["cam_1"]);
        assert!(result.updated.contains(&"cam_2".to_string()));
        assert!(result.removed.contains(&"cam_3".to_string()));
    }

    #[tokio::test]
    async fn test_sync_cameras_all_updated() {
        let (base_url, state) = start_mock_server().await;
        seed_camera(
            &state,
            "cam_1",
            "Front Door",
            "rtsp://192.168.1.10:554/stream1",
        )
        .await;
        seed_camera(
            &state,
            "cam_2",
            "Back Yard",
            "rtsp://192.168.1.11:554/stream1",
        )
        .await;

        let client = MiBeeClient::new(&base_url, "test", "test");
        let result = client
            .sync_cameras(&["cam_1".to_string(), "cam_2".to_string()])
            .await
            .unwrap();

        assert!(result.added.is_empty());
        assert!(result.removed.is_empty());
        assert_eq!(result.updated.len(), 2);
        assert!(result.updated.contains(&"cam_1".to_string()));
        assert!(result.updated.contains(&"cam_2".to_string()));
    }

    #[tokio::test]
    async fn test_sync_cameras_server_error() {
        let (_base_url, _state) = start_mock_server().await;

        // Use an invalid URL to simulate server error.
        let bad_client = MiBeeClient::new("http://127.0.0.1:1", "test", "test");
        let result = bad_client.sync_cameras(&[]).await.unwrap();
        assert!(!result.errors.is_empty());
    }

    // ── Auth header tests ───────────────────────────────────────────────────

    #[test]
    fn test_client_credentials_stored() {
        let client = MiBeeClient::new("http://localhost:8080", "admin", "hunter2");
        assert_eq!(client.username, "admin");
        assert_eq!(client.password, "hunter2");
    }

    // ── SSE parsing tests ────────────────────────────────────────────────────

    #[test]
    fn test_handle_sse_line_unknown_event_type() {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let mut event_type: Option<String> = None;
        let mut data = String::new();

        // Without "event:" prefix, event_type defaults to "unknown"
        handle_sse_line(
            "data: {\"cameraId\":\"cam_1\",\"timestamp\":1,\"data\":null}",
            &mut event_type,
            &mut data,
            &tx,
        );
        assert_eq!(event_type, None);
        assert_eq!(
            data,
            "{\"cameraId\":\"cam_1\",\"timestamp\":1,\"data\":null}"
        );
    }

    #[test]
    fn test_handle_sse_line_empty_line_dispatches_event() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut event_type: Option<String> = Some("camera_added".to_string());
        // JSON needs type field for CameraEvent deserialization (will be overwritten)
        let mut data =
            "{\"type\":\"x\",\"cameraId\":\"cam_1\",\"timestamp\":1,\"data\":null}".to_string();

        // Empty line dispatches the event
        handle_sse_line("", &mut event_type, &mut data, &tx);
        assert_eq!(event_type, None);
        assert!(data.is_empty());

        let event = rx.try_recv().unwrap();
        assert_eq!(event.event_type, "camera_added");
        assert_eq!(event.camera_id, "cam_1");
        assert_eq!(event.timestamp, 1);
    }

    #[test]
    fn test_handle_sse_line_event_prefix() {
        let (tx, _rx) = tokio::sync::mpsc::channel(16);
        let mut event_type: Option<String> = None;
        let mut data = String::new();

        handle_sse_line("event: motion_detected", &mut event_type, &mut data, &tx);
        assert_eq!(event_type, Some("motion_detected".to_string()));
    }

    #[test]
    fn test_handle_sse_line_multiple_data_lines() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut event_type: Option<String> = None;
        let mut data = String::new();

        // Two data lines separated by newline
        handle_sse_line("data: line1", &mut event_type, &mut data, &tx);
        handle_sse_line("data: line2", &mut event_type, &mut data, &tx);
        assert_eq!(data, "line1\nline2");

        // Set valid CameraEvent data and dispatch
        event_type = Some("test".to_string());
        data = "{\"type\":\"x\",\"cameraId\":\"cam_1\",\"timestamp\":1,\"data\":null}".to_string();

        // Dispatch
        handle_sse_line("", &mut event_type, &mut data, &tx);
        let event = rx.try_recv().unwrap();
        assert_eq!(event.event_type, "test");
    }

    #[test]
    fn test_handle_sse_line_invalid_json_skipped() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut event_type: Option<String> = Some("bad".to_string());
        let mut data = "{invalid json}".to_string();

        // Dispatch with invalid JSON — silently skipped, no event sent
        handle_sse_line("", &mut event_type, &mut data, &tx);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_subscribe_events_via_mock_server() {
        let (base_url, _state) = start_mock_server().await;
        let client = MiBeeClient::new(&base_url, "test", "test");

        // Subscribe to events
        let mut rx = client.subscribe_events().await.unwrap();

        // Read first event
        let event1 = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout waiting for event1")
            .expect("channel closed");
        assert_eq!(event1.event_type, "camera_added");
        assert_eq!(event1.camera_id, "cam_1");
        assert_eq!(event1.timestamp, 1);

        // Read second event
        let event2 = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("timeout waiting for event2")
            .expect("channel closed");
        assert_eq!(event2.event_type, "motion_detected");
        assert_eq!(event2.camera_id, "cam_2");
        assert_eq!(event2.timestamp, 2);
        assert_eq!(event2.data, Some(serde_json::json!({"zone": "entrance"})));
    }

    // ── Serialization tests ──────────────────────────────────────────────────

    #[test]
    fn test_create_camera_request_serialization() {
        let req = CreateCameraRequest {
            name: "Test Cam".to_string(),
            source: "rtsp://localhost/stream".to_string(),
            stream_type: Some("rtsp".to_string()),
            enabled: Some(true),
        };
        let json = serde_json::to_value(&req).unwrap();
        assert_eq!(json["name"], "Test Cam");
        assert_eq!(json["source"], "rtsp://localhost/stream");
        assert_eq!(json["streamType"], "rtsp");
        assert_eq!(json["enabled"], true);
    }

    #[test]
    fn test_mibee_camera_deserialization() {
        let json = serde_json::json!({
            "id": "cam_1",
            "name": "Front Door",
            "source": "rtsp://192.168.1.10:554/stream1",
            "enabled": true,
            "streamType": "rtsp",
            "createdAt": "2026-06-09T00:00:00Z",
            "updatedAt": "2026-06-09T00:00:00Z"
        });
        let camera: MiBeeCamera = serde_json::from_value(json).unwrap();
        assert_eq!(camera.id, "cam_1");
        assert_eq!(camera.name, "Front Door");
        assert!(camera.enabled);
        assert_eq!(camera.stream_type, "rtsp");
    }

    #[test]
    fn test_camera_event_serialization() {
        let event = CameraEvent {
            event_type: "motion_detected".to_string(),
            camera_id: "cam_1".to_string(),
            timestamp: 1712345678000,
            data: Some(serde_json::json!({"confidence": 0.95})),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "motion_detected");
        assert_eq!(json["cameraId"], "cam_1");
        assert_eq!(json["timestamp"], serde_json::json!(1712345678000i64));
        assert_eq!(json["data"]["confidence"], 0.95);
    }
}
