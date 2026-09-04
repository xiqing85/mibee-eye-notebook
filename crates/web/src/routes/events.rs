//! Server-Sent Events (SSE) endpoint for real-time camera events.
//!
//! `GET /api/events` opens a persistent SSE connection that streams
//! camera hot-plug events (`camera_added`, `camera_offline`) to the
//! browser. The browser uses this to refresh the camera list without
//! polling.
//!
//! # Event format
//!
//! Each SSE event is a UTF-8 text block terminated by `\n\n`:
//!
//! ```text
//! event: camera_added
//! data: {"camera_id":"...","device_index":0,"name":"..."}
//!
//! event: camera_offline
//! data: {"camera_id":"...","device_index":0}
//! ```

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Extension;
use axum::http::HeaderMap;
use axum::response::sse::{Event, KeepAlive, Sse};
use futures::stream::Stream;
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::BroadcastStream;

use crate::errors::ApiError;
use security::middleware::AuthenticatedUser;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Events broadcast to SSE clients when camera state changes.
///
/// These are produced by the hot-plug monitor (via main.rs) and consumed
/// by the SSE endpoint.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum CameraEvent {
    /// A new camera was discovered (plugged in or detected on startup).
    CameraAdded {
        camera_id: String,
        device_index: u32,
        name: String,
    },
    /// A previously-known camera went offline (unplugged).
    CameraOfflined {
        camera_id: String,
        device_index: u32,
    },
    /// An AI inference completed on a camera (SPEC v1 §6 `ai_detection`).
    /// Produced by the AI engine's per-camera workers and bridged into
    /// this bus by main.rs.
    AiDetection {
        camera_id: String,
        detections: Vec<streaming::ai::Detection>,
        frame_number: u64,
    },
}

/// Type alias for the broadcast sender used to fan out camera events.
pub type EventBus = broadcast::Sender<CameraEvent>;

/// Create a new event bus with a reasonable buffer size.
pub fn new_event_bus() -> EventBus {
    broadcast::channel::<CameraEvent>(64).0
}

// ---------------------------------------------------------------------------
// SSE handler
// ---------------------------------------------------------------------------

/// `GET /api/events` — Server-Sent Events stream for camera hot-plug events.
///
/// Returns a persistent SSE connection. The browser receives
/// `camera_added` / `camera_offlined` events as they happen.
///
/// Authenticated via session cookie (same as all other API endpoints).
#[tracing::instrument(skip_all)]
pub async fn sse_events(
    Extension(event_tx): Extension<Arc<EventBus>>,
    Extension(_user): Extension<AuthenticatedUser>,
) -> std::result::Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, ApiError> {
    // Subscribe to the broadcast channel.
    let rx = event_tx.subscribe();

    // Convert the broadcast receiver into an async stream.
    let event_stream = BroadcastStream::new(rx).filter_map(|result| {
        match result {
            Ok(event) => Some(Ok(event_to_sse(event))),
            // Lagged errors are expected when events arrive faster than
            // the client can consume; just skip them.
            Err(_lagged) => None,
        }
    });

    // Wrap with keep-alive pings every 15s.
    Ok(Sse::new(event_stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    ))
}

/// Convert a [`CameraEvent`] into an Axum SSE [`Event`].
fn event_to_sse(event: CameraEvent) -> Event {
    match &event {
        CameraEvent::CameraAdded {
            camera_id,
            device_index,
            name,
        } => Event::default().event("camera_added").data(
            serde_json::json!({
                "camera_id": camera_id,
                "device_index": device_index,
                "name": name,
            })
            .to_string(),
        ),
        CameraEvent::CameraOfflined {
            camera_id,
            device_index,
        } => Event::default().event("camera_offlined").data(
            serde_json::json!({
                "camera_id": camera_id,
                "device_index": device_index,
            })
            .to_string(),
        ),
        CameraEvent::AiDetection {
            camera_id,
            detections,
            frame_number,
        } => Event::default().event("ai_detection").data(
            serde_json::json!({
                "camera_id": camera_id,
                "detections": detections,
                "frame_number": frame_number,
            })
            .to_string(),
        ),
    }
}

/// Returns the SSE-friendly headers for `text/event-stream`.
///
/// Currently unused directly (Axum's `Sse` handles this), but documented
/// here for reference.
#[allow(dead_code)]
pub fn sse_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/event-stream".parse().unwrap());
    headers.insert("cache-control", "no-cache".parse().unwrap());
    headers.insert("connection", "keep-alive".parse().unwrap());
    headers
}
